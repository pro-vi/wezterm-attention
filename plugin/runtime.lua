return function()
  local attention_cache = {}
  local legacy_cache_key_by_marker_id = {}
  local marker_id_by_local = {}
  local seen_marker_ids_by_window = {}
  local callback_views_by_window = {}
  local delivering_views = false

  local function bind(context)
    local M = context.M
    local defaults = context.defaults
    local wezterm = context.wezterm
    local protocol = context.protocol
    local diagnostic = context.diagnostic
    local report_error_once = context.report_error_once
    local report_warning_once = context.report_warning_once
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
    local withdraw_closed_tab_orders = context.withdraw_closed_tab_orders
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

    local function rebuild_scalar_projection()
      for id in pairs(legacy_cache_key_by_marker_id) do legacy_cache_key_by_marker_id[id] = nil end
      for _, observations in pairs(seen_marker_ids_by_window) do
        for key, observation in pairs(observations) do
          local id = type(observation) == "table" and observation.marker_id or key
          local previous = legacy_cache_key_by_marker_id[id]
          if previous == nil then legacy_cache_key_by_marker_id[id] = key
          elseif previous ~= key then legacy_cache_key_by_marker_id[id] = false end
        end
      end
    end

    local function observed_in_other_window(key, window_key)
      for other_window, observations in pairs(seen_marker_ids_by_window) do
        if other_window ~= window_key and observations[key] then return true end
      end
      return false
    end

    local function observe_pane(window, pane, read)
      if not read.cache_key or (read.kind ~= "v1" and read.kind ~= "v2") then return end
      local window_key = tostring(window:window_id())
      local local_id = tostring(pane_method(pane, "pane_id"))
      local observations = seen_marker_ids_by_window[window_key] or {}
      for key, previous in pairs(observations) do
        if key ~= read.cache_key and type(previous) == "table" and previous.local_id == local_id then
          observations[key] = nil
          if not observed_in_other_window(key, window_key) then attention_cache[key] = nil end
        end
      end
      observations[read.cache_key] = {
        kind = read.kind, marker_id = read.marker_id,
        domain = pane_method(pane, "get_domain_name") or "?",
        local_id = local_id,
      }
      marker_id_by_local[local_id] = read.cache_key
      seen_marker_ids_by_window[window_key] = observations
      rebuild_scalar_projection()
    end

    local function same_cached_attention(a, b)
      if not a or not b then return a == b end
      return a.type == b.type
        and a.frame == b.frame
        and a.activity_type == b.activity_type
        and a.event_id == b.event_id
        and a.source == b.source
        and a.provider == b.provider
        and (a.subagents or 0) == (b.subagents or 0)
        and (a.review == true) == (b.review == true)
        and a.binding_phase == b.binding_phase
        and a.pane_presence == b.pane_presence
        and a.reader_confidence == b.reader_confidence
        and a.binding_health == b.binding_health
        and a.base_title == b.base_title
        and a.settled_title == b.settled_title
    end

    --- Return the panes of the tab holding `target` in a captured tab list. The
    --- poll answers membership from its own inventory; this serves the review
    --- key binding, which has no inventory of its own.
    local function tab_panes_containing_read(tabs, target)
      if not target then return nil end
      for _, tab in ipairs(tabs) do
        -- A captured tab list outlives the tabs in it, so panes() can raise. A
        -- tab that cannot be read cannot be shown to hold this pane, and if it
        -- did hold it that pane is gone, so skipping it answers the question
        -- rather than merely surviving it.
        local panes_ok, panes = pcall(tab.panes, tab)
        if not panes_ok or type(panes) ~= "table" then panes = {} end
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
        id, atype, frame, raw, publication_id, observed_now, source, subagents, flagged)
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

      -- `raw`, `identity` and `source` always describe the marker file,
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
        subagents   = subagents,
        review      = review,
      }
    end

    --- Re-read one pane's files and rebuild its cache entry. The Alt+B handler
    --- changes a pane's state outside the poll loop, and dropping the entry instead
    --- would blank a sibling's ✓ or a "+N" until the next tick.
    local function refresh_cached_pane(dir, id, now)
      local atype, frame, _, _, raw, publication_id, source =
        read_effective_marker(dir, id)
      cache_marker_values(
        id, atype, frame, raw, publication_id, now, source,
        count_live_subagents(dir, id, now), review_flagged(dir, id))
    end

    local function acknowledge_focused_pane(pane_id, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
      local observed_now = (opts and opts.now_ms) or now_ms()

      local id = tostring(pane_id)
      local cached = attention_cache[id]
      if not (cached and acknowledge_set[cached.type]) then return "absent" end
      -- The caller's own observation, when it supplied one. Without it this
      -- compares disk against the shared display cache, which another window's
      -- poll can replace between this poll's enumeration and its focus query --
      -- so equality would mean the two reads agree, not that this poll saw what
      -- it is about to dismiss.
      local observed_identity = opts and opts.observed_identity

      -- The count this tick's poll already read from disk. Acknowledgement is about
      -- the marker only, so it must hand the count back unchanged; recomputing zero
      -- here would drop the "+N" and make every tick a visible change.
      local subagents = cached.subagents or 0
      -- The user's review flag survives acknowledgement, and every cache write
      -- below has to carry it: a flagged pane whose ✓ the user just looked at falls
      -- back to showing the ◆, it does not go quiet.
      local flagged = cached.review == true

      local current_type, current_frame, _, _, raw, publication_id, source =
        read_marker(dir, id)
      if not current_type then
        clear_acknowledgement(dir, id)
        cache_marker_values(id, nil, nil, nil, nil, observed_now, nil, subagents, flagged)
        return "absent"
      end

      local current_identity = marker_identity(raw, publication_id)
      if (observed_identity or cached.identity) ~= current_identity then
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source,
          subagents, flagged)
        return "kept"
      end

      if not acknowledge_set[current_type] then
        clear_acknowledgement(dir, id)
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source,
          subagents, flagged)
        return "kept"
      end

      local write_ack = (opts and opts.write_acknowledgement) or write_acknowledgement
      if not write_ack(dir, id, current_identity) then
        cache_marker_values(
          id, current_type, current_frame, raw, publication_id, observed_now, source,
          subagents, flagged)
        return "failed"
      end

      -- A writer may replace or clear the marker while the sidecar is being
      -- written. Re-read effective truth before updating the cache: only the exact
      -- identity that was viewed is suppressed.
      local effective_type, effective_frame, _, _, effective_raw, effective_publication_id,
        effective_source = read_effective_marker(dir, id)
      cache_marker_values(
        id, effective_type, effective_frame, effective_raw, effective_publication_id, observed_now,
        effective_source, subagents, flagged)
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
    local tab_source_state = { retry_index = 1, retry_at = 0 }

    local function reset_tab_source()
      tab_source_state = { retry_index = 1, retry_at = 0 }
    end

    local function parse_tab_source_response(stdout)
      if not protocol or type(stdout) ~= "string"
          or #stdout > protocol.limits.max_json_bytes then return nil end
      local value = context.decode_json(stdout)
      if type(value) ~= "table" or value.schema ~= 1 or value.command ~= "tab-source"
          or value.status ~= "ok" or value.complete ~= true then return nil end
      local source = value.result
      if type(source) ~= "table" or type(source.socket_path) ~= "string"
          or source.socket_path:sub(1, 1) ~= "/" or source.socket_path:find("%z")
          or not context.is_hex64(source.realm_id) or not context.is_hex64(source.incarnation_id)
          or context.sha256(source.socket_path) ~= source.realm_id then return nil end
      for key in pairs(source) do
        if key ~= "socket_path" and key ~= "realm_id" and key ~= "incarnation_id" then return nil end
      end
      return source
    end

    local function acquire_tab_source(socket)
      local root = M._active_integration_root
      -- Without the writer the shim can only fail, and the backoff would run it
      -- every thirty seconds for as long as the GUI lives.
      if type(socket) ~= "string" or socket:sub(1, 1) ~= "/" or not root
          or not M._active_writer_installed
          or type(wezterm.run_child_process) ~= "function" then return end
      if tab_source_state.socket ~= socket then
        tab_source_state = { socket = socket, retry_index = 1, retry_at = 0 }
      end
      local state = tab_source_state
      if state.source or state.pending or now_ms() < state.retry_at then return end
      local token = {}
      state.pending = token
      local ok, success, stdout = pcall(wezterm.run_child_process,
        { root .. "/bin/attention", "tab-source", "--socket", socket })
      if tab_source_state ~= state or state.pending ~= token then return end
      state.pending = nil
      local source = ok and success and parse_tab_source_response(stdout) or nil
      if source then
        state.source = source
      else
        local delay = publish_backoff_seconds[math.min(state.retry_index, #publish_backoff_seconds)]
        state.retry_at = now_ms() + delay * 1000
        state.retry_index = state.retry_index + 1
        report_error_once("tab-source:" .. socket,
          "cannot identify the tab publisher's GUI socket; publishing without source identity")
      end
    end

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
      argv[#argv + 1] = "--socket"
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

    --- May this window conclude that a storage identity it used to see is gone?
    --- Deleting its records is irreversible, so every way of not knowing answers
    --- no: the pane is still here, or nobody looked where it was, or the domain
    --- it lived on went unwatched, or something on that domain has not yet said
    --- which identity it carries.
    local function decide_absence(evidence, gone_key, gone, live_local_ids)
        local pane = evidence.panes[gone_key]
        if pane then return "present" end
        -- Checked before the gap, because this is something established rather
        -- than something failed to establish: this poll read that same physical
        -- pane answering to a different key. Uncertainty elsewhere is no reason
        -- to forget it, and forgetting it leaves the old key looking absent on
        -- some later tick, after the pane has taken a new local id.
        local replacement = gone.local_id and evidence.identified_by_local[gone.local_id]
        if replacement and replacement ~= gone_key then return "superseded" end
        -- A tab whose contents nobody could bound could be holding this.
        if evidence.gap then return "unknown" end
        local domain = evidence.domains[gone.domain]
        if not domain or not domain.observed then return "unknown" end
        if domain.unresolved then return "unknown" end
        -- The pane is alive and answering to a different storage key now. Its
        -- files belong to it and must stay; this key is simply no longer the one
        -- it goes by, so the key retires without them.
        -- Alive but not identified anywhere: its presence is not proof of which
        -- key replaced this one, so this stays undecided rather than retiring.
        --
        -- Reached when a pane answers with its id and nothing else. pane_id is
        -- held by the handle, while get_domain_name and get_user_vars resolve
        -- through the mux and can fail together, and a pane whose domain could
        -- not be read is filed under "?" -- so it marks that bucket unresolved
        -- and says nothing about the domain the retained entry was recorded on.
        -- Every earlier return is bypassed and this one is what refuses.
        if gone.local_id and live_local_ids[gone.local_id] then return "unknown" end
        return "absent"
    end

    --- What may this poll dismiss for the pane the user is looking at? Only a
    --- publication this poll read itself: acknowledging records that it was
    --- shown, and neither a carried observation nor the shared display cache can
    --- say that. No publication read is an answer, not a missing argument.
    local function decide_acknowledgement(evidence, read)
        if not read or not read.cache_key then return nil end
        local pane = evidence.panes[read.cache_key]
        -- No `observed` test: only panes this poll enumerated are ever put in
        -- here, so membership is the freshness check. A flag that cannot be false
        -- reads like a guard and guards nothing.
        if not pane then return nil end
        if pane.kind ~= read.kind then return nil end
        -- No publication read is an answer: there was nothing to dismiss. Saying
        -- so here keeps the contract in the decision rather than leaving the
        -- executor to notice that its expected value is missing.
        if pane.kind == "v2" and not pane.event_id then return nil end
        if pane.kind == "v1" and not pane.identity then return nil end
        return { kind = pane.kind, marker_id = pane.marker_id, event_id = pane.event_id,
          identity = pane.identity }
    end

    --- What may be reported about a domain's publication work? A count and a
    --- resolution belong to a domain every pane of which answered. Anything less
    --- renews the observation -- so a retry is not dropped for going a round
    --- unseen -- without letting the panes that did answer stand for the rest.
    local function decide_publication(domain_item, gap)
        if not domain_item then return nil end
        if not domain_item.observed then return nil end
        if gap then return { action = "renew", unpublished = domain_item.unpublished == true } end
        -- Coverage, not identity resolution: a domain every tab of which
        -- answered can be concluded about even when one of its panes has yet to
        -- publish, because that pane is the reason the schedule exists.
        local complete = domain_item.observed and not domain_item.indeterminate
        if complete then
          return { action = "conclude", count = domain_item.count,
            unpublished = domain_item.unpublished == true }
        end
        if domain_item.unpublished == true then
          -- Positively unpublished is the one conclusion a partial look can
          -- support: a pane that answered said so.
          return { action = "renew", unpublished = true }
        end
        return { action = "renew" }
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

    --- Mux windows that exist, by id as a decimal string, whether or not a GUI
    --- window shows them. Nil when the mux cannot be listed.
    local function mux_window_keys()
      local mux = wezterm.mux
      if not mux or type(mux.all_windows) ~= "function" then return nil end
      local ok, windows = pcall(mux.all_windows)
      if not ok or type(windows) ~= "table" then return nil end
      local keys = {}
      for _, mux_window in ipairs(windows) do
        local id_ok, id = pcall(mux_window.window_id, mux_window)
        if id_ok and id ~= nil then keys[tostring(id)] = true end
      end
      return keys
    end

    --- Windows that are still open. A workspace switch makes the GUI window
    --- show another workspace's mux window, so the one it showed leaves
    --- gui_windows() while it still exists, with its tabs, to be shown again.
    --- Only a window gone from both has closed. Without the mux listing this is
    --- the GUI inventory alone, as before.
    local function open_window_keys(opts)
      local live = gui_window_keys(opts)
      if not live then return nil end
      local existing = mux_window_keys()
      if existing then
        for key in pairs(existing) do live[key] = true end
      end
      return live
    end

    local function prune_closed_publish_windows(current_window_key, opts, dir)
      local live = gui_window_keys(opts)
      if not live then return end
      -- The callback's window is authoritative even if WezTerm's inventory is
      -- between insertion and publication for a newly created GUI window.
      live[current_window_key] = true
      local open = open_window_keys(opts) or live
      open[current_window_key] = true
      withdraw_closed_tab_orders(dir, open)
      for window_key in pairs(seen_marker_ids_by_window) do
        if not open[window_key] then seen_marker_ids_by_window[window_key] = nil end
      end
      -- Publication work is about what a poll can see: a hidden window is not
      -- polled, so it cannot renew or conclude an observation.
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
        local live = gui_window_keys(opts)
        for window_key, observation in pairs(current.window_observations) do
          -- When inventory fails, only a new poll can renew this observation.
          -- Retiring stale retry evidence does not declare the pane absent.
          if (live and not live[window_key]) or (not live and not observation.fresh) then
            current.window_observations[window_key] = nil
            local domains = publish_domains_by_window[window_key]
            if domains then
              domains[current.domain] = nil
              if next(domains) == nil then publish_domains_by_window[window_key] = nil end
            end
          else
            observation.fresh = false
          end
        end
        if not retain_publish_schedule(current) then return end
        spawn_republish(current.domain, current.socket, current.root)
        current.retry_index = current.retry_index + 1
        schedule_publish_retry(current, opts)
      end)
      return true
    end

    --- `partial` means part of this domain was not enumerated this tick. The
    --- observation is still renewed, because the retry above drops one that goes
    --- a round without being seen, but nothing is concluded from numbers drawn
    --- from the tabs that happened to answer.
    local function update_publish_schedule(
        domain, window_key, pane_count, unpublished, opts, partial)
      local socket = context.unix_domain_socket(domain)
      local root = M._active_integration_root
      if not socket and unpublished then
        report_warning_once("unpublished-domain:" .. domain, "panes on domain " .. domain
          .. " have not published their identity, and there is no socket on this machine to "
          .. "republish it through; they show no attention until something in the pane "
          .. "publishes it, as the shell integration does at each prompt")
      end
      if not socket or not root or not M._active_writer_installed then return false end
      local schedule = publish_schedule_by_realm[socket]
      if not schedule then
        -- A partial look cannot start one either: its count is the count of the
        -- panes that answered, and stabilisation is measured on that count.
        if partial or not unpublished then return false end
        publish_schedule_by_realm[socket] = {
          socket = socket,
          domain = domain,
          root = root,
          window_observations = {
            [window_key] = {
              pane_count = pane_count, stable_polls = 1, unpublished = true, fresh = true,
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
      if partial then
        if observation then
          observation.fresh = true
          -- A pane that answered said it is unpublished. That is the one thing a
          -- partial look establishes, and dropping it lets another window's
          -- resolution retire a schedule this one still needs.
          if unpublished then observation.unpublished = true end
        elseif unpublished then
          -- This window has said nothing here before, and what it says now is
          -- that work is outstanding. Record the requirement with no count: a
          -- partial look cannot supply one, and stabilisation must wait for a
          -- tick that can.
          schedule.window_observations[window_key] =
            { pane_count = nil, stable_polls = 0, unpublished = true, fresh = true }
        end
        return false
      end
      if not observation then
        schedule.window_observations[window_key] = {
          pane_count = pane_count, stable_polls = 1, unpublished = unpublished, fresh = true,
        }
        return false
      end
      local was_unpublished = observation.unpublished
      observation.fresh = true
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
      for _, item in pairs(schedule.window_observations) do item.fresh = false end
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
    --- Returns (type, frame, source, reserved, subagents, review) or nil. `source` is
    --- the marker's JSON `source` string when it carried one; `reserved` is always
    --- false to retain tuple positions; `subagents` is how many of the pane's
    --- subagents ran a tool call in the last ten minutes, 0 when none; `review` is
    --- true when the user has flagged the pane with Alt+B.
    ---
    --- `type` is the effective one: it is `review` when the flag outranks the
    --- marker file, and the marker's own type when that outranks the flag — in
    --- which case the flag is still reported by the sixth return.
    ---
    --- A pane with live subagents and no marker returns (nil, nil, nil, false, n):
    --- the count is real even though there is no marker type to report.
    --- Without an explicit legacy dir, a scalar observed in multiple full
    --- addresses returns nil. Use get_attention_view(pane) to disambiguate.
    function M.get_attention(marker_id, opts)
      local id = tostring(marker_id)
      if opts and opts.dir then
        -- The id becomes a path segment, and the read below can remove a stale
        -- acknowledgement beside it.
        if not context.canonical_pane_id(id) then return nil end
        local atype, frame, _, _, _, _, source = read_effective_marker(opts.dir, id)
        local now = (opts and opts.now_ms) or now_ms()
        local flagged = review_flagged(opts.dir, id) or atype == "review"
        if flagged and review_outranks(atype) then
          atype, frame = "review", nil
        end
        return atype, frame, source, false, count_live_subagents(opts.dir, id, now), flagged
      end
      local mapped = legacy_cache_key_by_marker_id[id]
      if mapped == false then return nil end -- More than one observed full address.
      local cached = mapped and attention_cache[mapped] or nil
      if not cached then cached = attention_cache[id] end
      if cached then
        return cached.type, cached.frame, cached.source, false, cached.subagents or 0,
          cached.review == true
      end
      return nil
    end

    --- Return a copy of the cached full-pane v2 view for a pane, or nil when the
    --- pane has no cache entry. This performs no filesystem or process work.
    local function copy_public_view(cached)
      return {
        provider = cached.provider,
        binding_id = cached.binding_id,
        binding_phase = cached.binding_phase,
        type = cached.type,
        event_id = cached.event_id,
        subagents = cached.subagents or 0,
        review = cached.review == true,
        reader_confidence = cached.reader_confidence,
        activity_type = cached.activity_type,
        source = cached.source,
        address = cached.address and context.deep_copy(cached.address) or nil,
        launch_id = cached.launch_id,
        marker_id = cached.marker_id,
        pane_presence = cached.pane_presence,
        binding_health = cached.binding_health,
        lifecycle = cached.lifecycle and context.deep_copy(cached.lifecycle) or nil,
      }
    end

    function M.get_attention_view(pane)
      local read = resolve_pane_read(pane)
      local cached = read.cache_key and attention_cache[read.cache_key] or nil
      if not cached then return nil end
      if read.kind == "v2" and cached.launch_id ~= read.launch_id then return nil end
      return copy_public_view(cached)
    end

    local function same_public_value(a, b)
      if type(a) ~= type(b) then return false end
      if type(a) ~= "table" then return a == b end
      for key, value in pairs(a) do if not same_public_value(value, b[key]) then return false end end
      for key in pairs(b) do if a[key] == nil then return false end end
      return true
    end

    --- Once per distinct error, so a consumer's bug is visible with its own
    --- words and a failure on every poll is still one line. The count of
    --- distinct errors is bounded too: text that changes on every call would
    --- otherwise be a line per poll.
    local view_change_failures, distinct_view_change_failures = {}, 0
    local function report_view_change_failure(failure)
      local text = tostring(failure):sub(1, 512)
      if view_change_failures[text] then return end
      view_change_failures[text] = true
      distinct_view_change_failures = distinct_view_change_failures + 1
      if distinct_view_change_failures > 16 then
        report_error_once("on-view-change-error-cap",
          "on_view_change keeps failing with new errors; further ones are not logged")
        return
      end
      report_error_once("on-view-change-error:" .. text,
        "on_view_change failed: " .. text .. "; future polls remain enabled")
    end

    --- `unsettled` says a tab in this window could not be read. A scope nobody
    --- could look at has not been lost, and reporting it so would have a consumer
    --- discard state it still needs -- a dismissal, a policy -- and rebuild it as
    --- new when the pane comes back.
    ---
    --- Baselines are per window, and each window delivers from its own poll. A
    --- pane moved between windows is therefore scope_lost in one and initial in
    --- the other, in whichever order the two windows happen to poll; nothing
    --- orders messages across windows.
    local function deliver_window_views(window, entries, opts, unsettled)
      local callback = M._on_view_change
      if not callback then return end
      local window_key = redraw_window_key(window)
      local previous = callback_views_by_window[window_key] or {}
      local next_views, messages, losses = {}, {}, {}
      local function lost(state, id)
        losses[#losses + 1] = { kind = "scope_lost", window_id = id, previous_scope = context.deep_copy(state.scope) }
      end
      for key, read in pairs(entries) do
        local cached = attention_cache[key]
        local old = previous[key]
        if cached and cached.launch_id == read.launch_id then
          local target = cached._records and cached._records.selection_target
          local scope = target and { address = context.deep_copy(read.address), launch_id = read.launch_id, target = context.deep_copy(target) }
          if not scope and old and old.scope.launch_id == read.launch_id then scope = old.scope end
          if scope then
            local view = copy_public_view(cached)
            local replaced = old and not same_public_value(old.scope, scope)
            if replaced then lost(old, window:window_id()) end
            if not old or replaced or not same_public_value(old.view, view) then
              messages[#messages + 1] = { kind = (not old or replaced) and "initial" or "updated",
                window_id = window:window_id(), scope = context.deep_copy(scope), view = context.deep_copy(view) }
            end
            next_views[key] = { scope = context.deep_copy(scope), view = view }
          end
        end
      end
      for key, state in pairs(previous) do
        if not next_views[key] then
          if unsettled then next_views[key] = state
          else lost(state, window:window_id()) end
        end
      end
      callback_views_by_window[window_key] = next_views
      local live = open_window_keys(opts)
      if live then
        live[window_key] = true
        for other, states in pairs(callback_views_by_window) do
          if not live[other] then
            for _, state in pairs(states) do lost(state, tonumber(other) or other) end
            callback_views_by_window[other] = nil
          end
        end
      end
      delivering_views = true
      for _, batch in ipairs({ losses, messages }) do
        for _, message in ipairs(batch) do
          local ok, failure = pcall(callback, message)
          if not ok then report_view_change_failure(failure) end
        end
      end
      delivering_views = false
    end

    --- Remove the attention marker for a marker id (see M.pane_marker_id).
    function M.remove_marker(marker_id, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local id = tostring(marker_id)
      -- The id becomes a path segment of four removals; "../x" would reach
      -- outside the state directory.
      if not context.canonical_pane_id(id) then return end
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
      if delivering_views then return end
      context.refresh_domain_facts()
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local mux_win = window:mux_window()
      if not mux_win then return end
      local window_key = redraw_window_key(window)
      prune_closed_publish_windows(window_key, opts, dir)

      -- A tab that closes between this listing and its panes() call below is
      -- still in the list and already out of the mux, so panes() raises. That is
      -- a race with the user, not a fault: M.doctor already treats it that way.
      local tabs_ok, mux_tabs = pcall(mux_win.tabs, mux_win)
      if not tabs_ok or type(mux_tabs) ~= "table" then return end
      -- The absence sweep below deletes records for panes it cannot see, so a
      -- pane it merely failed to read must not look absent. These carry the
      -- unreadable tabs' panes from the last tick that could read them.
      -- What this tick established, and how. The decisions below need different
      -- strengths of the same fact -- a pane this tick read is enough to dismiss
      -- its notification, where one it merely used to know of is not -- so
      -- provenance travels with the fact instead of living in a separate table
      -- each consumer combines for itself.
      -- `gap` means a listed tab could not be read and nothing is known about
      -- what it held -- it has never been read successfully, so there is no
      -- remembered membership to bound it with. Its scope is the whole window,
      -- which is why it blocks every negative conclusion rather than one domain's.
      -- `identified_by_local` maps a GUI-local pane to the storage key it was
      -- read under this tick, so a key it used to answer to can be recognised as
      -- replaced rather than as missing.
      local evidence = { panes = {}, domains = {}, gap = false, identified_by_local = {} }

      --- Domain evidence, created on first mention. `observed` means a pane was
      --- enumerated on it now. `unresolved` means a pane on it has no usable
      --- identity, so nothing can be concluded about which identities are gone.
      --- `indeterminate` is narrower than `unresolved`: a pane's identity could
      --- not be read at all, so its publication status is unknown too. A pane
      --- that is merely unpublished is not indeterminate -- it is the work the
      --- schedule exists for.
      --- `count` and `unpublished` describe only the panes that answered.
      local function domain_evidence(domain)
        local item = evidence.domains[domain]
        if not item then
          item = { observed = false, unresolved = false,
            indeterminate = false, count = 0 }
          evidence.domains[domain] = item
        end
        return item
      end

      local pane_ids = {}
      local before = {}
      local title_enabled = M._active_settled_title_fallback ~= false
      local before_titles = title_enabled and {} or nil
      local callback_entries = {}
      local seen = {}             -- cache key → identity and domain observed in this window
      local live_local_ids = {}   -- GUI pane lifetime is independent of its storage key.

      local now = (opts and opts.now_ms) or now_ms()
      local cfg_indicators = M._active_indicators or defaults.indicators
      local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
      local frame_count = #frames
      local utc_sampled = false
      local poll_now_unix_ns
      local poll_utc_error
      local saw_v2 = false
      local earliest_wakeup_unix_ns

      --- The time now, for a read that saw a write time ahead of this poll's
      --- sample. Nil when the caller fixed the poll's time.
      local function resample_utc()
        if opts and type(opts.utc_now) == "function" then
          local ok, value = pcall(opts.utc_now)
          return ok and format_unix_ns20(value) or nil
        elseif opts and opts.now_unix_ns then
          return nil
        end
        return (wezterm_now_unix_ns20())
      end

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
        -- tab_id is the id this handle was built from, so it answers without
        -- consulting the mux and cannot raise the way panes() can.
        local id_ok, tab_id = pcall(tab.tab_id, tab)
        tab_id = id_ok and tab_id or nil
        local panes_ok, tab_panes = pcall(tab.panes, tab)
        if not panes_ok or type(tab_panes) ~= "table" then
          -- The tab went away between the listing and here. Keep the remaining
          -- tabs -- aborting would cost every later tab its refresh -- and treat
          -- the panes it held as unknown rather than as closed. They stay
          -- protected only while this tab is still listed, so once WezTerm drops
          -- it they are swept normally.
          tab_panes = {}
          -- What a tab held when it last answered is a lower bound on what it
          -- holds now, not an upper one -- a pane can be moved into a tab after
          -- its last successful read, and would appear in no memory of it. So
          -- there is nothing to remember that would narrow this: the tab is a
          -- gap, and the gap is what every decision consults.
          evidence.gap = true
        end
        for _, p in ipairs(tab_panes) do
          local domain = pane_method(p, "get_domain_name") or "?"
          local domain_item = domain_evidence(domain)
          domain_item.observed = true
          -- Recorded here rather than beside the cache key below, so a pane with
          -- no identity to key still tells its tab which domain it was on.
          domain_item.count = domain_item.count + 1
          local local_id = tostring(pane_method(p, "pane_id"))
          live_local_ids[local_id] = true
          local read = resolve_pane_read(p)
          local key = read.cache_key
          marker_id_by_local[local_id] = key or false
          if key and title_enabled then
            local prior_title = settled_title_state[key]
            before_titles[key] = prior_title and prior_title.settled or nil
          end

          if read.kind == "invalid" then
            local item = read.diagnostic or invalid("pane identity is invalid")
            report_error_once("v2-identity:" .. local_id .. ":" .. item.code,
              item.code .. ": " .. item.message)
          elseif read.kind == "v2" then
            callback_entries[key] = read
            saw_v2 = true
            local now_unix_ns, utc_error = sample_utc_once()
            if utc_error then
              report_error_once("v2-clock:" .. local_id,
                utc_error .. ": WezTerm UTC is unavailable; TTL-bearing v2 state is omitted")
            end
            seen[key] = { domain = domain, kind = "v2", marker_id = read.marker_id, local_id = local_id }
            evidence.panes[key] = { kind = "v2", domain = domain,
              marker_id = read.marker_id, local_id = local_id }
            evidence.identified_by_local[local_id] = key
            pane_ids[#pane_ids + 1] = key
            before[key] = attention_cache[key]
            local view = read_attention_view(read, now_unix_ns, {
              dir = dir,
              glob = opts and opts.glob,
              previous_view = before[key],
              resample_utc = resample_utc,
            })
            -- A hook writes no frame, so a thinking view is animated from the
            -- wall clock, the same way a v1 marker without one is.
            if view.type == "thinking" and view.frame == nil then
              view.frame = frame_for_now(now, frame_count)
            end
            attention_cache[key] = view
            evidence.panes[key].event_id = view.event_id
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
            seen[id] = { domain = domain, kind = "v1", marker_id = id, local_id = local_id }
            evidence.panes[id] = { kind = "v1", domain = domain,
              marker_id = id, local_id = local_id }
            evidence.identified_by_local[local_id] = id
            pane_ids[#pane_ids + 1] = id
            before[id] = attention_cache[id]
            local atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source =
              read_marker(dir, id)
            -- One read of each sidecar per pane per tick, with this tick's clock,
            -- whether or not the pane has a marker.
            local subagents = count_live_subagents(dir, id, now)
            local flagged = review_flagged(dir, id)
            local acknowledged = acknowledgement_matches(dir, id, raw, publication_id)
            if atype then
              -- The publication this poll read, kept with the pane's evidence, and
              -- only when there was one: marker_identity of nothing still returns
              -- a string, which would look like a publication to compare against.
              -- The acknowledgement below compares against this rather than the
              -- display cache, which another window's poll can replace between
              -- the enumeration and the focus query.
              evidence.panes[id].identity = marker_identity(raw, publication_id)
              local cached = attention_cache[id]
              local observed_at = now
              if cached and cached.raw == raw and cached.observed_at then
                observed_at = cached.observed_at
              end

              local effective_updated_at = updated_at or observed_at
              local ttl = stale_ttl_ms(atype, marker_ttl_ms)
              if ttl and now - effective_updated_at > ttl then
                remove_expired_marker(dir, id)
                cache_marker_values(id, nil, nil, nil, nil, now, nil, subagents, flagged)
              elseif acknowledged then
                cache_marker_values(
                  id, nil, nil, nil, nil, observed_at, nil, subagents, flagged)
              else
                if atype == "thinking" and frame == nil then
                  frame = frame_for_now(now, frame_count)
                end
                cache_marker_values(
                  id, atype, frame, raw, publication_id, observed_at, source, subagents,
                  flagged)
              end
            else
              cache_marker_values(id, nil, nil, nil, nil, now, nil, subagents, flagged)
            end
          elseif read.kind == "unpublished" then
            -- Positively unpublished: work to schedule, and a reason nothing on
            -- this domain can be declared absent.
            domain_item.unpublished = true
            domain_item.unresolved = true
          end
          -- An identity that failed to parse is no more resolved than one that
          -- has not arrived, and is equally capable of being the pane whose
          -- records are about to be swept. It is not evidence of publication
          -- work either way, so it sets no unpublished flag.
          if read.kind == "invalid" then
            domain_item.unresolved = true
            -- Unlike an unpublished pane, this one says nothing about whether the
            -- domain's publication work is done: the read failed, so "none of
            -- these is unpublished" is not something this tick can claim.
            domain_item.indeterminate = true
          end
          if key and title_enabled then
            local cached = attention_cache[key]
            sample_settled_title(
              key, read.launch_id, pane_method(p, "get_title"), cached and cached.provider or nil)
          end
        end
      end


      -- Retiring says this window left the domain. An unreadable tab has not said
      -- that, so a domain it remembers counts as still here.
      local possible_domains = {}
      for domain, item in pairs(evidence.domains) do
        if item.observed then possible_domains[domain] = true end
      end
      local previous_domains = publish_domains_by_window[window_key]
      if previous_domains then
        for domain in pairs(previous_domains) do
          -- A tab nobody could read might be the one holding this domain.
          if not possible_domains[domain] and not evidence.gap then
            retire_publish_observation(domain, window_key)
          end
        end
      end
      -- A domain with no evidence this tick is normally gone. During a gap it is
      -- merely unsettled, and dropping it here would lose the obligation: the
      -- tick that can finally exclude it would have nothing left to retire.
      if evidence.gap and previous_domains then
        for domain in pairs(previous_domains) do possible_domains[domain] = true end
      end
      publish_domains_by_window[window_key] = possible_domains
      -- Every domain this window still has any evidence for, so a retry is never
      -- dropped merely because the tab holding its pane went quiet. What each one
      -- is allowed to say is decide_publication's answer, not this loop's.
      for domain in pairs(possible_domains) do
        local decision = decide_publication(evidence.domains[domain], evidence.gap)
        -- Carried over only because the window is unsettled: renew it so the
        -- retry is not dropped for staleness, and conclude nothing.
        if not decision and evidence.gap then decision = { action = "renew" } end
        if decision then
          update_publish_schedule(
            domain, window_key,
            decision.count or 0,
            decision.unpublished == true,
            opts,
            decision.action ~= "conclude")
        end
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
      -- Asked at most once a tick, and only when something is otherwise eligible
      -- for unlinking. Every other source of ownership is a saved observation --
      -- what some window saw when it last polled -- and a pane that has just been
      -- moved into a window appears in no observation until that window polls.
      -- Which window's status callback runs first would otherwise decide whether
      -- a moved pane keeps its files. This asks the mux instead of the memories.
      --
      -- `complete` is false as soon as any part of the walk fails or any pane
      -- declines to say who it is, because then some pane that was not read could
      -- be the owner. Incomplete is not permission.
      local mux_ownership
      local function current_marker_owners()
        if mux_ownership then return mux_ownership end
        local markers, complete = {}, true
        local mux = wezterm.mux
        local windows_ok, windows = pcall(function() return mux and mux.all_windows() end)
        if not windows_ok or type(windows) ~= "table" then
          mux_ownership = { markers = markers, complete = false }
          return mux_ownership
        end
        for _, mux_window in ipairs(windows) do
          local tabs_ok, window_tabs = pcall(mux_window.tabs, mux_window)
          if not tabs_ok or type(window_tabs) ~= "table" then complete = false
          else
            for _, window_tab in ipairs(window_tabs) do
              local panes_ok, tab_panes = pcall(window_tab.panes, window_tab)
              if not panes_ok or type(tab_panes) ~= "table" then complete = false
              else
                for _, owned in ipairs(tab_panes) do
                  local owner = resolve_pane_read(owned)
                  if (owner.kind == "v1" or owner.kind == "v2") and owner.marker_id then
                    markers[owner.marker_id] = true
                  else
                    complete = false
                  end
                end
              end
            end
          end
        end
        mux_ownership = { markers = markers, complete = complete }
        return mux_ownership
      end

      local previously_seen = seen_marker_ids_by_window[window_key]
      -- A pane the sweep did not observe because its tab could not be read is
      -- unknown, not closed. It is carried forward as still present, so it is
      -- neither swept now nor treated as newly arrived next tick.
      if previously_seen then
        for gone_key, gone_value in pairs(previously_seen) do
          local gone = type(gone_value) == "table" and gone_value
            or { domain = gone_value, kind = "v1", marker_id = gone_key }
          local verdict = decide_absence(evidence, gone_key, gone, live_local_ids)
          if verdict == "unknown" and not seen[gone_key] then
            -- Keep remembering it as well as keeping its files. Dropping it here
            -- would leave the next tick -- which may be able to decide -- nothing
            -- to compare against, and the record would outlive the pane for good.
            seen[gone_key] = gone_value
          elseif verdict == "superseded" and not seen[gone_key] then
            -- The pane lives on under another key. Retire this one from the cache
            -- and the inventory, and leave every file alone: they are that pane's.
            local shared = observed_in_other_window(gone_key, window_key)
            pane_ids[#pane_ids + 1] = gone_key
            before[gone_key] = attention_cache[gone_key]
            if not shared then attention_cache[gone_key] = nil end
          elseif verdict == "absent" and not seen[gone_key] then
            local shared = observed_in_other_window(gone_key, window_key)
            -- The key is retired either way. Unlinking the files it names is a
            -- separate question: another v1 pane in this window may still be
            -- writing them. These are the cheap vetoes; each one is a
            -- positive sighting, and any of them is enough to keep the files.
            -- No live-local-id term: reaching this verdict already means that
            -- check passed, since decide_absence answers "unknown" for a pane
            -- whose handle is still enumerated.
            local may_unlink = not shared and gone.kind == "v1"
            local owners = may_unlink and current_marker_owners() or nil
            if owners and not owners.markers[gone.marker_id] and not owners.complete then
              -- Nothing sighted, and the search could not finish. Keep the files
              -- and keep the obligation, so a tick that can finish still decides.
              seen[gone_key] = gone_value
            else
              pane_ids[#pane_ids + 1] = gone_key
              before[gone_key] = attention_cache[gone_key]
              if owners and not owners.markers[gone.marker_id] then
                remove_marker(dir, gone.marker_id)
              end
              if not shared then attention_cache[gone_key] = nil end
            end
          end
        end
      end
      seen_marker_ids_by_window[window_key] = seen
      -- A scalar cannot select one of several realms. Build this projection
      -- from all observed windows, rather than letting poll/overlay order win.
      rebuild_scalar_projection()

      if saw_v2 then
        schedule_ttl_wakeup(window, earliest_wakeup_unix_ns, poll_now_unix_ns, opts)
      else
        schedule_ttl_wakeup(window, nil, nil, opts)
      end

      -- Everything below is about the focused window only. An unfocused window
      -- must neither acknowledge a marker its user has not seen nor be sent a key
      -- action, so an unfocused poll ends here with the cache correct.
      if not window:is_focused() then
        deliver_window_views(window, callback_entries, opts, evidence.gap)
        return
      end

      local current_active_pane = window:active_pane()
      if current_active_pane then
        local active_read = resolve_pane_read(current_active_pane)
        local candidate = decide_acknowledgement(evidence, active_read)
        if candidate and candidate.kind == "v1" then
          acknowledge_focused_pane(active_read.marker_id, {
            dir = dir, now_ms = now,
            -- The publication this poll read, so the helper compares against its
            -- caller's observation rather than against a display cache another
            -- window's poll may have replaced in between.
            observed_identity = candidate.identity,
          })
        elseif candidate and candidate.kind == "v2" then
          acknowledge_focused_v2_pane(active_read, {
            dir = dir, now_unix_ns = poll_now_unix_ns,
            observed_event_id = candidate.event_id,
            resample_utc = resample_utc,
          })
        end
      end

      deliver_window_views(window, callback_entries, opts, evidence.gap)

      -- The event pane can transport a redraw when no current pane is available,
      -- but it never authorizes acknowledgement.
      local action_pane = current_active_pane or (opts and opts.active_pane)

      local changed = false
      for _, id in ipairs(pane_ids) do
        local title_state = title_enabled and settled_title_state[id]
        local settled_title = title_state and title_state.settled or nil
        if not same_cached_attention(before[id], attention_cache[id])
            or (title_enabled and before_titles[id] ~= settled_title) then
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
      tab_source = function() return tab_source_state.source end,
      reset_tab_source = reset_tab_source,
      acquire_tab_source = acquire_tab_source,
      parse_tab_source_response = parse_tab_source_response,
      same_cached_attention = same_cached_attention,
      observe_pane = observe_pane,
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
