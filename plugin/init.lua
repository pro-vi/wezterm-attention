local wezterm = require("wezterm")

local M = {}

-- ── Defaults ────────────────────────────────────────────────────────────────

local home = wezterm.home_dir or os.getenv("HOME") or os.getenv("USERPROFILE") or "/tmp"

local defaults = {
  -- Where marker files are written (one file per pane ID)
  dir = home .. "/.local/state/wezterm-attention",

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
-- update-status reads files on the 5s interval; format-tab-title reads cache.

local attention_cache = {} -- { [pane_id_string] = { type = "stop", frame = 0 } }

-- ── Public API ──────────────────────────────────────────────────────────────

--- Read the cached attention state for a pane.
--- Returns (type, frame) or nil.
function M.get_attention(pane_id, opts)
  local id = tostring(pane_id)
  if opts and opts.dir then
    -- Direct read (bypasses cache) when custom dir is specified
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
--- handler if you set auto_poll = false, or if WezTerm only fires one handler.
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

-- ── apply_to_config ─────────────────────────────────────────────────────────

local applied = false

function M.apply_to_config(config, opts)
  -- Idempotency: only register events once
  if applied then return end
  applied = true

  opts = opts or {}

  -- Merge options with defaults
  local dir = opts.dir or defaults.dir
  local auto_poll = opts.auto_poll ~= false -- default true
  M._active_dir = dir -- expose for M.poll()

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

  -- ── Poller: update-status (runs on status_update_interval) ────────────
  -- Set auto_poll = false if you already have an update-status handler
  -- and call attention.poll(window) from it instead.

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

  -- ── Renderer: format-tab-title (must be instant, no I/O) ─────────────

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

    -- Active tab: auto-clear applicable markers (from cache, remove files)
    if tab.is_active then
      for _, p in ipairs(tab.panes) do
        local id = tostring(p.pane_id)
        local cached = attention_cache[id]
        if cached and clear_set[cached.type] then
          remove_marker(dir, id)
          attention_cache[id] = nil
        end
      end
      return " " .. index .. ": " .. base .. " "
    end

    -- Inactive tab: find highest-priority attention across panes (from cache)
    local best_type     = nil
    local best_priority = -1
    local best_frame    = nil

    for _, p in ipairs(tab.panes) do
      local cached = attention_cache[tostring(p.pane_id)]
      if cached then
        local pri = priority_map[cached.type] or 0
        if pri > best_priority then
          best_type     = cached.type
          best_priority = pri
          best_frame    = cached.frame
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

        -- Toggle off if already in review
        local cached = attention_cache[id]
        if cached and cached.type == "review" then
          os.remove(path)
          attention_cache[id] = nil
          return
        end

        -- Toggle on (ensure directory exists)
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
