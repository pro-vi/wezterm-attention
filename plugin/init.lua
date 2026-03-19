local wezterm = require("wezterm")

local M = {}

-- ── Defaults ────────────────────────────────────────────────────────────────

local defaults = {
  -- Where marker files are written (one file per pane ID)
  dir = os.getenv("HOME") .. "/.claude/wezterm-attention",

  -- Tab background tint per attention type (subtle, dark)
  colors = {
    thinking = "#1c1730",
    stop     = "#12271c",
    notify   = "#240f16",
    review   = "#1a1a0c",
  },

  -- Tab text indicators
  indicators = {
    thinking_frames = { "◌ ", "◔ ", "◑ ", "◕ " },
    stop   = "✓ ",
    notify = "! ",
    review = "◆ ",
  },

  -- Higher index = higher priority when multiple panes have attention
  priority = { "thinking", "review", "stop", "notify" },

  -- These types auto-clear when their tab becomes active
  auto_clear = { "stop", "notify" },

  -- Keybind to toggle "review" marker on active pane (false to disable)
  review_key = { key = "b", mods = "ALT" },
}

-- ── Marker I/O ──────────────────────────────────────────────────────────────

local function get_attention(dir, pane_id)
  local f = io.open(dir .. "/" .. pane_id, "r")
  if not f then return nil end
  local content = f:read("*a")
  f:close()

  -- Try JSON first: {"type":"stop","frame":2}
  local ok, data = pcall(function()
    return wezterm.json_parse(content)
  end)
  if ok and data and data.type then
    return data.type, data.frame
  end

  -- Fallback: plain text (backward compat)
  local text = content:gsub("%s+", "")
  if text ~= "" then return text, nil end
  return nil
end

local function remove_marker(dir, pane_id)
  os.remove(dir .. "/" .. pane_id)
end

-- ── Public API ──────────────────────────────────────────────────────────────

--- Read the attention state for a pane.
--- Returns (type, frame) or nil.
function M.get_attention(pane_id, opts)
  local dir = (opts and opts.dir) or defaults.dir
  return get_attention(dir, tostring(pane_id))
end

--- Remove the attention marker for a pane.
function M.remove_marker(pane_id, opts)
  local dir = (opts and opts.dir) or defaults.dir
  remove_marker(dir, tostring(pane_id))
end

-- ── apply_to_config ─────────────────────────────────────────────────────────

function M.apply_to_config(config, opts)
  opts = opts or {}

  -- Merge options with defaults
  local dir = opts.dir or defaults.dir

  local colors = {}
  for k, v in pairs(defaults.colors) do colors[k] = v end
  if opts.colors then
    for k, v in pairs(opts.colors) do colors[k] = v end
  end

  local indicators = {}
  for k, v in pairs(defaults.indicators) do indicators[k] = v end
  if opts.indicators then
    for k, v in pairs(opts.indicators) do indicators[k] = v end
  end

  local auto_clear = opts.auto_clear or defaults.auto_clear
  local priority   = opts.priority   or defaults.priority

  -- Build lookup tables
  local clear_set = {}
  for _, t in ipairs(auto_clear) do clear_set[t] = true end

  local priority_map = {}
  for i, t in ipairs(priority) do priority_map[t] = i end

  -- ── Events ──────────────────────────────────────────────────────────────

  -- Cleanup marker when pane closes
  wezterm.on("pane-destroyed", function(_window, pane)
    remove_marker(dir, pane:pane_id())
  end)

  -- Tab title: index + dir + title + attention indicator
  wezterm.on("format-tab-title", function(tab)
    local pane = tab.active_pane
    local title = pane.title or ""
    local index = tab.tab_index + 1

    -- Directory basename from cwd
    local cwd = pane.current_working_dir
    local dir_name = ""
    if cwd then
      local path = cwd.file_path or cwd.path or tostring(cwd)
      dir_name = string.match(path, "([^/]+)/?$") or ""
    end

    local base = dir_name ~= "" and (dir_name .. " / " .. title) or title

    -- Active tab: auto-clear applicable markers
    if tab.is_active then
      for _, p in ipairs(tab.panes) do
        local attention = get_attention(dir, p.pane_id)
        if attention and clear_set[attention] then
          remove_marker(dir, p.pane_id)
        end
      end
      return " " .. index .. ": " .. base .. " "
    end

    -- Inactive tab: find highest-priority attention across panes
    local best_type     = nil
    local best_priority = -1
    local best_frame    = nil

    for _, p in ipairs(tab.panes) do
      local attention, frame = get_attention(dir, p.pane_id)
      if attention then
        local pri = priority_map[attention] or 0
        if pri > best_priority then
          best_type     = attention
          best_priority = pri
          best_frame    = frame
        end
      end
    end

    -- Resolve indicator text
    local indicator = ""
    if best_type then
      if best_type == "thinking" then
        local frames = indicators.thinking_frames
        indicator = frames[((best_frame or 0) % #frames) + 1]
      elseif indicators[best_type] then
        indicator = indicators[best_type]
      end
    end

    local text = " " .. indicator .. index .. ": " .. base .. " "

    -- Tint tab background for attention types
    if best_type and colors[best_type] then
      return {
        { Background = { Color = colors[best_type] } },
        { Text = text },
      }
    end

    return text
  end)

  -- ── Review toggle keybind ───────────────────────────────────────────────

  local review_key = opts.review_key
  if review_key == nil then review_key = defaults.review_key end

  if review_key then
    config.keys = config.keys or {}
    table.insert(config.keys, {
      key  = review_key.key,
      mods = review_key.mods,
      action = wezterm.action_callback(function(_win, pane)
        local path = dir .. "/" .. tostring(pane:pane_id())

        -- Toggle off if already in review
        local attention = get_attention(dir, pane:pane_id())
        if attention == "review" then
          os.remove(path)
          return
        end

        -- Toggle on
        local w = io.open(path, "w")
        if w then
          w:write('{"type":"review"}')
          w:close()
        end
      end),
    })
  end
end

return M
