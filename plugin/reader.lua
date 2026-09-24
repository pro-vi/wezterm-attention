return function(context)
  local M = context.M
  local protocol = context.protocol
  local protocol_load_error = context.protocol_load_error
  local parse_wire_json = context.parse_wire_json
  local address_cache_key = context.address_cache_key
  local same_address = context.same_address
  local diagnostic = context.diagnostic
  local invalid = context.invalid
  local diagnostics_have_unavailable_io = context.diagnostics_have_unavailable_io
  local health_from_diagnostics = context.health_from_diagnostics
  local collect_diagnostic = context.collect_diagnostic
  local v2_pane_root = context.v2_pane_root
  local binding_root = context.binding_root
  local read_expected_record_cached = context.read_expected_record_cached
  local read_record_collection = context.read_record_collection
  local identity_diagnostic = context.identity_diagnostic
  local same_target
  local age_exceeds_ms = context.age_exceeds_ms
  local eligible_subagent = context.eligible_subagent
  local deep_copy = context.deep_copy
  local defaults = context.defaults
  local wezterm = context.wezterm

  local function request_evidence(observations, provider)
    local groups, ordered = {}, {}
    for _, item in ipairs(observations) do
      local kind = item.kind
      local question_mode = item.question_mode
      local request_kind, role, namespace, native_id
      local c = item.correlation or {}
      if (kind == "tool_preflight" or kind == "tool_result") and item.tool_class ~= "generic" then
        request_kind = item.tool_class
        role = kind == "tool_preflight" and "request" or (item.question_mode == "nonblocking" and "publication" or "result")
        namespace, native_id = "tool", c.tool_call_id
      elseif kind == "approval_requested" or kind == "automatic_denial" then
        request_kind, role = "approval", kind == "approval_requested" and "request" or "denial"
        if kind == "automatic_denial" then
          local class, mode = context.classify_lifecycle_tool(provider, item.tool_name)
          if class ~= "generic" then request_kind, question_mode = class, mode end
        end
        namespace, native_id = "tool", c.tool_call_id
      elseif kind == "elicitation_requested" or kind == "elicitation_action_selected" then
        request_kind, role = "elicitation", kind == "elicitation_requested" and "request" or "selection"
        namespace, native_id = "elicitation", c.elicitation_id
      elseif kind == "notice" then
        request_kind, role = "notice", "request"
      end
      if request_kind then
        if not native_id then namespace = "observation_id" end
        local pieces = {}
        for _, value in ipairs({ request_kind, item.actor.kind, item.actor.agent_id or "", namespace or "", native_id or item.observation_id, c.turn_id or "", c.mcp_server_name or "", item.tool_name or "", question_mode or "" }) do
          pieces[#pieces + 1] = #value .. ":" .. value
        end
        local key = table.concat(pieces)
        local group = groups[key]
        if not group then
          group = { kind = request_kind, actor = deep_copy(item.actor), correlation = deep_copy(item.correlation), tool_name = item.tool_name,
            request_observation_ids = {}, result_observation_ids = {}, selection_observation_ids = {}, denial_observation_ids = {}, relations = {} }
          if request_kind == "question" then group.question_mode = question_mode; group.publication_observation_ids = {} end
          groups[key] = group; ordered[#ordered + 1] = group
        end
        local names = { request = "request_observation_ids", result = "result_observation_ids", publication = "publication_observation_ids", selection = "selection_observation_ids", denial = "denial_observation_ids" }
        local ids = group[names[role]]
        ids[#ids + 1] = item.observation_id
      end
    end
    for _, group in ipairs(ordered) do
      if #group.request_observation_ids > 0 then
        for field, kind in pairs({ result_observation_ids = "tool_result_observed", selection_observation_ids = "elicitation_action_selected", denial_observation_ids = "automatic_denial_observed" }) do
          for _, id in ipairs(group[field]) do group.relations[#group.relations + 1] = { kind = kind, observation_id = id } end
        end
        table.sort(group.relations, function(a,b) return a.kind .. a.observation_id < b.kind .. b.observation_id end)
      end
    end
    return ordered
  end

  local latest_written = setmetatable({}, { __mode = "k" })

  --- The latest write time of any observation in a snapshot, remembered per
  --- snapshot table.
  local function latest_written_unix_ns(snapshot)
    local latest = latest_written[snapshot]
    if latest then return latest end
    latest = ""
    for _, pool in pairs(snapshot.pools) do
      for _, observation in ipairs(pool.observations) do
        if observation.written_at_unix_ns > latest then latest = observation.written_at_unix_ns end
      end
    end
    latest_written[snapshot] = latest
    return latest
  end

  local function lifecycle_facet(snapshot, status, problem, now_unix_ns)
    local availability = { valid = "available", missing = "absent", cached = "cached", unavailable = "unavailable", invalid = "invalid" }
    local facet = { availability = availability[status] or "absent", coverage = "bounded_window", observations = {}, requests = {}, retention_floors = {}, diagnostics = {} }
    if problem then
      facet.diagnostics[1] = problem
      if problem.code == "future_schema" then facet.availability = "unsupported" end
    end
    if not snapshot then return facet end
    facet.snapshot_id = snapshot.snapshot_id
    for name, pool in pairs(snapshot.pools) do
      facet.retention_floors[name] = pool.retention_floor_mono_ns
      for _, observation in ipairs(pool.observations) do
        local copy = deep_copy(observation)
        copy.pool = name
        facet.observations[#facet.observations + 1] = copy
        if now_unix_ns and now_unix_ns < observation.written_at_unix_ns and #facet.diagnostics < 8 then
          facet.diagnostics[#facet.diagnostics + 1] = diagnostic("clock_skew", "lifecycle write time is ahead of UTC")
        end
      end
    end
    table.sort(facet.observations, function(a,b)
      return a.observed_mono_ns .. a.observation_id < b.observed_mono_ns .. b.observation_id
    end)
    facet.requests = request_evidence(facet.observations, snapshot.provider)
    return facet
  end

  -- ── Pane identity ───────────────────────────────────────────────────────────
  -- A marker file is named by the pane id a process reads from its own
  -- $WEZTERM_PANE. A GUI attached to a mux server over a unix domain gives its
  -- client panes fresh local ids, so pane:pane_id() there names a different pane
  -- than the writer did, and every marker read or write through it addresses the
  -- wrong file. The fix is a published id: the shell (and any hook) emits its
  -- $WEZTERM_PANE as the WEZTERM_PANE user var via OSC 1337 SetUserVar, and
  -- pane:get_user_vars() returns it for local and mux-client panes alike.

  local function canonical_pane_id(value)
    if type(value) ~= "string" then return nil end
    local max_digits = protocol and protocol.limits.pane_id_max_digits or 20
    if #value > max_digits or not value:match("^%d+$") then return nil end
    if #value > 1 and value:sub(1, 1) == "0" then return nil end
    return value
  end

  local function pane_call(pane, name)
    local fn = pane and pane[name]
    if type(fn) ~= "function" then return false, nil end
    local ok, result = pcall(fn, pane)
    if not ok then return false, nil end
    return true, result
  end

  local function pane_method(pane, name)
    local ok, result = pane_call(pane, name)
    if not ok then return nil end
    return result
  end

  -- ── Domains ─────────────────────────────────────────────────────────────────
  -- WezTerm runs the "local" domain, every exec domain, every WSL domain and
  -- every serial port as a LocalDomain: this GUI's own mux numbers the pane, and
  -- a process inside reads that same number from $WEZTERM_PANE. There,
  -- pane_id() is the truth. Every other domain is a client of some other mux,
  -- whose panes this GUI renumbered, so only what the pane published says which
  -- pane it is.
  --
  -- The domain lists are read from the config table when first needed after it
  -- was handed to apply_to_config, not while apply_to_config runs: a config
  -- that sets unix_domains or exec_domains after calling it is the same config.

  local function trimmed_directory(path)
    return (path:gsub("(.)/+$", "%1"))
  end

  --- Where WezTerm puts a unix domain's socket when the domain names none:
  --- RUNTIME_DIR/sock, RUNTIME_DIR being $XDG_RUNTIME_DIR/wezterm where the
  --- platform has a runtime directory (not macOS or Windows) and
  --- ~/.local/share/wezterm otherwise. Nil on Windows, where no attention
  --- writer runs.
  local function default_unix_socket()
    local triple = type(wezterm.target_triple) == "string" and wezterm.target_triple or ""
    if triple:find("windows", 1, true) then return nil end
    if not triple:find("darwin", 1, true) then
      local runtime = os.getenv("XDG_RUNTIME_DIR")
      if runtime and runtime:sub(1, 1) == "/" then
        return trimmed_directory(runtime) .. "/wezterm/sock"
      end
    end
    local home = context.home_dir
    if type(home) ~= "string" or home:sub(1, 1) ~= "/" then return nil end
    return trimmed_directory(home) .. "/.local/share/wezterm/sock"
  end

  local function names_of(list, into)
    if type(list) ~= "table" then return end
    for _, domain in ipairs(list) do
      if type(domain) == "table" and type(domain.name) == "string" then
        into[domain.name] = domain
      end
    end
  end

  local domain_facts
  local function current_domain_facts()
    if domain_facts then return domain_facts end
    local facts = { locals = { ["local"] = true }, wsl_defaults = false, sockets = {} }
    local config = M._active_config
    local ok = type(config) ~= "table" or pcall(function()
      local named = {}
      names_of(config.exec_domains, named)
      names_of(config.serial_ports, named)
      -- Without a wsl_domains list WezTerm makes one "WSL:<distro>" domain per
      -- installed distribution; with one, the list is all there is.
      if type(config.wsl_domains) == "table" then
        names_of(config.wsl_domains, named)
      else
        facts.wsl_defaults = true
      end
      for name in pairs(named) do facts.locals[name] = true end
      -- An unset unix_domains is WezTerm's one implicit domain, "unix".
      local unix_domains = config.unix_domains
      if unix_domains == nil then unix_domains = { { name = "unix" } } end
      local listed = {}
      names_of(unix_domains, listed)
      for name, domain in pairs(listed) do
        -- A proxied domain reaches its mux through a command, often on another
        -- host; the socket on this machine, if any, is a different mux.
        if domain.proxy_command == nil then
          if domain.socket_path == nil then
            facts.sockets[name] = default_unix_socket()
          elseif type(domain.socket_path) == "string" and domain.socket_path:sub(1, 1) == "/" then
            facts.sockets[name] = domain.socket_path
          end
        end
      end
    end)
    if not ok then facts = { locals = { ["local"] = true }, wsl_defaults = false, sockets = {} } end
    domain_facts = facts
    return facts
  end

  --- Forget what was read from the config, so the next question reads it again.
  local function refresh_domain_facts()
    domain_facts = nil
  end

  local function is_local_domain(name)
    if type(name) ~= "string" then return false end
    local facts = current_domain_facts()
    return facts.locals[name] == true or (facts.wsl_defaults and name:sub(1, 4) == "WSL:")
  end

  --- The socket of the mux behind a unix domain, or nil when there is none on
  --- this machine to publish through.
  local function unix_domain_socket(name)
    return current_domain_facts().sockets[name]
  end

  local function resolve_pane_read(pane)
    if not pane then
      return { kind = "invalid", diagnostic = invalid("pane is unavailable") }
    end
    local vars_ok, vars = pane_call(pane, "get_user_vars")
    if not vars_ok then
      return {
        kind = "invalid",
        diagnostic = diagnostic("probe_unavailable", "pane user variables are unavailable"),
      }
    end
    local domain = pane_method(pane, "get_domain_name")
    -- User vars are set by whatever the pane prints, and a local pane can print
    -- another pane's identity: a catted file, the prompt of a shell on another
    -- host. In a local domain the id is known, so a var naming a different pane
    -- is not this pane's.
    local local_id = is_local_domain(domain)
      and canonical_pane_id(tostring(pane_method(pane, "pane_id"))) or nil
    if type(vars) == "table" and vars.WEZTERM_ATTENTION ~= nil then
      local wire, wire_diagnostic = parse_wire_json(vars.WEZTERM_ATTENTION)
      if not wire then return { kind = "invalid", diagnostic = wire_diagnostic } end
      if local_id and wire.address.pane_id ~= local_id then
        return { kind = "invalid", diagnostic = invalid(
          "WEZTERM_ATTENTION names pane " .. wire.address.pane_id .. ", not this local pane",
          { pane_id = local_id }) }
      end
      return {
        kind = "v2",
        address = wire.address,
        launch_id = wire.launch_id,
        marker_id = wire.address.pane_id,
        cache_key = address_cache_key(wire.address),
      }
    end

    if local_id then return { kind = "v1", marker_id = local_id, cache_key = local_id } end
    local published = type(vars) == "table" and canonical_pane_id(vars.WEZTERM_PANE) or nil
    if published then
      return { kind = "v1", marker_id = published, cache_key = published }
    end
    return { kind = "unpublished", domain = domain or "?" }
  end

  local function same_target(left, right)
    if not left or not right or left.kind ~= right.kind then return false end
    if left.kind == "binding" then return left.binding_id == right.binding_id end
    return left.kind == "launch"
  end

  local function effective_attention_type(activity_type, review)
    if not review then return activity_type end
    local priority = M._active_priority_map or { thinking = 1, review = 2, stop = 3, notify = 4 }
    if not activity_type or (priority.review or 0) > (priority[activity_type] or 0) then
      return "review"
    end
    return activity_type
  end

  local function empty_v2_view(read, diagnostics, records)
    local unavailable = diagnostics_have_unavailable_io(diagnostics)
    records = records or {}
    return {
      address = read.address,
      launch_id = read.launch_id,
      marker_id = read.marker_id,
      cache_key = read.cache_key,
      binding_id = records.pointer and records.pointer.binding_id or nil,
      type = nil,
      activity_type = nil,
      frame = nil,
      source = nil,
      subagents = 0,
      review = false,
      binding_phase = nil,
      pane_presence = unavailable and "unavailable" or "present",
      reader_confidence = "unconfirmed",
      binding_health = health_from_diagnostics(diagnostics),
      diagnostics = diagnostics,
      _records = records,
    }
  end

  local function read_view_at(read, now_unix_ns, opts)
    local diagnostics = {}
    local previous = opts and opts.previous_view or nil
    local previous_records = {}
    if previous and previous.launch_id == read.launch_id
        and same_address(previous.address, read.address) then
      previous_records = previous._records or {}
    end
    local records = { reviews = {}, presences = {} }
    if not protocol then
      collect_diagnostic(diagnostics, diagnostic(
        "probe_unavailable", "v2 protocol manifest is unavailable",
        { detail = tostring(protocol_load_error) }))
      return empty_v2_view(read, diagnostics, records)
    end

    local dir = (opts and opts.dir) or M._active_dir or defaults.dir
    local address = read.address
    local pane_root = v2_pane_root(dir, address)
    local realm_path = dir .. "/v2/realms/" .. address.realm_id .. "/realm.json"
    local incarnation_path = dir .. "/v2/realms/" .. address.realm_id
      .. "/incarnations/" .. address.incarnation_id .. "/incarnation.json"
    local claim_path = pane_root .. "/claim.json"

    local realm, realm_diagnostic = read_expected_record_cached(
      realm_path, "realm", { realm_id = address.realm_id }, true, previous_records.realm)
    records.realm = realm
    collect_diagnostic(diagnostics, realm_diagnostic)
    local incarnation, incarnation_diagnostic = read_expected_record_cached(
      incarnation_path, "incarnation", {
        realm_id = address.realm_id, incarnation_id = address.incarnation_id,
      }, true, previous_records.incarnation)
    records.incarnation = incarnation
    collect_diagnostic(diagnostics, incarnation_diagnostic)
    local claim, claim_diagnostic = read_expected_record_cached(claim_path, "claim", {
      address = address, launch_id = read.launch_id,
    }, true, previous_records.claim)
    records.claim = claim
    collect_diagnostic(diagnostics, claim_diagnostic)
    if not realm or not incarnation or not claim then
      return empty_v2_view(read, diagnostics, records)
    end

    local launch_root = pane_root .. "/launches/" .. read.launch_id
    local pointer_path = launch_root .. "/current-binding.json"
    local pointer, pointer_diagnostic = read_expected_record_cached(
      pointer_path, "current_binding", {
      address = address, launch_id = read.launch_id,
    }, false, previous_records.pointer)
    records.pointer = pointer
    collect_diagnostic(diagnostics, pointer_diagnostic)
    if pointer_diagnostic and not pointer then return empty_v2_view(read, diagnostics, records) end

    local binding
    local activity
    local activity_clear
    local binding_end
    local acknowledgement
    local clear
    local floor
    local subagent_fences_valid = true
    local activity_fence_valid = true
    local current_target = { kind = "launch" }
    local records_root = launch_root
    local previous_binding_records = {}
    if not pointer_diagnostic then
      records.selection_target = pointer and { kind = "binding", binding_id = pointer.binding_id } or { kind = "launch" }
    end
    if pointer and previous_records.pointer
        and previous_records.pointer.binding_id == pointer.binding_id then
      previous_binding_records = previous_records
    end
    if pointer then
      current_target = { kind = "binding", binding_id = pointer.binding_id }
      records_root = binding_root(dir, address, read.launch_id, pointer.binding_id)
      local binding_diagnostic
      binding, binding_diagnostic = read_expected_record_cached(
        records_root .. "/binding.json", "binding", {
        address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
      }, true, previous_binding_records.binding)
      records.binding = binding
      collect_diagnostic(diagnostics, binding_diagnostic)
      if not binding then return empty_v2_view(read, diagnostics, records) end

      local end_diagnostic
      binding_end, end_diagnostic = read_expected_record_cached(
        records_root .. "/end.json", "binding_end", {
        address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
      }, false, previous_binding_records.binding_end)
      records.binding_end = binding_end
      collect_diagnostic(diagnostics, end_diagnostic)
      local activity_clear_diagnostic
      activity_clear, activity_clear_diagnostic = read_expected_record_cached(
        records_root .. "/activity-clear.json", "activity_clear", {
          address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
        }, false, previous_binding_records.activity_clear)
      records.activity_clear = activity_clear
      collect_diagnostic(diagnostics, activity_clear_diagnostic)
      if activity_clear_diagnostic and not activity_clear then activity_fence_valid = false end
      local clear_diagnostic
      clear, clear_diagnostic = read_expected_record_cached(
        records_root .. "/agents-clear.json", "subagent_clear", {
          address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
        }, false, previous_binding_records.clear)
      records.clear = clear
      collect_diagnostic(diagnostics, clear_diagnostic)
      if clear_diagnostic and not clear then subagent_fences_valid = false end
      local floor_diagnostic
      floor, floor_diagnostic = read_expected_record_cached(
        records_root .. "/agents-floor.json", "subagent_retention_floor", {
          address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
        }, false, previous_binding_records.floor)
      records.floor = floor
      collect_diagnostic(diagnostics, floor_diagnostic)
      if floor_diagnostic and not floor then subagent_fences_valid = false end
    end

    local previous_selected_records = pointer and previous_binding_records or previous_records
    local activity_diagnostic
    activity, activity_diagnostic = read_expected_record_cached(
      records_root .. "/activity.json", "activity", {
        address = address, launch_id = read.launch_id,
      }, false, previous_selected_records.activity)
    records.activity = activity
    collect_diagnostic(diagnostics, activity_diagnostic)
    if activity and not same_target(activity.target, current_target) then
      collect_diagnostic(diagnostics, identity_diagnostic("activity", records_root .. "/activity.json"))
      activity = nil
      records.activity = nil
    end
    if not activity_fence_valid then
      activity = nil
    elseif activity and activity_clear
        and activity.observed_mono_ns <= activity_clear.observed_mono_ns then
      activity = nil
    end

    local ack_diagnostic
    acknowledgement, ack_diagnostic = read_expected_record_cached(
      records_root .. "/ack.json", "acknowledgement", {
        address = address, launch_id = read.launch_id,
      }, false, previous_selected_records.acknowledgement)
    records.acknowledgement = acknowledgement
    collect_diagnostic(diagnostics, ack_diagnostic)

    local activity_expiry_unix_ns
    if activity and activity.ttl_ms then
      local expired, age_error, boundary =
        age_exceeds_ms(now_unix_ns, activity.written_at_unix_ns, activity.ttl_ms)
      if age_error then
        collect_diagnostic(diagnostics, diagnostic(
          age_error, "activity wall age is not trustworthy", { event_id = activity.event_id }))
        activity = nil
      elseif expired then
        activity = nil
      elseif boundary then
        activity_expiry_unix_ns = boundary
      end
    end

    if activity and acknowledgement then
      if not same_target(acknowledgement.target, activity.target) then
        collect_diagnostic(diagnostics, identity_diagnostic(
          "acknowledgement", records_root .. "/ack.json"))
      elseif acknowledgement.activity_event_id == activity.event_id then
        activity = nil
      end
    end

    local review = false
    local reviews, review_diagnostics = read_record_collection(
      pane_root .. "/reviews/*.json", "review", { address = address },
      previous_records.reviews, "owner_key", opts)
    records.reviews = reviews
    for _, item in ipairs(review_diagnostics) do collect_diagnostic(diagnostics, item) end
    for _ in pairs(reviews) do
      review = true
      break
    end

    local subagents = 0
    local next_wakeup_unix_ns = activity and activity_expiry_unix_ns or nil
    if pointer and subagent_fences_valid then
      local presences, presence_diagnostics = read_record_collection(
        records_root .. "/agents/*.json", "subagent_presence", {
          address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
        }, previous_binding_records.presences, "agent_key", opts)
      records.presences = presences
      for _, item in ipairs(presence_diagnostics) do collect_diagnostic(diagnostics, item) end
      for path, record in pairs(presences) do
        local record_diagnostic
        if record and binding and record.provider ~= binding.provider then
          record, record_diagnostic = nil, invalid(
            "subagent provider does not match its binding", { path = path })
          records.presences[path] = nil
        end
        collect_diagnostic(diagnostics, record_diagnostic)
        if record then
          local eligible, eligibility_diagnostic, wakeup_boundary =
            eligible_subagent(record, clear, floor, now_unix_ns)
          collect_diagnostic(diagnostics, eligibility_diagnostic)
          if eligible then
            subagents = subagents + 1
            if wakeup_boundary
                and (not next_wakeup_unix_ns or wakeup_boundary < next_wakeup_unix_ns) then
              next_wakeup_unix_ns = wakeup_boundary
            end
          end
        end
      end
    end

    local activity_type = activity and activity.type or nil
    local effective_type = effective_attention_type(activity_type, review)
    local unavailable = diagnostics_have_unavailable_io(diagnostics)
    local snapshot, snapshot_problem, snapshot_status
    if binding then
      snapshot, snapshot_problem, snapshot_status = read_expected_record_cached(
        records_root .. "/lifecycle.json", "lifecycle_snapshot", {
          address = address, launch_id = read.launch_id, binding_id = pointer.binding_id,
        }, false, previous_binding_records.lifecycle)
      if snapshot and snapshot.provider ~= binding.provider then
        snapshot, snapshot_problem, snapshot_status = nil, invalid("lifecycle provider differs from its binding"), "invalid"
      end
    end
    records.lifecycle = snapshot
    -- The record reader hands back the previous snapshot table when the file's
    -- bytes have not changed, and then the facet built from it last poll is
    -- still the facet: copying and grouping every observation again would give
    -- the same tables. Only the clock can change it, through the clock-skew
    -- diagnostics, so it is reused only while neither poll saw any.
    local lifecycle
    local previous_lifecycle = previous and previous.lifecycle
    if snapshot and snapshot_status == "valid" and previous_lifecycle
        and previous_binding_records.lifecycle == snapshot
        and previous_lifecycle.availability == "available"
        and #previous_lifecycle.diagnostics == 0
        and not (now_unix_ns and now_unix_ns < latest_written_unix_ns(snapshot)) then
      lifecycle = previous_lifecycle
      lifecycle.badge_acknowledgement = nil
    else
      lifecycle = lifecycle_facet(snapshot, snapshot_status, snapshot_problem, now_unix_ns)
    end
    local raw_activity = records.activity
    if acknowledgement and raw_activity and same_target(acknowledgement.target, current_target)
      and same_target(raw_activity.target, current_target)
      and acknowledgement.activity_event_id == raw_activity.event_id then
      lifecycle.badge_acknowledgement = {
        activity_event_id = acknowledgement.activity_event_id,
        event_id = acknowledgement.event_id,
        target = deep_copy(acknowledgement.target),
      }
    end

    return {
      address = address,
      launch_id = read.launch_id,
      marker_id = read.marker_id,
      cache_key = read.cache_key,
      binding_id = pointer and pointer.binding_id or nil,
      provider = binding and binding.provider or nil,
      event_id = activity and activity.event_id or nil,
      lifecycle = lifecycle,
      type = effective_type,
      activity_type = activity_type,
      frame = effective_type == activity_type and activity and activity.frame or nil,
      source = activity and activity.source or nil,
      subagents = subagents,
      review = review,
      binding_phase = binding_end and binding
        and binding_end.observed_mono_ns >= binding.observed_mono_ns
        and "ended" or (binding and "active" or nil),
      pane_presence = unavailable and "unavailable" or "present",
      reader_confidence = unavailable and "unconfirmed" or "confirmed",
      binding_health = health_from_diagnostics(diagnostics),
      diagnostics = diagnostics,
      next_wakeup_unix_ns = next_wakeup_unix_ns,
      _records = records,
    }
  end

  local function saw_clock_skew(view)
    for _, list in ipairs({ view.diagnostics, view.lifecycle and view.lifecycle.diagnostics }) do
      for _, item in ipairs(list or {}) do
        if item.code == "clock_skew" then return true end
      end
    end
    return false
  end

  --- Read a pane's view as of `now_unix_ns`. A poll samples UTC once and then
  --- reads its panes, so a record written after the sample and before its
  --- read carries a write time ahead of the sample, which reads as a clock
  --- that is ahead. `opts.resample_utc`, when given, is asked once for the time
  --- now; a later time means the record was simply newer than the sample, and
  --- the pane is read again as of that time.
  local function read_attention_view(read, now_unix_ns, opts)
    local view = read_view_at(read, now_unix_ns, opts)
    local resample = opts and opts.resample_utc
    if not resample or not now_unix_ns or not saw_clock_skew(view) then return view end
    local later = resample()
    if type(later) ~= "string" or later <= now_unix_ns then return view end
    local again = {}
    for key, value in pairs(opts) do again[key] = value end
    again.resample_utc = nil
    return read_view_at(read, later, again)
  end

  --- The id under which this pane's markers are written, or nil when the pane
  --- has published nothing and its local id cannot be trusted to name them.
  function M.pane_marker_id(pane)
    local read = resolve_pane_read(pane)
    if read.kind == "v1" or read.kind == "v2" then return read.marker_id end
    return nil
  end


  return {
    lifecycle_facet = lifecycle_facet,
    is_local_domain = is_local_domain,
    unix_domain_socket = unix_domain_socket,
    refresh_domain_facts = refresh_domain_facts,
    canonical_pane_id = canonical_pane_id,
    pane_call = pane_call,
    pane_method = pane_method,
    resolve_pane_read = resolve_pane_read,
    same_target = same_target,
    effective_attention_type = effective_attention_type,
    empty_v2_view = empty_v2_view,
    read_attention_view = read_attention_view,
    pane_marker_id = M.pane_marker_id,
  }
end
