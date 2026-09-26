return function(context)
  local wezterm = context.wezterm
  local now_ms = context.now_ms
  local reported_errors = {}
  local publication_session = tostring({}):gsub("[^%w]", "")

  --- Log a message once per key, so a persistent failure does not fill the log
  --- on every poll tick.
  local function report_error_once(key, message)
    if reported_errors[key] then return end
    reported_errors[key] = true
    wezterm.log_error("wezterm-attention: " .. message)
  end

  --- The same, for a setup the plugin works around rather than a failure.
  local function report_warning_once(key, message)
    if reported_errors[key] then return end
    reported_errors[key] = true
    local log = type(wezterm.log_warn) == "function" and wezterm.log_warn or wezterm.log_error
    log("wezterm-attention: " .. message)
  end

  local function json_string(value)
    return '"' .. value:gsub('[%z\1-\31\\"]', function(char)
      local escapes = {
        ['"'] = '\\"', ["\\"] = "\\\\", ["\b"] = "\\b", ["\f"] = "\\f",
        ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t",
      }
      return escapes[char] or string.format("\\u%04x", char:byte())
    end) .. '"'
  end

  --- Place `body` at `path` through a temporary file this process owns, so a
  --- reader sees either the previous content or the whole new one. The name
  --- carries this process's session token: two WezTerm processes writing the
  --- same file never delete each other's in-flight copy. `key` prefixes the
  --- reported failures, so one caller's persistent failure is logged once.
  local function replace_file(path, body, key)
    local tmp = path .. "." .. publication_session .. ".tmp"
    os.remove(tmp)
    local file, open_err = io.open(tmp, "w")
    if not file then
      report_error_once(key .. "-open:" .. path,
        "failed to write " .. tmp .. ": " .. tostring(open_err))
      return false
    end
    local wrote, write_err = file:write(body .. "\n")
    local closed, close_err = file:close()
    if not wrote or not closed then
      os.remove(tmp)
      report_error_once(key .. "-finish:" .. path,
        "failed to finish " .. tmp .. ": " .. tostring(write_err or close_err))
      return false
    end
    local renamed, rename_err = os.rename(tmp, path)
    if not renamed then
      os.remove(tmp)
      report_error_once(key .. "-rename:" .. path,
        "failed to place " .. path .. ": " .. tostring(rename_err))
      return false
    end
    return true
  end

  local function file_exists(path)
    local file = io.open(path, "r")
    if not file then return false end
    file:close()
    return true
  end

  --- Spell an integral number the way JSON wants it read back. What
  --- `tostring` gives for an integral value depends on the Lua the plugin
  --- runs under; the reader of the file below wants an integer in every one
  --- of them.
  local function integer(value)
    return string.format("%d", math.floor(value))
  end

  --- The composed list last written for each window, so an unchanged bar costs
  --- no file work. Keyed by path, because that is what a write would replace.
  local published_tab_lists = {}
  --- The one path each window's order is written to by this process. It is
  --- chosen at the window's first publication and kept for as long as the
  --- window is open, so no window ever has two files of this process's.
  local published_path_by_window = {}

  --- The bytes of every tab order this GUI process has published, by path.
  --- A config reload starts this module afresh, and `wezterm.GLOBAL` is what
  --- WezTerm keeps across one, so this is how a reloaded plugin still knows
  --- which files are its own. GLOBAL takes only UTF-8 text; a path or body it
  --- refuses leaves that file unremembered, which only ever keeps a file.
  local function remember_tab_order(path, body)
    pcall(function()
      local global = wezterm.GLOBAL
      if global.wezterm_attention_tab_orders == nil then
        global.wezterm_attention_tab_orders = {}
      end
      global.wezterm_attention_tab_orders[path] = body
    end)
  end

  local function remembered_tab_order(path)
    local ok, body = pcall(function()
      return wezterm.GLOBAL.wezterm_attention_tab_orders[path]
    end)
    return ok and type(body) == "string" and body or nil
  end

  --- Remove a tab order this process published, but only while the file still
  --- holds the bytes this process wrote there. The unsourced name is shared by
  --- every GUI process, because window ids restart in each one: a file another
  --- process has rewritten since is that process's, and stays. Returns an error
  --- text only for a file of ours that could not be removed.
  local function remove_own_tab_order(path, body)
    local file = io.open(path, "rb")
    if not file then return nil end
    local current = file:read("*a")
    file:close()
    if current ~= body then return nil end
    local removed, err = os.remove(path)
    if removed or not file_exists(path) then return nil end
    return tostring(err)
  end

  --- One encoding per drawn tab. The formatter is called once per tab, so
  --- without this every tab's text would be escaped again on every one of those
  --- calls. A drawn tab is a fresh table each time it is drawn, and the entries
  --- of tabs that closed go with them, so the keys are weak.
  local encoded_tabs = setmetatable({}, { __mode = "k" })

  local function encode_tab(entry)
    local encoded = encoded_tabs[entry]
    if encoded then return encoded end
    local ids = {}
    for position, marker_id in ipairs(entry.marker_ids) do
      ids[position] = json_string(marker_id)
    end
    encoded = table.concat({
      '{"marker_ids":[', table.concat(ids, ","),
      '],"number":', integer(entry.number),
      ',"text":', json_string(entry.text), "}",
    })
    encoded_tabs[entry] = encoded
    return encoded
  end

  --- Publish the drawn order under its source incarnation and window ID, or
  --- under the legacy window ID while the source is unknown. It is not a pane
  --- record and carries its own schema. `marker_ids` are what `gui_tab_pane_ids`
  --- returned: the panes' cache keys, already translated out of the window's
  --- local numbering, so a reader never repeats that translation.
  ---
  --- A window keeps the name of its first publication, source or none, for as
  --- long as this module runs, so it has one file here. The caller holds a
  --- window's first publication until the source is answered, so the
  --- unsourced name is used only where no source will come. A config reload
  --- starts the module afresh, and the plugin before it may have had no
  --- writer to ask: a window it published unsourced gets its sourced name
  --- now, and the unsourced file is removed while it still holds the bytes
  --- this process wrote.
  ---
  --- Honest about when it was written, not guaranteed current: nothing
  --- refreshes `published_at_ms` while the bar draws the same thing. The write
  --- happens when the composed list changes. `drawn_at` is when the bar drew
  --- it, for a caller that held the draw back; it defaults to now.
  local function publish_tab_order(dir, window_id, tabs, source, drawn_at)
    local rows = {}
    for index, entry in ipairs(tabs) do rows[index] = encode_tab(entry) end
    local list = "[" .. table.concat(rows, ",") .. "]"
    local window_key = dir .. "/tabs/" .. integer(window_id)
    local path = published_path_by_window[window_key]
    local held = path and published_tab_lists[path]
    if held then
      if held.list == list then return false end
      source = held.source or nil
    else
      path = dir .. "/tabs/"
        .. (source and (source.incarnation_id .. "-") or "") .. integer(window_id) .. ".json"
    end
    local body = table.concat({
      '{"published_at_ms":', integer(drawn_at or now_ms()),
      ',"schema":', source and "2" or "1",
      source and (',"source":' .. wezterm.json_encode(source)) or "",
      ',"tabs":', list,
      ',"window_id":', integer(window_id), "}",
    })
    if not replace_file(path, body, "publish-tabs") then return false end
    published_tab_lists[path] = {
      list = list, body = body .. "\n", window_id = tostring(window_id), window_key = window_key,
      source = source or false,
    }
    published_path_by_window[window_key] = path
    remember_tab_order(path, body .. "\n")
    if source and not held then
      local unsourced = window_key .. ".json"
      local earlier = remembered_tab_order(unsourced)
      if earlier then
        remember_tab_order(unsourced, nil)
        local err = remove_own_tab_order(unsourced, earlier)
        if err then
          report_error_once("replace-tabs:" .. unsourced,
            "cannot remove the unsourced tab order " .. unsourced .. ": " .. err)
        end
      end
    end
    return true
  end

  --- Remove the tab orders this process published for windows no longer in
  --- `live`, a set keyed by window id as a decimal string. A closed window's
  --- bar never redraws, so nothing else would ever take its file back. Only a
  --- file this process wrote, still holding what it wrote, is touched: another
  --- GUI process's file, or one left behind by a process that has exited, is
  --- `attention sweep`'s to collect. Forgetting the path is what lets a window that later reuses the
  --- id publish again.
  local function withdraw_closed_tab_orders(dir, live)
    local prefix = dir .. "/tabs/"
    for path, publication in pairs(published_tab_lists) do
      local window_id = path:sub(1, #prefix) == prefix and publication.window_id
      if window_id and not live[window_id] then
        published_tab_lists[path] = nil
        published_path_by_window[publication.window_key] = nil
        remember_tab_order(path, nil)
        local err = remove_own_tab_order(path, publication.body)
        if err then
          report_error_once("withdraw-tabs:" .. path,
            "cannot withdraw closed window's tab order " .. path .. ": " .. err)
        end
      end
    end
  end

  return {
    report_error_once = report_error_once,
    report_warning_once = report_warning_once,
    publish_tab_order = publish_tab_order,
    withdraw_closed_tab_orders = withdraw_closed_tab_orders,
  }
end
