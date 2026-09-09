-- Adapted copy of bootstrap's detect_agent consumer. The v2 cached view is
-- authoritative; process and title checks remain only as legacy fallback.
return function(attention, agent_relay)
  local valid = { claude = true, codex = true, pi = true }

  local function basename(path)
    if not path or path == "" then return "" end
    return path:match("([^/\\]+)$") or path
  end

  return function(pane, title)
    if attention and attention.get_attention_view then
      local ok, view = pcall(attention.get_attention_view, pane)
      if ok and type(view) == "table" and valid[view.provider] then
        return view.provider
      end
    end

    local ok, proc_path = pcall(function() return pane:get_foreground_process_name() end)
    local proc = ok and basename(proc_path) or ""
    if proc:find("codex", 1, true) then return "codex" end
    if agent_relay and agent_relay.detect_process_agent then
      local ok_info, process_info = pcall(function() return pane:get_foreground_process_info() end)
      if ok_info then
        local process_agent = agent_relay.detect_process_agent(proc_path, process_info)
        if process_agent then return process_agent end
      end
    end

    title = title or ""
    if title:sub(1, 2) == "\xcf\x80" then return "pi" end
    if title:sub(1, 3) == "\xe2\x9c\xb3" then return "claude" end
    local prefix2 = title:sub(1, 2)
    if prefix2 == "\xe2\xa0" or prefix2 == "\xe2\xa1"
        or prefix2 == "\xe2\xa2" or prefix2 == "\xe2\xa3" then
      return "claude"
    end
    return nil
  end
end
