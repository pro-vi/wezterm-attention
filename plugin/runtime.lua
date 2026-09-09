return function()
  local attention_cache = {}
  local legacy_cache_key_by_marker_id = {}
  local marker_id_by_local = {}
  local seen_marker_ids_by_window = {}

  local function bind(context)
    local M = context.M
    local defaults = context.defaults
    local wezterm = context.wezterm
    local protocol = context.protocol
    local diagnostic = context.diagnostic
    local plugin_root = context.plugin_root
    local report_error_once = context.report_error_once
    local resolve_pane_read = context.resolve_pane_read
    local read_attention_view = context.read_attention_view
    local pane_method = context.pane_method
    local selected_v2_records_root = context.selected_v2_records_root
    local v2_review_paths = context.v2_review_paths
    local write_v2_user_review = context.write_v2_user_review
    local clear_v2_reviews = context.clear_v2_reviews
    local refresh_cached_v2 = context.refresh_cached_v2
    local acknowledge_focused_v2_pane = context.acknowledge_focused_v2_pane
    local read_effective_marker = context.read_effective_marker
    local read_marker = context.read_marker
    local marker_identity = context.marker_identity
    local clear_acknowledgement = context.clear_acknowledgement
    local write_acknowledgement = context.write_acknowledgement
    local count_live_subagents = context.count_live_subagents
    local review_flagged = context.review_flagged
    local acknowledgement_matches = context.acknowledgement_matches
    local remove_expired_marker = context.remove_expired_marker
    local remove_marker = context.remove_marker
    local clear_review_flag = context.clear_review_flag
    local write_review_flag = context.write_review_flag
    local now_ms = context.now_ms
    local frame_for_now = context.frame_for_now
    local stale_ttl_ms = context.stale_ttl_ms
    local format_unix_ns20 = context.format_unix_ns20
    local wezterm_now_unix_ns20 = context.wezterm_now_unix_ns20
    local seconds_until_after = context.seconds_until_after
    local sample_settled_title = context.sample_settled_title
    local settled_title_state = context.settled_title_state
    local gui_tab_pane_ids = context.gui_tab_pane_ids
    local resolve_visible_attention = context.resolve_visible_attention

    local function same_cached_attention(a, b)
      if not a or not b then return a == b end
      return a.type == b.type
        and a.frame == b.frame
        and a.activity_type == b.activity_type
        and a.event_id == b.event_id
        and a.source == b.source
        and a.provider == b.provider
        and (a.puppet == true) == (b.puppet == true)
        and (a.subagents or 0) == (b.subagents or 0)
        and (a.review == true) == (b.review == true)
        and a.binding_phase == b.binding_phase
        and a.pane_presence == b.pane_presence
        and a.reader_confidence == b.reader_confidence
        and a.binding_health == b.binding_health
        and a.base_title == b.base_title
        and a.settled_title == b.settled_title
    end

    --- Return the panes of the tab holding marker_id in a captured tab list.
    local function tab_panes_containing(tabs, marker_id)
      if not marker_id then return nil end
      for _, tab in ipairs(tabs) do
        local panes = tab:panes()
        for _, p in ipairs(panes) do
          if M.pane_marker_id(p) == marker_id then
            return panes
          end
        end
      end
      return nil
    end

    local function tab_panes_containing_read(tabs, target)
      if not target then return nil end
      for _, tab in ipairs(tabs) do
        local panes = tab:panes()
        for _, pane in ipairs(panes) do
          local read = resolve_pane_read(pane)
          if read.kind == target.kind
              and ((read.kind == "v2" and read.cache_key == target.cache_key)
                or (read.kind == "v1" and read.marker_id == target.marker_id)) then
            return panes
          end
        end
      end
      return nil
    end

    --- Acknowledge the one pane the user is actually looking at: suppress its
    --- effective attention if disk still says its type is one they configured to acknowledge on
    --- sight (stop, notify by default). Canonical writer truth is never moved or
    --- removed; the acknowledged marker identity is written to a sidecar instead.
    ---
    --- The caller must already have established that the GUI window has keyboard
    --- focus and that this is its active pane. Both conditions matter: a marker
    --- acknowledged while its window is in the background is a notification the user
    --- never saw.
    ---
    --- Does the user's review flag outrank what the marker file says? Absent and
    --- acknowledged markers (both arrive here as a nil type) are outranked by
    --- anything, and a marker the configured priority order puts below `review`
    --- — `thinking` by default — is too. `stop` and `notify` outrank it, so a
    --- flagged pane that finishes still shows its ✓ first and falls back to the ◆
    --- once that ✓ has been acknowledged.
    local function review_outranks(atype)
      if not atype then return true end
      local priority = M._active_priority_map or {}
      return (priority[atype] or 0) < (priority.review or 0)
    end

    local function cache_marker_values(
        id, atype, frame, raw, publication_id, observed_now, source, puppet, subagents, flagged)
      subagents = tonumber(subagents) or 0
      -- A marker file whose own type is "review" was written by an older Alt+B,
      -- before the flag moved to its own file. It is the same user flag.
      local review = flagged == true or atype == "review"

      local effective = atype
      if review and review_outranks(atype) then
        effective = "review"
        frame = nil
      end

      if not effective then
        -- Nothing to show. A pane whose subagents are still working keeps a
        -- count-only entry so its tab can render "+N"; with nothing left to say,
        -- the entry goes away entirely, as it always has.
        if subagents > 0 then
          attention_cache[id] = {
            type        = nil,
            observed_at = observed_now,
            puppet      = false,
            subagents   = subagents,
            review      = review,
          }
        else
          attention_cache[id] = nil
        end
        return
      end

      if effective == "thinking" and frame == nil then
        local cfg_indicators = M._active_indicators or defaults.indicators
        local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
        frame = frame_for_now(observed_now, #frames)
      end

      -- `raw`, `identity`, `source` and `puppet` always describe the marker file,
      -- even when the review flag has taken over `type`. The marker is still the
      -- thing acknowledgement compares against and TTL ages out; the flag only
      -- decides what the tab shows.
      attention_cache[id] = {
        type        = effective,
        frame       = frame,
        observed_at = observed_now,
        raw         = raw,
        identity    = marker_identity(raw, publication_id),
        source      = source,
        puppet      = puppet == true,
        subagents   = subagents,
        review      = review,
      }
    end

    --- Re-read one pane's files and rebuild its cache entry. The Alt+B handler
    --- changes a pane's state outside the poll loop, and dropping the entry instead
    --- would blank a sibling's ✓ or a "+N" until the next tick.
    local function refresh_cached_pane(dir, id, now)
      local atype, frame, _, _, raw, publication_id, source, puppet =
        read_effective_marker(dir, id)
      cache_marker_values(
        id, atype, frame, raw, publication_id, now, source, puppet,
        count_live_subagents(dir, id, now), review_flagged(dir, id))
    end

    local function acknowledge_focused_pane(pane_id, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
      local observed_now = (opts and opts.now_ms) or now_ms()

      local id = tostring(pane_id)
      local cached = attention_cache[id]
      if not (cached and acknowledge_set[cached.type]) then return "absent" end

      -- The count this tick's poll already read from disk. Acknowledgement is about
      -- the marker only, so it must hand the count back unchanged; recomputing zero
      -- here would drop the "+N" and make every tick a visible change.
      local subagents = cached.subagents or 0
      -- The user's review flag survives acknowledgement, and every cache write
      -- below has to carry it: a flagged pane whose ✓ the user just looked at falls
      -- back to showing the ◆, it does not go quiet.
      local flagged = cached.review == true

      local current_type, current_frame, _, _, raw, publication_id, source, puppet =
        read_marker(dir, id)
      if not current_type then
        clear_acknowledgement(dir, id)
        cache_marker_values(id, nil, nil, nil, nil, observed_now, nil, false, subagents, flagged)
        return "absent"
      end

      local current_identity = marker_identity(raw, publication_id)
      if cached.identity ~= current_identity then
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
          subagents, flagged)
        return "kept"
      end

      if not acknowledge_set[current_type] then
        clear_acknowledgement(dir, id)
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
          subagents, flagged)
        return "kept"
      end

      local write_ack = (opts and opts.write_acknowledgement) or write_acknowledgement
      if not write_ack(dir, id, current_identity) then
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
          subagents, flagged)
        return "failed"
      end

      -- A writer may replace or clear the marker while the sidecar is being
      -- written. Re-read effective truth before updating the cache: only the exact
      -- identity that was viewed is suppressed.
      local effective_type, effective_frame, _, _, effective_raw, effective_publication_id,
        effective_source, effective_puppet = read_effective_marker(dir, id)
      cache_marker_values(
        id, effective_type, effective_frame, effective_raw, effective_publication_id, observed_now,
        effective_source, effective_puppet, subagents, flagged)
      return effective_type and "kept" or "acknowledged"
    end

    -- ── Focus and redraw ────────────────────────────────────────────────────────

    local redraw_disabled = {}

    local function redraw_window_key(window)
      return tostring(window:window_id())
    end

    --- Ask WezTerm to rebuild every tab title without changing tab selection or
    --- user-owned title and status values.
    ---
    --- ActivateTabRelative(0) re-activates the tab that is already active. WezTerm
    --- answers by recomputing the tab bar, which is the whole point; the active
    --- tab, active pane, status strings, mux window title, and user-owned title
    --- values remain intact while attention decoration updates. This is not a
    --- dedicated invalidation API, so it lives behind this one function: if
    --- WezTerm ever ships a real "redraw the tab bar" call, only this body changes.
    ---
    --- The caller must have established that the window is focused. Performing a
    --- key action against a background window can emit terminal focus events, so
    --- that guard is correctness, not economy.
    --- A host that renders tab titles itself (renderer = "manual", drawing on its
    --- own update-status tick) already repaints without being asked, so
    --- request_redraw = false turns this off and saves the tab-activation pass the
    --- action costs.
    local function request_tab_bar_redraw(window, pane)
      if M._active_request_redraw == false then return false end
      if not pane then return false end

      local window_key = redraw_window_key(window)
      if redraw_disabled[window_key] then return false end

      local ok, err = pcall(
        window.perform_action, window, wezterm.action.ActivateTabRelative(0), pane)
      if not ok then
        redraw_disabled[window_key] = true
        wezterm.log_error("wezterm-attention: tab bar redraw failed: " .. tostring(err))
        return false
      end
      return true
    end

    local ttl_wakeup_by_window = {}
    local publish_schedule_by_realm = {}
    local publish_domains_by_window = {}
    local publish_backoff_seconds = { 2, 5, 10, 30 }

    local function spawn_republish(domain, socket, root)
      if type(wezterm.background_child_process) ~= "function" then
        report_error_once("publish-spawn:" .. socket,
          "cannot republish mux identity: background_child_process is unavailable")
        return false
      end
      local argv = {
        "env", "WEZTERM_ATTENTION_DIR=" .. M._active_dir,
      }
      local executable_dir = wezterm.executable_dir
      if type(executable_dir) == "string" and executable_dir ~= "" then
        local inherited_path = os.getenv("PATH")
        argv[#argv + 1] = "PATH=" .. executable_dir
          .. (inherited_path and inherited_path ~= "" and (":" .. inherited_path) or "")
      end
      argv[#argv + 1] = root .. "/bin/attention"
      argv[#argv + 1] = "hooks"
      argv[#argv + 1] = "publish"
      argv[#argv + 1] = "--realm"
      argv[#argv + 1] = socket
      argv[#argv + 1] = "--quiet"
      local ok, started = pcall(wezterm.background_child_process, argv)
      if not ok or started == false then
        report_error_once("publish-spawn:" .. socket,
          "failed to start mux identity publication for " .. domain)
        return false
      end
      return true
    end

    local function publish_call_after(opts)
      return (opts and opts.call_after) or (wezterm.time and wezterm.time.call_after)
    end

    local schedule_publish_retry

    local function retain_publish_schedule(schedule)
      for _, observation in pairs(schedule.window_observations) do
        if observation.unpublished then
          schedule.unpublished = true
          return true
        end
      end
      schedule.unpublished = false
      schedule.token = schedule.token + 1
      publish_schedule_by_realm[schedule.socket] = nil
      return false
    end

    local function retire_publish_observation(domain, window_key)
      for _, schedule in pairs(publish_schedule_by_realm) do
        if schedule.domain == domain and schedule.window_observations[window_key] then
          schedule.window_observations[window_key] = nil
          retain_publish_schedule(schedule)
        end
      end
    end

    local function gui_window_keys(opts)
      local inventory = opts and opts.gui_windows
      if inventory == nil and wezterm.gui then inventory = wezterm.gui.gui_windows end
      if type(inventory) == "function" then
        local ok, windows = pcall(inventory)
        if not ok then return nil end
        inventory = windows
      end
      if type(inventory) ~= "table" then return nil end
      local keys = {}
      for _, gui_window in ipairs(inventory) do
        local ok, id = pcall(gui_window.window_id, gui_window)
        if ok and id ~= nil then keys[tostring(id)] = true end
      end
      return keys
    end

    local function prune_closed_publish_windows(current_window_key, opts)
      local live = gui_window_keys(opts)
      if not live then return end
      -- The callback's window is authoritative even if WezTerm's inventory is
      -- between insertion and publication for a newly created GUI window.
      live[current_window_key] = true
      for window_key in pairs(publish_domains_by_window) do
        if not live[window_key] then publish_domains_by_window[window_key] = nil end
      end
      for _, schedule in pairs(publish_schedule_by_realm) do
        local changed = false
        for window_key in pairs(schedule.window_observations) do
          if not live[window_key] then
            schedule.window_observations[window_key] = nil
            changed = true
          end
        end
        if changed then retain_publish_schedule(schedule) end
      end
    end

    schedule_publish_retry = function(schedule, opts)
      local call_after = publish_call_after(opts)
      if type(call_after) ~= "function" then
        report_error_once("publish-retry:" .. schedule.socket,
          "cannot retry mux identity publication: call_after is unavailable")
        return false
      end
      local index = math.min(schedule.retry_index, #publish_backoff_seconds)
      local delay = publish_backoff_seconds[index]
      schedule.token = schedule.token + 1
      local token = schedule.token
      call_after(delay, function()
        local current = publish_schedule_by_realm[schedule.socket]
        if current ~= schedule or current.token ~= token or not current.unpublished then return end
        spawn_republish(current.domain, current.socket, current.root)
        current.retry_index = current.retry_index + 1
        schedule_publish_retry(current, opts)
      end)
      return true
    end

    local function update_publish_schedule(
        domain, window_key, pane_count, unpublished, opts)
      local socket = M._active_unix_domains and M._active_unix_domains[domain]
      local root = M._active_integration_root
      if not socket or not root then return false end
      local schedule = publish_schedule_by_realm[socket]
      if not schedule then
        if not unpublished then return false end
        publish_schedule_by_realm[socket] = {
          socket = socket,
          domain = domain,
          root = root,
          window_observations = {
            [window_key] = {
              pane_count = pane_count, stable_polls = 1, unpublished = true,
            },
          },
          retry_index = 1,
          token = 0,
          unpublished = true,
          started = false,
        }
        return false
      end
      schedule.unpublished = true
      schedule.domain = domain
      schedule.root = root
      local observation = schedule.window_observations[window_key]
      if not observation then
        schedule.window_observations[window_key] = {
          pane_count = pane_count, stable_polls = 1, unpublished = unpublished,
        }
        return false
      end
      local was_unpublished = observation.unpublished
      observation.unpublished = unpublished
      if not unpublished then
        retain_publish_schedule(schedule)
        return false
      end
      if not was_unpublished then
        observation.pane_count = pane_count
        observation.stable_polls = 1
        return false
      end
      if observation.pane_count ~= pane_count then
        observation.pane_count = pane_count
        observation.stable_polls = 1
        schedule.retry_index = 1
        schedule.started = false
        schedule.token = schedule.token + 1
        return false
      end
      if schedule.started then return false end
      observation.stable_polls = observation.stable_polls + 1
      if observation.stable_polls < 2 then return false end
      schedule.started = true
      spawn_republish(domain, socket, root)
      schedule_publish_retry(schedule, opts)
      return true
    end

    --- Schedule a future poll just after the earliest TTL boundary. The callback
    --- changes no cache entry itself; it only causes records and UTC to be read
    --- again. A later poll with the same boundary does not stack another timer.
    local function schedule_ttl_wakeup(window, boundary, now_unix_ns, opts)
      local window_key = redraw_window_key(window)
      if not boundary or not now_unix_ns then
        ttl_wakeup_by_window[window_key] = nil
        return
      end
      local current = ttl_wakeup_by_window[window_key]
      if current and current.boundary == boundary then return end

      local call_after = (opts and opts.call_after)
        or (wezterm.time and wezterm.time.call_after)
      if type(call_after) ~= "function" then
        report_error_once("ttl-wakeup:" .. window_key,
          "cannot schedule the next v2 TTL read: wezterm.time.call_after is unavailable")
        return
      end

      local delay = seconds_until_after(now_unix_ns, boundary)
      if not delay then return end
      local token = (current and current.token or 0) + 1
      ttl_wakeup_by_window[window_key] = { boundary = boundary, token = token }
      call_after(delay, function()
        local scheduled = ttl_wakeup_by_window[window_key]
        if not scheduled or scheduled.token ~= token then return end
        ttl_wakeup_by_window[window_key] = nil
        M.poll(window, {
          active_pane = opts and opts.active_pane,
          dir = opts and opts.dir,
          glob = opts and opts.glob,
        })
      end)
    end

    -- ── Public API ──────────────────────────────────────────────────────────────

    --- Read the cached attention state for a marker id (see M.pane_marker_id).
    --- Returns (type, frame, source, puppet, subagents, review) or nil. `source` is
    --- the marker's JSON `source` string when it carried one; `puppet` is true only
    --- when the marker set `"puppet": true`; `subagents` is how many of the pane's
    --- subagents ran a tool call in the last ten minutes, 0 when none; `review` is
    --- true when the user has flagged the pane with Alt+B.
    ---
    --- `type` is the effective one: it is `review` when the flag outranks the
    --- marker file, and the marker's own type when that outranks the flag — in
    --- which case the flag is still reported by the sixth return.
    ---
    --- A pane with live subagents and no marker returns (nil, nil, nil, false, n):
    --- the count is real even though there is no marker type to report.
    function M.get_attention(marker_id, opts)
      local id = tostring(marker_id)
      if opts and opts.dir then
        local atype, frame, _, _, _, _, source, puppet = read_effective_marker(opts.dir, id)
        local now = (opts and opts.now_ms) or now_ms()
        local flagged = review_flagged(opts.dir, id) or atype == "review"
        if flagged and review_outranks(atype) then
          atype, frame = "review", nil
        end
        return atype, frame, source, puppet, count_live_subagents(opts.dir, id, now), flagged
      end
      local mapped = legacy_cache_key_by_marker_id[id]
      local cached = mapped and attention_cache[mapped] or nil
      if not cached then cached = attention_cache[id] end
      if cached then
        return cached.type, cached.frame, cached.source, cached.puppet, cached.subagents or 0,
          cached.review == true
      end
      return nil
    end

    --- Return a copy of the cached full-pane v2 view for a pane, or nil when the
    --- pane has no cache entry. This performs no filesystem or process work.
    function M.get_attention_view(pane)
      local read = resolve_pane_read(pane)
      local cached = read.cache_key and attention_cache[read.cache_key] or nil
      if not cached then return nil end
      return {
        provider = cached.provider,
        binding_id = cached.binding_id,
        binding_phase = cached.binding_phase,
        type = cached.type,
        event_id = cached.event_id,
        subagents = cached.subagents or 0,
        review = cached.review == true,
        reader_confidence = cached.reader_confidence,
      }
    end

    --- Remove the attention marker for a marker id (see M.pane_marker_id).
    function M.remove_marker(marker_id, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local id = tostring(marker_id)
      remove_marker(dir, id)
      attention_cache[id] = nil
    end

    --- Inspect GUI-only identity publication. Filesystem, socket, process,
    --- permission, and version probes belong to `attention doctor`.
    function M.doctor(window)
      local diagnostics = {}
      if not window or type(window.mux_window) ~= "function" then
        return { diagnostic("probe_unavailable", "GUI window is unavailable") }
      end
      local ok, mux_win = pcall(window.mux_window, window)
      if not ok or not mux_win or type(mux_win.tabs) ~= "function" then
        return { diagnostic("probe_unavailable", "GUI mux window is unavailable") }
      end
      local tabs_ok, tabs = pcall(mux_win.tabs, mux_win)
      if not tabs_ok or type(tabs) ~= "table" then
        return { diagnostic("probe_unavailable", "GUI panes are unavailable") }
      end
      for _, tab_value in ipairs(tabs) do
        local panes_ok, panes = pcall(tab_value.panes, tab_value)
        if not panes_ok or type(panes) ~= "table" then
          diagnostics[#diagnostics + 1] = diagnostic("probe_unavailable", "GUI tab panes are unavailable")
        else
          for _, pane in ipairs(panes) do
            local read = resolve_pane_read(pane)
            if read.kind == "unpublished" then
              diagnostics[#diagnostics + 1] = diagnostic(
                "identity_unpublished", "mux pane has not published a trustworthy identity",
                { domain = read.domain })
            elseif read.kind == "invalid" then
              diagnostics[#diagnostics + 1] = read.diagnostic
            end
          end
        end
      end
      return diagnostics
    end

    --- Poll marker files, update the cache, acknowledge what the user is looking
    --- at, and ask WezTerm to redraw the tab bar if what it shows has changed.
    --- Call this from your own update-status handler if you set auto_poll = false;
    --- pass the handler's pane as opts.active_pane.
    ---
    --- Only refreshes entries for panes in the current window. Cross-window cache
    --- entries are left alone — pruning them here would cause cache thrash when
    --- multiple windows fire update-status (each window would wipe the other's
    --- entries every tick, producing visible tab-indicator blinking). Stale
    --- thinking markers are removed here by TTL; a closed pane's marker is removed
    --- by the observed-then-gone sweep below.
    ---
    --- The redraw exists because caching alone is not enough: WezTerm calls
    --- format-tab-title when something it knows about changes, and a marker file
    --- appearing on disk is not one of those things. Without the request below, a
    --- background pane's new state sat in the cache, unrendered, until the user
    --- happened to switch tabs — which is exactly when they no longer needed to be
    --- told.
    function M.poll(window, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local mux_win = window:mux_window()
      if not mux_win then return end
      local window_key = redraw_window_key(window)
      prune_closed_publish_windows(window_key, opts)

      local mux_tabs = mux_win:tabs()
      local pane_ids = {}
      local before = {}
      local before_titles = {}
      local seen = {}             -- cache key → identity and domain observed in this window
      local domains_present = {}  -- domain names this window still holds a pane of
      local pane_count_by_domain = {}
      local unpublished_by_domain = {}

      local now = (opts and opts.now_ms) or now_ms()
      local cfg_indicators = M._active_indicators or defaults.indicators
      local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
      local frame_count = #frames
      local utc_sampled = false
      local poll_now_unix_ns
      local poll_utc_error
      local saw_v2 = false
      local earliest_wakeup_unix_ns

      local function sample_utc_once()
        if utc_sampled then return poll_now_unix_ns, poll_utc_error end
        utc_sampled = true
        if opts and type(opts.utc_now) == "function" then
          local ok, value, code = pcall(opts.utc_now)
          if ok then
            poll_now_unix_ns = format_unix_ns20(value)
            if not poll_now_unix_ns then poll_utc_error = code or "probe_unavailable" end
          else
            poll_utc_error = "probe_unavailable"
          end
        elseif opts and opts.now_unix_ns then
          poll_now_unix_ns = format_unix_ns20(opts.now_unix_ns)
          if not poll_now_unix_ns then poll_utc_error = "probe_unavailable" end
        else
          poll_now_unix_ns, poll_utc_error = wezterm_now_unix_ns20()
        end
        return poll_now_unix_ns, poll_utc_error
      end

      for _, tab in ipairs(mux_tabs) do
        for _, p in ipairs(tab:panes()) do
          local domain = pane_method(p, "get_domain_name") or "?"
          domains_present[domain] = true
          pane_count_by_domain[domain] = (pane_count_by_domain[domain] or 0) + 1
          local local_id = tostring(pane_method(p, "pane_id"))
          local read = resolve_pane_read(p)
          local key = read.cache_key
          marker_id_by_local[local_id] = key or false
          if key then
            local prior_title = settled_title_state[key]
            before_titles[key] = prior_title and prior_title.settled or nil
          end

          if read.kind == "invalid" then
            local item = read.diagnostic or invalid("pane identity is invalid")
            report_error_once("v2-identity:" .. local_id .. ":" .. item.code,
              item.code .. ": " .. item.message)
          elseif read.kind == "v2" then
            saw_v2 = true
            local now_unix_ns, utc_error = sample_utc_once()
            if utc_error then
              report_error_once("v2-clock:" .. local_id,
                utc_error .. ": WezTerm UTC is unavailable; TTL-bearing v2 state is omitted")
            end
            seen[key] = { domain = domain, kind = "v2", marker_id = read.marker_id }
            pane_ids[#pane_ids + 1] = key
            before[key] = attention_cache[key]
            local view = read_attention_view(read, now_unix_ns, {
              dir = dir,
              glob = opts and opts.glob,
              previous_view = before[key],
            })
            attention_cache[key] = view
            legacy_cache_key_by_marker_id[read.marker_id] = key
            for _, item in ipairs(view.diagnostics or {}) do
              report_error_once("v2:" .. key .. ":" .. item.code .. ":" .. item.message,
                item.code .. ": " .. item.message)
            end
            local boundary = view.next_wakeup_unix_ns
            if boundary and (not earliest_wakeup_unix_ns or boundary < earliest_wakeup_unix_ns) then
              earliest_wakeup_unix_ns = boundary
            end
          elseif read.kind == "v1" then
            local id = read.marker_id
            legacy_cache_key_by_marker_id[id] = nil
            seen[id] = { domain = domain, kind = "v1", marker_id = id }
            pane_ids[#pane_ids + 1] = id
            before[id] = attention_cache[id]
            local atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet =
              read_marker(dir, id)
            -- One read of each sidecar per pane per tick, with this tick's clock,
            -- whether or not the pane has a marker.
            local subagents = count_live_subagents(dir, id, now)
            local flagged = review_flagged(dir, id)
            local acknowledged = acknowledgement_matches(dir, id, raw, publication_id)
            if atype then
              local cached = attention_cache[id]
              local observed_at = now
              if cached and cached.raw == raw and cached.observed_at then
                observed_at = cached.observed_at
              end

              local effective_updated_at = updated_at or observed_at
              local ttl = stale_ttl_ms(atype, marker_ttl_ms)
              if ttl and now - effective_updated_at > ttl then
                remove_expired_marker(dir, id)
                cache_marker_values(id, nil, nil, nil, nil, now, nil, false, subagents, flagged)
              elseif acknowledged then
                cache_marker_values(
                  id, nil, nil, nil, nil, observed_at, nil, false, subagents, flagged)
              else
                if atype == "thinking" and frame == nil then
                  frame = frame_for_now(now, frame_count)
                end
                cache_marker_values(
                  id, atype, frame, raw, publication_id, observed_at, source, puppet, subagents,
                  flagged)
              end
            else
              cache_marker_values(id, nil, nil, nil, nil, now, nil, false, subagents, flagged)
            end
          elseif read.kind == "unpublished" then
            unpublished_by_domain[read.domain] = true
          end
          if key then
            local cached = attention_cache[key]
            sample_settled_title(
              key, read.launch_id, pane_method(p, "get_title"), cached and cached.provider or nil)
          end
        end
      end


      local previous_domains = publish_domains_by_window[window_key]
      if previous_domains then
        for domain in pairs(previous_domains) do
          if not domains_present[domain] then retire_publish_observation(domain, window_key) end
        end
      end
      publish_domains_by_window[window_key] = {}
      for domain in pairs(domains_present) do
        publish_domains_by_window[window_key][domain] = true
        update_publish_schedule(
          domain,
          window_key,
          pane_count_by_domain[domain] or 0,
          unpublished_by_domain[domain] == true,
          opts)
      end

      -- WezTerm emits no pane-destroyed event, so a closed pane is detected by
      -- absence: an id this window reported on the previous poll and does not
      -- report now belonged to a pane that is gone, and its marker would otherwise
      -- outlive it forever.
      --
      -- One case is not a closed pane. Detaching a mux domain drops every one of
      -- its panes from this window in a single tick while those panes, and the
      -- processes writing their markers, keep running on the server. So an id is
      -- only swept when this window still holds some pane of that id's domain.
      local previously_seen = seen_marker_ids_by_window[window_key]
      if previously_seen then
        for gone_key, gone_value in pairs(previously_seen) do
          local gone = type(gone_value) == "table" and gone_value
            or { domain = gone_value, kind = "v1", marker_id = gone_key }
          if not seen[gone_key] and domains_present[gone.domain] then
            pane_ids[#pane_ids + 1] = gone_key
            before[gone_key] = attention_cache[gone_key]
            if gone.kind == "v1" then
              remove_marker(dir, gone.marker_id)
            elseif legacy_cache_key_by_marker_id[gone.marker_id] == gone_key then
              legacy_cache_key_by_marker_id[gone.marker_id] = nil
            end
            attention_cache[gone_key] = nil
          end
        end
      end
      seen_marker_ids_by_window[window_key] = seen

      if saw_v2 then
        schedule_ttl_wakeup(window, earliest_wakeup_unix_ns, poll_now_unix_ns, opts)
      else
        schedule_ttl_wakeup(window, nil, nil, opts)
      end

      -- Everything below is about the focused window only. An unfocused window
      -- must neither acknowledge a marker its user has not seen nor be sent a key
      -- action, so an unfocused poll ends here with the cache correct.
      if not window:is_focused() then return end

      local current_active_pane = window:active_pane()
      if current_active_pane then
        local active_read = resolve_pane_read(current_active_pane)
        if active_read.kind == "v1"
            and tab_panes_containing(mux_tabs, active_read.marker_id) then
          acknowledge_focused_pane(active_read.marker_id, { dir = dir, now_ms = now })
        elseif active_read.kind == "v2"
            and tab_panes_containing_read(mux_tabs, active_read) then
          acknowledge_focused_v2_pane(active_read, {
            dir = dir, now_unix_ns = poll_now_unix_ns,
          })
        end
      end

      -- The event pane can transport a redraw when no current pane is available,
      -- but it never authorizes acknowledgement.
      local action_pane = current_active_pane or (opts and opts.active_pane)

      local changed = false
      for _, id in ipairs(pane_ids) do
        local title_state = settled_title_state[id]
        local settled_title = title_state and title_state.settled or nil
        if not same_cached_attention(before[id], attention_cache[id])
            or before_titles[id] ~= settled_title then
          changed = true
          break
        end
      end

      if changed then request_tab_bar_redraw(window, action_pane) end
    end

    --- Apply the shared attention indicator and color decoration to a base title.
    -- `2: ◔ name`: the index first, as WezTerm's own default renders it, then the
    -- attention indicator, then the base. `show_index` false drops the index the
    -- way `show_tab_index_in_tab_bar = false` does for the default renderer.

    return {
      same_cached_attention = same_cached_attention,
      tab_panes_containing = tab_panes_containing,
      tab_panes_containing_read = tab_panes_containing_read,
      review_outranks = review_outranks,
      cache_marker_values = cache_marker_values,
      refresh_cached_pane = refresh_cached_pane,
      acknowledge_focused_pane = acknowledge_focused_pane,
      redraw_window_key = redraw_window_key,
      request_tab_bar_redraw = request_tab_bar_redraw,
      spawn_republish = spawn_republish,
      publish_call_after = publish_call_after,
      schedule_publish_retry = schedule_publish_retry,
      update_publish_schedule = update_publish_schedule,
      schedule_ttl_wakeup = schedule_ttl_wakeup,
    }
  end

  return {
    attention_cache = attention_cache,
    legacy_cache_key_by_marker_id = legacy_cache_key_by_marker_id,
    marker_id_by_local = marker_id_by_local,
    seen_marker_ids_by_window = seen_marker_ids_by_window,
    bind = bind,
  }
end
