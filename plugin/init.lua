local wezterm = require("wezterm")

local M = {}

-- ── Defaults ────────────────────────────────────────────────────────────────

local home = wezterm.home_dir or os.getenv("HOME") or os.getenv("USERPROFILE") or "/tmp"

local defaults = {
  -- Where marker files are written (one file per pane ID)
  dir = home .. "/.local/state/wezterm-attention",

  -- Render mode: "tab" | "manual"
  --   tab:    plugin owns format-tab-title (default)
  --   manual: plugin registers no tab handler; use wrap_title_formatter() or API
  renderer = "tab",

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

-- Known attention types (reject unknown values from marker files)
local valid_types = { thinking = true, stop = true, notify = true, review = true }

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
    return data.type, data.frame
  end

  -- Fallback: plain text (backward compat)
  local text = content:gsub("%s+", "")
  if valid_types[text] then return text, nil end
  return nil
end

local function remove_marker(dir, pane_id)
  os.remove(dir .. "/" .. pane_id)
end

-- ── In-memory cache ─────────────────────────────────────────────────────────
-- format-tab-title must not do I/O (blocks GUI thread).
-- update-status reads files on the interval; format-tab-title reads cache.

local attention_cache = {} -- { [pane_id_string] = { type = "stop", frame = 0 } }

-- ── Internal helpers ────────────────────────────────────────────────────────

--- Build the default tab title: "dir / pane_title"
local function default_title(tab)
  local pane = tab.active_pane
  local title = pane.title or ""

  local cwd = pane.current_working_dir
  local dir_name = ""
  if cwd then
    local path = cwd.file_path or cwd.path or tostring(cwd)
    dir_name = string.match(path, "([^/]+)/?$") or ""
  end

  return dir_name ~= "" and (dir_name .. " / " .. title) or title
end

--- Get the resolved attention indicator and type for a tab.
--- Considers all panes and applies priority. Returns (indicator, type, color) or ("", nil, nil).
local function get_tab_attention(tab, opts)
  local cfg_indicators = (opts and opts.indicators) or M._active_indicators or defaults.indicators
  local cfg_colors = (opts and opts.colors) or M._active_colors or defaults.colors
  local cfg_priority = M._active_priority_map or {}

  local best_type     = nil
  local best_priority = -1
  local best_frame    = nil

  for _, p in ipairs(tab.panes) do
    local cached = attention_cache[tostring(p.pane_id)]
    if cached then
      local pri = cfg_priority[cached.type] or 0
      if pri > best_priority then
        best_type     = cached.type
        best_priority = pri
        best_frame    = cached.frame
      end
    end
  end

  if not best_type then return "", nil, nil end

  local indicator = ""
  if best_type == "thinking" then
    local frames = cfg_indicators.thinking_frames
    indicator = frames[((best_frame or 0) % #frames) + 1]
  elseif cfg_indicators[best_type] then
    indicator = cfg_indicators[best_type]
  end

  return indicator, best_type, cfg_colors[best_type]
end

--- Auto-clear applicable markers on an active tab (stop, notify by default).
local function auto_clear_tab(tab)
  local dir = M._active_dir or defaults.dir
  local clear_set = M._active_clear_set or { stop = true, notify = true }
  for _, p in ipairs(tab.panes) do
    local id = tostring(p.pane_id)
    local cached = attention_cache[id]
    if cached and clear_set[cached.type] then
      remove_marker(dir, id)
      attention_cache[id] = nil
    end
  end
end

-- ── Public API ──────────────────────────────────────────────────────────────

--- Read the cached attention state for a pane.
--- Returns (type, frame) or nil.
function M.get_attention(pane_id, opts)
  local id = tostring(pane_id)
  if opts and opts.dir then
    return read_marker(opts.dir, id)
  end
  local cached = attention_cache[id]
  if cached then return cached.type, cached.frame end
  return nil
end

--- Remove the attention marker for a pane.
function M.remove_marker(pane_id, opts)
  local dir = (opts and opts.dir) or defaults.dir
  local id = tostring(pane_id)
  remove_marker(dir, id)
  attention_cache[id] = nil
end

--- Poll marker files and update cache. Call from your own update-status
--- handler if you set auto_poll = false.
function M.poll(window, opts)
  local dir = (opts and opts.dir) or M._active_dir or defaults.dir
  local mux_win = window:mux_window()
  if not mux_win then return end

  local seen = {}
  for _, tab in ipairs(mux_win:tabs()) do
    for _, p in ipairs(tab:panes()) do
      local id = tostring(p:pane_id())
      seen[id] = true
      local atype, frame = read_marker(dir, id)
      if atype then
        attention_cache[id] = { type = atype, frame = frame }
      else
        attention_cache[id] = nil
      end
    end
  end

  for id in pairs(attention_cache) do
    if not seen[id] then attention_cache[id] = nil end
  end
end

--- Wrap a user's title function with attention decoration.
--- For renderer = "manual" mode. Returns a function suitable for wezterm.on("format-tab-title", ...).
---
--- Usage:
---   wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
---     return string.format("%d %s", tab.tab_index + 1, ctx.default_title)
---   end))
function M.wrap_title_formatter(base_fn)
  return function(tab, tabs, panes, config, hover, max_width)
    local ctx = {
      tabs         = tabs,
      panes        = panes,
      config       = config,
      hover        = hover,
      max_width    = max_width,
      default_title = default_title(tab),
      attention    = { get_tab_attention(tab) },
    }

    -- Auto-clear on active tab
    if tab.is_active then
      auto_clear_tab(tab)
    end

    local base = base_fn(tab, ctx)
    local index = tab.tab_index + 1

    if tab.is_active then
      return " " .. index .. ": " .. base .. " "
    end

    local indicator, atype, color = get_tab_attention(tab)
    local text = " " .. indicator .. index .. ": " .. base .. " "

    if color then
      return {
        { Background = { Color = color } },
        { Text = text },
      }
    end

    return text
  end
end

-- ── apply_to_config ─────────────────────────────────────────────────────────

local applied = false

function M.apply_to_config(config, opts)
  if applied then return end
  applied = true

  opts = opts or {}

  -- Merge options with defaults
  local dir = opts.dir or defaults.dir
  local auto_poll = opts.auto_poll ~= false
  M._active_dir = dir

  -- Resolve renderer: support both new "renderer" and legacy "format_tab_title"
  local renderer = opts.renderer or defaults.renderer
  if opts.format_tab_title == false then renderer = "manual" end

  local title_formatter = opts.title_formatter -- optional user callback

  local colors = {}
  for k, v in pairs(defaults.colors) do colors[k] = v end
  if opts.colors then
    for k, v in pairs(opts.colors) do colors[k] = v end
  end
  M._active_colors = colors

  local indicators = {}
  for k, v in pairs(defaults.indicators) do indicators[k] = v end
  if opts.indicators then
    for k, v in pairs(opts.indicators) do indicators[k] = v end
  end
  M._active_indicators = indicators

  local auto_clear = opts.auto_clear or defaults.auto_clear
  local priority   = opts.priority   or defaults.priority

  -- Build lookup tables
  local clear_set = {}
  for _, t in ipairs(auto_clear) do clear_set[t] = true end
  M._active_clear_set = clear_set

  local priority_map = {}
  for i, t in ipairs(priority) do priority_map[t] = i end
  M._active_priority_map = priority_map

  -- ── Poller: update-status ─────────────────────────────────────────────

  if auto_poll then
    wezterm.on("update-status", function(window, _pane)
      M.poll(window)
    end)
  end

  -- ── Cleanup marker when pane closes ───────────────────────────────────

  wezterm.on("pane-destroyed", function(_window, pane)
    local id = tostring(pane:pane_id())
    remove_marker(dir, id)
    attention_cache[id] = nil
  end)

  -- ── Renderer: format-tab-title ────────────────────────────────────────

  if renderer == "tab" then
    wezterm.on("format-tab-title", function(tab)
      local index = tab.tab_index + 1

      -- Build base title (user callback or default)
      local base
      if title_formatter then
        local ctx = {
          default_title = default_title(tab),
          attention     = { get_tab_attention(tab) },
        }
        base = title_formatter(tab, ctx)
      else
        base = default_title(tab)
      end

      -- Active tab: auto-clear, plain title
      if tab.is_active then
        auto_clear_tab(tab)
        return " " .. index .. ": " .. base .. " "
      end

      -- Inactive tab: attention indicator + background tint
      local indicator, attention_type, color = get_tab_attention(tab)
      local text = " " .. indicator .. index .. ": " .. base .. " "

      if color then
        return {
          { Background = { Color = color } },
          { Text = text },
        }
      end

      return text
    end)
  end
  -- renderer == "manual": no format-tab-title registered

  -- ── Review toggle keybind ─────────────────────────────────────────────

  local review_key = opts.review_key
  if review_key == nil then review_key = defaults.review_key end

  if review_key then
    config.keys = config.keys or {}
    table.insert(config.keys, {
      key  = review_key.key,
      mods = review_key.mods,
      action = wezterm.action_callback(function(_win, pane)
        local id = tostring(pane:pane_id())
        local path = dir .. "/" .. id

        local cached = attention_cache[id]
        if cached and cached.type == "review" then
          os.remove(path)
          attention_cache[id] = nil
          return
        end

        os.execute("mkdir -p " .. dir)
        local w = io.open(path, "w")
        if w then
          w:write('{"type":"review"}')
          w:close()
          attention_cache[id] = { type = "review" }
        end
      end),
    })
  end
end

return M
