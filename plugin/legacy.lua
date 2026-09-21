return function(context)
  local wezterm = context.wezterm
  local valid_types = context.valid_types
  local normalize_epoch_ms = context.normalize_epoch_ms
  local protocol = context.protocol
  local incarnation_socket_ctime_ns = context.incarnation_socket_ctime_ns
  local unix_ns20_from_epoch_ms = context.unix_ns20_from_epoch_ms
  local compare_ns20 = context.compare_ns20

  -- ── Marker I/O ──────────────────────────────────────────────────────────────

  --- Was this marker written before the mux that issues pane ids now existed?
  --- Then it describes a pane that is already gone, and the only reason it is
  --- being read at all is that some later pane inherited the id its filename is
  --- made of. A v2 record would have said which mux it belonged to; a bare-id
  --- flat file has nowhere to put that, so its own timestamp is the only evidence
  --- available, and the socket is the boundary it is measured against.
  ---
  --- Undated markers are exempt. A marker with no timestamp is one a writer may
  --- still mean, and inventing a date for it is the single way this test could
  --- discard live state rather than stale state.
  local function predates_this_mux(updated_at_ms)
    if not updated_at_ms or not incarnation_socket_ctime_ns then return false end
    if not unix_ns20_from_epoch_ms or not compare_ns20 then return false end
    local socket_ctime_ns = incarnation_socket_ctime_ns()
    if not socket_ctime_ns then return false end
    local written_at = unix_ns20_from_epoch_ms(updated_at_ms)
    if not written_at then return false end
    local order = compare_ns20(written_at, socket_ctime_ns)
    return order ~= nil and order < 0
  end

  local function read_marker(dir, pane_id)
    local f = io.open(dir .. "/" .. pane_id, "r")
    if not f then return nil end
    local content = f:read("*a")
    f:close()

    -- Try JSON first: {"type":"stop","frame":2}
    local ok, data = pcall(function()
      return wezterm.json_parse(content)
    end)
    if ok and data and valid_types[data.type] then
      local updated_at = normalize_epoch_ms(data.updated_at or data.updated_at_ms)
      local publication_id = type(data.publication_id) == "string"
        and data.publication_id ~= "" and data.publication_id or nil
      local source = type(data.source) == "string" and data.source ~= "" and data.source or nil
      return data.type, data.frame, updated_at,
        data.ttl_ms, content, publication_id, source
    end

    -- Fallback: plain text (backward compat)
    local text = content:gsub("%s+", "")
    if valid_types[text] then return text, nil, nil, nil, content, nil, nil, false end
    return nil
  end

  -- ── Subagent activity sidecar ───────────────────────────────────────────────
  -- Beside the marker file, a hook writer maintains "<marker id>.agents":
  --   {"agents":{"<agent id>":{"type":"<string>","last_ms":<epoch ms>}}}
  -- one entry per subagent that has run a tool call from that pane. An entry is
  -- live for ten minutes after its last_ms.
  --
  -- The file is independent of the marker. A pane can carry live subagents with
  -- no marker at all (the parent stopped and its check mark was acknowledged) or
  -- beside a marker of any type, so it is read for every pane, not only for panes
  -- whose marker parsed.

  local subagent_live_ms = protocol and protocol.limits
    and protocol.limits.subagent_ttl_ms or (10 * 60 * 1000)

  local function subagents_path(dir, pane_id)
    return dir .. "/" .. pane_id .. ".agents"
  end

  --- How many of this pane's subagents ran a tool call recently. Absent, empty
  --- and unparseable files all count zero: the sidecar is an addition to the
  --- marker protocol, and a writer that corrupts it must not be able to disturb
  --- the marker it sits beside.
  local function count_live_subagents(dir, pane_id, now)
    local file = io.open(subagents_path(dir, pane_id), "r")
    if not file then return 0 end
    local content = file:read("*a")
    file:close()
    if not content or content == "" then return 0 end

    local ok, data = pcall(function()
      return wezterm.json_parse(content)
    end)
    if not ok or type(data) ~= "table" or type(data.agents) ~= "table" then return 0 end

    local live = 0
    for _, entry in pairs(data.agents) do
      if type(entry) == "table" then
        local last_ms = normalize_epoch_ms(entry.last_ms)
        if last_ms and now - last_ms <= subagent_live_ms then live = live + 1 end
      end
    end
    return live
  end


  return {
    read_marker = read_marker,
    predates_this_mux = predates_this_mux,
    subagent_live_ms = subagent_live_ms,
    subagents_path = subagents_path,
    count_live_subagents = count_live_subagents,
  }
end
