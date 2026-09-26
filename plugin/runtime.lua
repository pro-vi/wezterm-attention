return function()
  local attention_cache = {}
  local cache_key_by_marker_id = {}
  local marker_id_by_local = {}
  local seen_marker_ids_by_window = {}
  local callback_views_by_window = {}
  local delivering_views = false

  --- The key a pane the GUI draws is cached under, from its GUI-local number:
  --- the one the last poll found for it, or nil when that poll found none (a
  --- mux-client pane that has not published its $WEZTERM_PANE) or none has
  --- walked the pane yet.
  local function drawn_pane_key(local_id)
    return marker_id_by_local[local_id] or nil
  end

  local function bind(context)
    local M = context.M
    local defaults = context.defaults
    local wezterm = context.wezterm
    local protocol_api = context.protocol_api
    local protocol = protocol_api.protocol
    local diagnostic = protocol_api.diagnostic
    local v2_pane_root = protocol_api.v2_pane_root
    local read_expected_record = protocol_api.read_expected_record
    local now_ms = protocol_api.now_ms
    local frame_for_now = protocol_api.frame_for_now
    local format_unix_ns20 = protocol_api.format_unix_ns20
    local wezterm_now_unix_ns20 = protocol_api.wezterm_now_unix_ns20
    local seconds_until_after = protocol_api.seconds_until_after
    local report_error_once = context.overlays.report_error_once
    local report_warning_once = context.overlays.report_warning_once
    local withdraw_closed_tab_orders = context.overlays.withdraw_closed_tab_orders
    local reader = context.reader
    local resolve_pane_read = reader.resolve_pane_read
    local read_attention_view = reader.read_attention_view
    local pane_method = reader.pane_method
    local sample_settled_title = context.titles.sample_settled_title
    local settled_title_state = context.titles.settled_title_state

    local function rebuild_scalar_projection()
      for id in pairs(cache_key_by_marker_id) do cache_key_by_marker_id[id] = nil end
      for _, observations in pairs(seen_marker_ids_by_window) do
        for key, observation in pairs(observations) do
          local id = observation.marker_id
          local previous = cache_key_by_marker_id[id]
          if previous == nil then cache_key_by_marker_id[id] = key
          elseif previous ~= key then cache_key_by_marker_id[id] = false end
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
      if read.kind ~= "claimed" then return end
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
          if read.kind == "claimed" and read.cache_key == target.cache_key then
            return panes
          end
        end
      end
      return nil
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

    --- The wait before retry number `index`; the last wait repeats.
    local function backoff_delay(index)
      return publish_backoff_seconds[math.min(index, #publish_backoff_seconds)]
    end

    --- The socket of this GUI's own mux: the one the tab source was asked
    --- about, else the one in this GUI's environment. Nil unless it is an
    --- absolute path.
    local function own_socket()
      local socket = tab_source_state.socket or os.getenv("WEZTERM_UNIX_SOCKET")
      if type(socket) == "string" and socket:sub(1, 1) == "/" then return socket end
      return nil
    end

    local function parse_tab_source_response(stdout)
      if not protocol or type(stdout) ~= "string"
          or #stdout > protocol.limits.max_json_bytes then return nil end
      local value = protocol_api.decode_json(stdout)
      if type(value) ~= "table" or value.schema ~= 1 or value.command ~= "tab-source"
          or value.status ~= "ok" or value.complete ~= true then return nil end
      local source = value.result
      if type(source) ~= "table" or type(source.socket_path) ~= "string"
          or source.socket_path:sub(1, 1) ~= "/" or source.socket_path:find("%z")
          or not protocol_api.is_hex64(source.realm_id) or not protocol_api.is_hex64(source.incarnation_id)
          or protocol_api.sha256(source.socket_path) ~= source.realm_id then return nil end
      for key in pairs(source) do
        if key ~= "socket_path" and key ~= "realm_id" and key ~= "incarnation_id" then return nil end
      end
      return source
    end

    --- Can this GUI ask the attention command who it is? Without the writer
    --- the shim can only fail, and the backoff would run it every thirty
    --- seconds for as long as the GUI lives. Without the manifest no answer
    --- can be read.
    local function can_acquire_tab_source(socket)
      return type(socket) == "string" and socket:sub(1, 1) == "/"
        and protocol ~= nil
        and M._active_integration_root ~= nil and M._active_writer_installed == true
        and type(wezterm.run_child_process) == "function"
    end

    --- Has the first run failed, and then the retry after each backoff wait?
    --- Retries go on every thirty seconds, but a window held past this point
    --- would stay out of `attention tabs` for as long as no answer comes.
    local function retries_used_up(state)
      return state.retry_index > #publish_backoff_seconds + 1
    end

    --- What a tab order is published under now: "ready" with the source,
    --- "unavailable" when no answer can come or none came through the whole
    --- backoff, and "pending" while one still can. A failed run schedules
    --- its retry, so until the backoff is used up it is still "pending".
    local function tab_source_status()
      local state = tab_source_state
      if state.source then return "ready", state.source end
      if retries_used_up(state) or not can_acquire_tab_source(own_socket()) then
        return "unavailable"
      end
      return "pending"
    end

    local realm_by_socket = {}

    --- What this GUI knows of its own mux, the one its local panes run in:
    --- the tab-source status, then the realm and incarnation the answer named.
    --- Before an answer the realm is the hash of this GUI's socket path as its
    --- environment spells it, which is the writer's realm whenever that path
    --- is already canonical; the incarnation is not known. Nil realm when this
    --- GUI has no socket to go by.
    local function own_mux_identity()
      local status, source = tab_source_status()
      if source then return status, source.realm_id, source.incarnation_id end
      local socket = own_socket()
      if not socket or not protocol then return status end
      local realm = realm_by_socket[socket]
      if not realm then
        realm = protocol_api.sha256(socket)
        realm_by_socket[socket] = realm
      end
      return status, realm
    end

    local function acquire_tab_source(socket)
      local root = M._active_integration_root
      if not can_acquire_tab_source(socket) then return end
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
        state.retry_at = now_ms() + backoff_delay(state.retry_index) * 1000
        state.retry_index = state.retry_index + 1
        report_error_once("tab-source:" .. socket,
          "cannot identify the tab publisher's GUI socket yet; unpublished tab orders wait for a retry")
        if retries_used_up(state) then
          report_error_once("tab-source-used-up:" .. socket,
            "no tab-source answer through the whole backoff; publishing tab orders without source identity")
        end
      end
    end

    --- The command line that runs `attention` from the integration `root`
    --- with `arguments`, against the state directory `dir`. The directory is
    --- handed over rather than left to the child's environment, which is
    --- this GUI's and may name no root or another one. WezTerm's own
    --- directory goes first on PATH, for the `wezterm cli` a publication runs.
    local function attention_argv(root, dir, arguments)
      local argv = { "env", "WEZTERM_ATTENTION_DIR=" .. dir }
      local executable_dir = wezterm.executable_dir
      if type(executable_dir) == "string" and executable_dir ~= "" then
        local inherited_path = os.getenv("PATH")
        argv[#argv + 1] = "PATH=" .. executable_dir
          .. (inherited_path and inherited_path ~= "" and (":" .. inherited_path) or "")
      end
      argv[#argv + 1] = root .. "/bin/attention"
      for _, argument in ipairs(arguments) do argv[#argv + 1] = argument end
      return argv
    end

    local function spawn_republish(domain, socket, root)
      if type(wezterm.background_child_process) ~= "function" then
        report_error_once("publish-spawn:" .. socket,
          "cannot republish mux identity: background_child_process is unavailable")
        return false
      end
      local argv = attention_argv(root, M._active_dir,
        { "hooks", "publish", "--socket", socket, "--quiet" })
      local ok, started = pcall(wezterm.background_child_process, argv)
      if not ok or started == false then
        report_error_once("publish-spawn:" .. socket,
          "failed to start mux identity publication for " .. domain)
        return false
      end
      return true
    end

    --- Run `attention plugin <action>` for the pane `read` names, and return
    --- the result of its answer. The plugin is not a process in the pane, so
    --- the pane's address and the launch it published go on the command line.
    --- The command takes the locks every writer of these records takes; this
    --- process writes none of them itself. Nil when it did not answer "ok",
    --- which is logged once per pane and action, and then whether the failure
    --- can pass: a lock wait that ran out, or a command that could not start
    --- or gave no answer. Any other diagnostic is a refusal, which stands
    --- until what the command reads changes.
    local function run_plugin_write(action, read, dir, extra)
      local root = M._active_integration_root
      local failure, passing = nil, false
      if not root or M._active_writer_installed ~= true then
        failure = "the attention command is not installed"
      elseif type(wezterm.run_child_process) ~= "function" then
        failure = "wezterm.run_child_process is unavailable"
      else
        local arguments = {
          "plugin", action,
          "--realm-id", read.address.realm_id,
          "--incarnation-id", read.address.incarnation_id,
          "--pane-id", read.address.pane_id,
          "--launch-id", read.launch_id,
        }
        for _, argument in ipairs(extra or {}) do arguments[#arguments + 1] = argument end
        local ok, success, stdout = pcall(wezterm.run_child_process,
          attention_argv(root, dir, arguments))
        local response = ok and type(stdout) == "string" and protocol
          and #stdout <= protocol.limits.max_json_bytes and protocol_api.decode_json(stdout) or nil
        if type(response) ~= "table" then response = nil end
        if ok and success and response and response.status == "ok"
            and type(response.result) == "table" then
          return response.result
        end
        local item = response and type(response.diagnostics) == "table" and response.diagnostics[1]
        if type(item) == "table" and type(item.code) == "string" then
          failure = item.code .. ": " .. tostring(item.message)
          passing = item.code == "probe_unavailable"
        elseif not ok then
          failure, passing = "it could not be started: " .. tostring(success), true
        elseif not success then
          -- A command built before the plugin's own subcommand existed exits
          -- with its usage text and prints nothing here.
          failure = "it gave no answer; the attention command in " .. root
            .. " may predate this plugin, so run scripts/install-cli.sh there"
          passing = true
        else
          failure, passing = "it gave no answer", true
        end
      end
      report_error_once("plugin-" .. action .. ":" .. read.cache_key,
        "attention plugin " .. action .. " failed for pane " .. read.marker_id .. ": " .. failure)
      return nil, passing
    end

    --- Does the pane carry the review its user set with the review key? Read
    --- from disk, never the cache, which can lag a flag another window's key
    --- press just wrote.
    local function user_review_present(read, dir)
      local owner_key = protocol_api.sha256("user")
      local record = read_expected_record(
        v2_pane_root(dir, read.address) .. "/reviews/" .. owner_key .. ".json",
        "review", { address = read.address }, true)
      return record ~= nil and record.owner_key == owner_key
    end

    --- Read one pane again after a write outside the poll, so the tab shows
    --- it now rather than on the next tick.
    local function refresh_cached_pane(read, dir, now_unix_ns)
      local view = read_attention_view(read, now_unix_ns or wezterm_now_unix_ns20(), {
        dir = dir, previous_view = attention_cache[read.cache_key],
      })
      attention_cache[read.cache_key] = view
      return view
    end

    --- The acknowledgement of each pane's latest activity event, by cache
    --- key: the event, and, after a run whose failure can pass, when to try
    --- again. A poll starts no second run for an event that is running, was
    --- answered or was refused, and retries a failure that can pass only
    --- after its backoff wait. An answer stands even when this reader still
    --- shows the event: asking again on every tick would change nothing. A
    --- newer event is tried afresh.
    local acknowledging = {}

    --- Acknowledge what the user is looking at: the activity `candidate`
    --- names, which this poll read for the active pane of the focused window.
    --- Both conditions matter: an activity acknowledged while its window is
    --- in the background is a notification the user never saw. The command
    --- writes the acknowledgement only while that activity is still the one
    --- the pane shows, so the check against a newer publication is made
    --- under the writers' locks rather than by reading again here.
    local function acknowledge_focused_pane(read, candidate, opts)
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
      -- Only what the tab shows is seen: when the review flag outranks the
      -- activity, the tab shows the flag, and the activity stays for later.
      if not (candidate and candidate.shown and acknowledge_set[candidate.shown]) then
        return "absent"
      end
      local state = acknowledging[read.cache_key]
      if state and state.event_id == candidate.event_id then
        if not state.retry_at or now_ms() < state.retry_at then return "pending" end
      else
        state = { event_id = candidate.event_id, retry_index = 1 }
        acknowledging[read.cache_key] = state
      end
      state.retry_at = nil
      local result, passing = run_plugin_write("acknowledge", read, dir,
        { "--activity-event-id", candidate.event_id })
      if not result then
        if passing then
          state.retry_at = now_ms() + backoff_delay(state.retry_index) * 1000
          state.retry_index = state.retry_index + 1
        end
        return "failed"
      end
      refresh_cached_pane(read, dir, opts and opts.now_unix_ns)
      if result.disposition == "ignored" then return "kept" end
      return "acknowledged"
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
    --- Forgetting it blanks what a still-open pane shows, so every way of not
    --- knowing answers no: the pane is still here, or nobody looked where it
    --- was, or the domain it lived on went unwatched, or something on that
    --- domain has not yet said which identity it carries.
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
        -- No publication read is an answer: there was nothing to dismiss. Saying
        -- so here keeps the contract in the decision rather than leaving the
        -- executor to notice that its expected value is missing.
        if not pane.event_id then return nil end
        return { event_id = pane.event_id, shown = pane.shown }
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

    --- The ids of `windows`, GUI or mux, as a set of decimal strings. A window
    --- whose id cannot be read is left out.
    local function window_keys(windows)
      local keys = {}
      for _, window in ipairs(windows) do
        local ok, id = pcall(window.window_id, window)
        if ok and id ~= nil then keys[tostring(id)] = true end
      end
      return keys
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
      return window_keys(inventory)
    end

    --- Mux windows that exist, by id as a decimal string, whether or not a GUI
    --- window shows them. Nil when the mux cannot be listed.
    local function mux_window_keys()
      local mux = wezterm.mux
      if not mux or type(mux.all_windows) ~= "function" then return nil end
      local ok, windows = pcall(mux.all_windows)
      if not ok or type(windows) ~= "table" then return nil end
      return window_keys(windows)
    end

    --- Windows that are still open. A workspace switch makes the GUI window
    --- show another workspace's mux window, so the one it showed leaves
    --- gui_windows() while it still exists, with its tabs, to be shown again.
    --- Only a window gone from both has closed. Without the mux listing it is
    --- the GUI inventory alone, so a window another workspace shows counts as
    --- closed. `live` is the GUI inventory the caller took; it is not changed.
    local function open_window_keys(live)
      if not live then return nil end
      local open = {}
      for key in pairs(live) do open[key] = true end
      for key in pairs(mux_window_keys() or {}) do open[key] = true end
      return open
    end

    local function prune_closed_publish_windows(current_window_key, opts, dir)
      local live = gui_window_keys(opts)
      if not live then return end
      -- The callback's window is authoritative even if WezTerm's inventory is
      -- between insertion and publication for a newly created GUI window.
      live[current_window_key] = true
      local open = open_window_keys(live)
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
      local delay = backoff_delay(schedule.retry_index)
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
      local socket = reader.unix_domain_socket(domain)
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
    --- the activity's `source` when it carried one; `reserved` is always
    --- false to retain tuple positions; `subagents` is how many of the pane's
    --- subagents are live, 0 when none; `review` is true when the pane carries
    --- a review flag.
    ---
    --- `type` is the effective one: it is `review` when the flag outranks the
    --- activity, and the activity's own type when that outranks the flag — in
    --- which case the flag is still reported by the sixth return.
    ---
    --- A pane with live subagents and no activity returns (nil, nil, nil, false, n):
    --- the count is real even though there is no type to report.
    --- A scalar observed at more than one full address returns nil. Use
    --- get_attention_view(pane) to disambiguate.
    function M.get_attention(marker_id)
      local mapped = cache_key_by_marker_id[tostring(marker_id)]
      local cached = mapped and attention_cache[mapped] or nil
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
        address = cached.address and protocol_api.deep_copy(cached.address) or nil,
        launch_id = cached.launch_id,
        marker_id = cached.marker_id,
        pane_presence = cached.pane_presence,
        binding_health = cached.binding_health,
        lifecycle = cached.lifecycle and protocol_api.deep_copy(cached.lifecycle) or nil,
      }
    end

    function M.get_attention_view(pane)
      local read = resolve_pane_read(pane)
      local cached = read.cache_key and attention_cache[read.cache_key] or nil
      if not cached then return nil end
      if read.kind == "claimed" and cached.launch_id ~= read.launch_id then return nil end
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
        losses[#losses + 1] = { kind = "scope_lost", window_id = id, previous_scope = protocol_api.deep_copy(state.scope) }
      end
      for key, read in pairs(entries) do
        local cached = attention_cache[key]
        local old = previous[key]
        if cached and cached.launch_id == read.launch_id then
          local target = cached._records and cached._records.selection_target
          local scope = target and { address = protocol_api.deep_copy(read.address), launch_id = read.launch_id, target = protocol_api.deep_copy(target) }
          if not scope and old and old.scope.launch_id == read.launch_id then scope = old.scope end
          if scope then
            local view = copy_public_view(cached)
            local replaced = old and not same_public_value(old.scope, scope)
            if replaced then lost(old, window:window_id()) end
            if not old or replaced or not same_public_value(old.view, view) then
              messages[#messages + 1] = { kind = (not old or replaced) and "initial" or "updated",
                window_id = window:window_id(), scope = protocol_api.deep_copy(scope), view = protocol_api.deep_copy(view) }
            end
            next_views[key] = { scope = protocol_api.deep_copy(scope), view = view }
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
      local live = open_window_keys(gui_window_keys(opts))
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

    --- Read the window's pane records, update the cache, acknowledge what the
    --- user is looking at, and ask WezTerm to redraw the tab bar if what it
    --- shows has changed. Call this from your own update-status handler if you
    --- set auto_poll = false; pass the handler's pane as opts.active_pane.
    ---
    --- Only refreshes entries for panes in the current window. Cross-window cache
    --- entries are left alone — pruning them here would cause cache thrash when
    --- multiple windows fire update-status (each window would wipe the other's
    --- entries every tick, producing visible tab-indicator blinking). A closed
    --- pane's entry is retired by the observed-then-gone check below.
    ---
    --- Acknowledging runs `attention plugin acknowledge`, once for each new
    --- activity the user looks at.
    ---
    --- The redraw exists because caching alone is not enough: WezTerm calls
    --- format-tab-title when something it knows about changes, and a record
    --- appearing on disk is not one of those things. Without the request below, a
    --- background pane's new state sat in the cache, unrendered, until the user
    --- happened to switch tabs — which is exactly when they no longer needed to be
    --- told.
    function M.poll(window, opts)
      if delivering_views then return end
      reader.refresh_domain_facts()
      local dir = (opts and opts.dir) or M._active_dir or defaults.dir
      local mux_win = window:mux_window()
      if not mux_win then return end
      local window_key = redraw_window_key(window)
      prune_closed_publish_windows(window_key, opts, dir)

      -- A tab that closes between this listing and its panes() call below is
      -- still in the list and already out of the mux, so panes() raises. That is
      -- a race with the user, not a fault.
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
      local saw_claimed = false
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

          if read.kind == "invalid" and not read.deferred then
            local item = read.diagnostic or diagnostic("record_invalid", "pane identity is invalid")
            report_error_once("v2-identity:" .. local_id .. ":" .. item.code,
              item.code .. ": " .. item.message)
          elseif read.kind == "claimed" then
            callback_entries[key] = read
            saw_claimed = true
            local now_unix_ns, utc_error = sample_utc_once()
            if utc_error then
              report_error_once("v2-clock:" .. local_id,
                utc_error .. ": WezTerm UTC is unavailable; TTL-bearing v2 state is omitted")
            end
            seen[key] = { domain = domain, kind = "claimed", marker_id = read.marker_id, local_id = local_id }
            evidence.panes[key] = { kind = "claimed", domain = domain,
              marker_id = read.marker_id, local_id = local_id }
            evidence.identified_by_local[local_id] = key
            pane_ids[#pane_ids + 1] = key
            before[key] = attention_cache[key]
            local read_opts = {
              dir = dir,
              glob = opts and opts.glob,
              previous_view = before[key],
              resample_utc = resample_utc,
              now_ms = now,
            }
            local view = read_attention_view(read, now_unix_ns, read_opts)
            -- A hook writes no frame, so a thinking view is animated from the
            -- wall clock.
            if view.type == "thinking" and view.frame == nil then
              view.frame = frame_for_now(now, frame_count)
            end
            attention_cache[key] = view
            evidence.panes[key].event_id = view.event_id
            -- The activity the tab shows for this pane, if it shows one: a
            -- review flag that outranks it hides it.
            evidence.panes[key].shown = view.type == view.activity_type and view.activity_type or nil
            for _, item in ipairs(view.diagnostics or {}) do
              report_error_once("v2:" .. key .. ":" .. item.code .. ":" .. item.message,
                item.code .. ": " .. item.message)
            end
            local boundary = view.next_wakeup_unix_ns
            if boundary and (not earliest_wakeup_unix_ns or boundary < earliest_wakeup_unix_ns) then
              earliest_wakeup_unix_ns = boundary
            end
          elseif read.kind == "unclaimed" then
            -- Published, and with nothing to show: no launch has claimed the
            -- pane. Its key still carries its settled title.
            seen[key] = { domain = domain, kind = "unclaimed", marker_id = read.marker_id,
              local_id = local_id }
            evidence.panes[key] = { kind = "unclaimed", domain = domain,
              marker_id = read.marker_id, local_id = local_id }
            evidence.identified_by_local[local_id] = key
            pane_ids[#pane_ids + 1] = key
            before[key] = attention_cache[key]
            attention_cache[key] = nil
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
      -- absence: a key this window reported on the previous poll and does not
      -- report now belonged to a pane that is gone, or to a pane that answers
      -- to another key now. Its cache entry is retired unless another window
      -- still shows it. Its records are the writer's, and stay.
      --
      -- A pane this window could not read, or a domain that detached while its
      -- panes run on in the server, is not known to be gone: decide_absence
      -- says so, and the key is remembered for a later tick to decide.
      local previously_seen = seen_marker_ids_by_window[window_key]
      if previously_seen then
        for gone_key, gone in pairs(previously_seen) do
          if not seen[gone_key] then
            local verdict = decide_absence(evidence, gone_key, gone, live_local_ids)
            if verdict == "unknown" then
              seen[gone_key] = gone
            elseif verdict ~= "present" then
              pane_ids[#pane_ids + 1] = gone_key
              before[gone_key] = attention_cache[gone_key]
              if not observed_in_other_window(gone_key, window_key) then
                attention_cache[gone_key] = nil
              end
            end
          end
        end
      end
      seen_marker_ids_by_window[window_key] = seen
      -- A scalar cannot select one of several realms. Build this projection
      -- from all observed windows, rather than letting poll/overlay order win.
      rebuild_scalar_projection()

      if saw_claimed then
        schedule_ttl_wakeup(window, earliest_wakeup_unix_ns, poll_now_unix_ns, opts)
      else
        schedule_ttl_wakeup(window, nil, nil, opts)
      end

      -- Everything below is about the focused window only. An unfocused window
      -- must neither acknowledge an activity its user has not seen nor be sent a key
      -- action, so an unfocused poll ends here with the cache correct.
      if not window:is_focused() then
        deliver_window_views(window, callback_entries, opts, evidence.gap)
        return
      end

      local current_active_pane = window:active_pane()
      if current_active_pane then
        local active_read = resolve_pane_read(current_active_pane)
        local candidate = decide_acknowledgement(evidence, active_read)
        if candidate then
          -- The activity this poll read, so what is dismissed is what this
          -- poll saw, not a display cache another window's poll may have
          -- replaced in between.
          acknowledge_focused_pane(active_read, candidate, {
            dir = dir, now_unix_ns = poll_now_unix_ns,
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

    return {
      tab_source = function() return tab_source_state.source end,
      tab_source_status = tab_source_status,
      own_mux_identity = own_mux_identity,
      reset_tab_source = reset_tab_source,
      acquire_tab_source = acquire_tab_source,
      parse_tab_source_response = parse_tab_source_response,
      same_cached_attention = same_cached_attention,
      observe_pane = observe_pane,
      tab_panes_containing_read = tab_panes_containing_read,
      run_plugin_write = run_plugin_write,
      user_review_present = user_review_present,
      refresh_cached_pane = refresh_cached_pane,
      request_tab_bar_redraw = request_tab_bar_redraw,
    }
  end

  return {
    attention_cache = attention_cache,
    drawn_pane_key = drawn_pane_key,
    bind = bind,
  }
end
