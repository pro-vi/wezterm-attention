return function(context)
  local wezterm = context.wezterm
  local valid_types = context.valid_types
  local normalize_epoch_ms = context.normalize_epoch_ms
  local protocol = context.protocol

  -- ── Marker I/O ──────────────────────────────────────────────────────────────

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
      local publication_id = type(data.publication_id) == "string"
        and data.publication_id ~= "" and data.publication_id or nil
      local source = type(data.source) == "string" and data.source ~= "" and data.source or nil
      return data.type, data.frame, normalize_epoch_ms(data.updated_at or data.updated_at_ms),
        data.ttl_ms, content, publication_id, source, data.puppet == true
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
    subagent_live_ms = subagent_live_ms,
    subagents_path = subagents_path,
    count_live_subagents = count_live_subagents,
  }
end
