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

  -- Generated thinking frames are derived from wall-clock buckets. The
  -- default matches WezTerm's default status update interval.
  frame_interval_ms = 1000,

  -- Backstop for unexpected event feedback from the redraw action.
  max_redraws_per_second = 4,

  -- Higher index = higher priority when multiple panes have attention
  priority = { "thinking", "review", "stop", "notify" },

  -- These types are acknowledged when their pane becomes active
  auto_clear = { "stop", "notify" },

  -- Stale marker cleanup by type, in milliseconds. Prevents zombie busy tabs
  -- after a process exits without clearing its marker. Set to false to disable.
  stale_after_ms = { thinking = 30 * 60 * 1000 },

  -- Keybind to toggle "review" marker on active pane (false to disable)
  review_key = { key = "b", mods = "ALT" },
}

-- Known attention types (reject unknown values from marker files)
local valid_types = { thinking = true, stop = true, notify = true, review = true }

local function coarse_now_ms()
  return os.time() * 1000
end

-- Bind one wall-clock source for this Lua generation. `%s%3f` is Chrono's
-- seconds-plus-three-fractional-digits form, which produces integer epoch
-- milliseconds. Builds without that API use the known one-second fallback.
local clock_resolution = 1000
local clock_now_ms = coarse_now_ms
local clock_fallback_reported = false

local function wezterm_clock_ms()
  local value = tonumber(wezterm.time.now():format_utc("%s%3f"))
  if not value or value < 1000000000000 then
    error("unexpected wezterm.time millisecond value")
  end
  return value
end

if wezterm.time and type(wezterm.time.now) == "function" then
  local ok = pcall(wezterm_clock_ms)
  if ok then
    clock_resolution = 1
    clock_now_ms = function()
      local current_ok, value = pcall(wezterm_clock_ms)
      if current_ok then return value end

      -- A later clock failure must fail coarse, never keep claiming a finer
      -- resolution than the source can provide.
      clock_resolution = 1000
      clock_now_ms = coarse_now_ms
      if not clock_fallback_reported then
        clock_fallback_reported = true
        wezterm.log_error("wezterm-attention: high-resolution clock failed; using one-second animation buckets")
      end
      return clock_now_ms()
    end
  end
end

local function now_ms()
  return clock_now_ms()
end

local function clock_resolution_ms()
  return clock_resolution
end

local function positive_number(value, fallback)
  local number = tonumber(value)
  if not number or number <= 0 or number ~= number then return fallback end
  return number
end

local function effective_frame_interval_ms()
  local configured = M._active_frame_interval_ms or defaults.frame_interval_ms
  return math.max(configured, clock_resolution_ms())
end

local function frame_for_now(poll_now_ms, frame_count)
  if frame_count <= 0 then return 0 end
  return math.floor(poll_now_ms / effective_frame_interval_ms()) % frame_count
end

local function normalize_epoch_ms(value)
  local n = tonumber(value)
  if not n then return nil end
  -- Accept either seconds or milliseconds. Current Unix seconds are 10 digits;
  -- current Unix milliseconds are 13 digits.
  if n < 100000000000 then return n * 1000 end
  return n
end

local function stale_ttl_ms(atype, marker_ttl_ms)
  local explicit = tonumber(marker_ttl_ms)
  if explicit and explicit > 0 then return explicit end

  local cfg = M._active_stale_after_ms
  if cfg == false then return nil end
  cfg = cfg or defaults.stale_after_ms
  if type(cfg) ~= "table" then return nil end

  local ttl = cfg[atype]
  if ttl == false then return nil end
  ttl = tonumber(ttl)
  if ttl and ttl > 0 then return ttl end
  return nil
end

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
    local revision = type(data.revision) == "string" and data.revision ~= "" and data.revision or nil
    return data.type, data.frame, normalize_epoch_ms(data.updated_at or data.updated_at_ms), data.ttl_ms, content, revision
  end

  -- Fallback: plain text (backward compat)
  local text = content:gsub("%s+", "")
  if valid_types[text] then return text, nil, nil, nil, content end
  return nil
end

local acknowledgement_errors = {}
local revision_counter = 0
local revision_session = tostring({}):gsub("[^%w]", "")

local function next_marker_revision()
  revision_counter = revision_counter + 1
  return table.concat({ tostring(math.floor(now_ms())), revision_session, revision_counter }, "-")
end

local function acknowledgement_path(dir, pane_id)
  return dir .. "/" .. pane_id .. ".ack"
end

local function marker_identity(revision, raw)
  if revision then return "revision\n" .. revision end
  return "raw\n" .. (raw or "")
end

local function read_acknowledgement(dir, pane_id)
  local file = io.open(acknowledgement_path(dir, pane_id), "r")
  if not file then return nil end
  local identity = file:read("*a")
  file:close()
  return identity
end

local function report_acknowledgement_error_once(key, message)
  if acknowledgement_errors[key] then return end
  acknowledgement_errors[key] = true
  wezterm.log_error("wezterm-attention: " .. message)
end

local function clear_acknowledgement(dir, pane_id)
  local path = acknowledgement_path(dir, pane_id)
  local existing = io.open(path, "r")
  if not existing then return true end
  existing:close()

  local ok, err = os.remove(path)
  if ok then return true end
  report_acknowledgement_error_once(
    "clear:" .. pane_id,
    "failed to remove acknowledgement " .. path .. ": " .. tostring(err))
  return false
end

local function write_acknowledgement(dir, pane_id, identity)
  local path = acknowledgement_path(dir, pane_id)
  local tmp = path .. ".tmp"
  os.remove(tmp)
  local file, open_err = io.open(tmp, "w")
  if not file then
    report_acknowledgement_error_once(
      "write:" .. pane_id,
      "failed to write acknowledgement " .. tmp .. ": " .. tostring(open_err))
    return false
  end

  local wrote, write_err = file:write(identity)
  local closed, close_err = file:close()
  if not wrote or not closed then
    os.remove(tmp)
    report_acknowledgement_error_once(
      "write:" .. pane_id,
      "failed to finish acknowledgement " .. tmp .. ": " .. tostring(write_err or close_err))
    return false
  end

  if not clear_acknowledgement(dir, pane_id) then
    os.remove(tmp)
    return false
  end

  local renamed, rename_err = os.rename(tmp, path)
  if not renamed then
    os.remove(tmp)
    report_acknowledgement_error_once(
      "write:" .. pane_id,
      "failed to place acknowledgement " .. path .. ": " .. tostring(rename_err))
    return false
  end
  return true
end

local function acknowledgement_matches(dir, pane_id, raw, revision)
  local acknowledged = read_acknowledgement(dir, pane_id)
  if not raw then
    if acknowledged then clear_acknowledgement(dir, pane_id) end
    return false
  end

  local identity = marker_identity(revision, raw)
  if acknowledged == identity then return true end

  -- Cleanup is best-effort. A stale sidecar never suppresses absent or
  -- mismatched canonical truth even when it cannot be removed.
  if acknowledged then clear_acknowledgement(dir, pane_id) end
  return false
end

local function read_effective_marker(dir, pane_id)
  -- A crash can strand only the temporary sidecar. It was never authoritative.
  os.remove(acknowledgement_path(dir, pane_id) .. ".tmp")
  local atype, frame, updated_at, marker_ttl_ms, raw, revision = read_marker(dir, pane_id)
  if acknowledgement_matches(dir, pane_id, raw, revision) then return nil end
  return atype, frame, updated_at, marker_ttl_ms, raw, revision
end

local function remove_marker(dir, pane_id)
  os.remove(dir .. "/" .. pane_id)
  clear_acknowledgement(dir, pane_id)
  os.remove(acknowledgement_path(dir, pane_id) .. ".tmp")
end

-- ── In-memory cache ─────────────────────────────────────────────────────────
-- format-tab-title must not poll every pane from disk (it blocks the GUI
-- thread). update-status fills this cache; acknowledgement performs one confirmation
-- read only when acknowledging a cached terminal marker.

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

--- Pane IDs of one tab, as the GUI hands them to format-tab-title.
local function gui_tab_pane_ids(tab)
  local ids = {}
  for _, p in ipairs(tab.panes) do
    ids[#ids + 1] = tostring(p.pane_id)
  end
  return ids
end

--- Pane IDs of one tab, as the mux hands them to poll().
local mux_projection_fallback_reported = false

local function mux_tab_pane_ids(tab)
  local panes_with_info = tab.panes_with_info
  if type(panes_with_info) == "function" then
    local ok, infos = pcall(panes_with_info, tab)
    if ok and type(infos) == "table" then
      local all = {}
      local zoomed = {}
      for _, info in ipairs(infos) do
        local pane = info.pane
        if pane then
          local id = tostring(pane:pane_id())
          all[#all + 1] = id
          if info.is_zoomed then zoomed[#zoomed + 1] = id end
        end
      end
      if #zoomed > 0 then return zoomed end
      return all
    end
  end

  if not mux_projection_fallback_reported then
    mux_projection_fallback_reported = true
    wezterm.log_error(
      "wezterm-attention: this WezTerm build does not expose tab:panes_with_info(); " ..
      "zoomed tabs may redraw on hidden-pane changes")
  end

  local ids = {}
  for _, p in ipairs(tab:panes()) do
    ids[#ids + 1] = tostring(p:pane_id())
  end
  return ids
end

--- Project one tab's panes into exactly what a title formatter can see:
--- { indicator = string, type = string|nil, color = string|nil }.
--- Pane IDs are the input, not a GUI or mux tab object, so the renderer and
--- the poller resolve the same value from the same tab. Panes with no cache
--- entry contribute nothing.
local function resolve_visible_attention(pane_ids, opts)
  local cfg_indicators = (opts and opts.indicators) or M._active_indicators or defaults.indicators
  local cfg_colors = (opts and opts.colors) or M._active_colors or defaults.colors
  local cfg_priority = M._active_priority_map or {}

  local best_type     = nil
  local best_priority = -1
  local best_frame    = nil

  for _, id in ipairs(pane_ids) do
    local cached = attention_cache[id]
    if cached then
      local pri = cfg_priority[cached.type] or 0
      if pri > best_priority then
        best_type     = cached.type
        best_priority = pri
        best_frame    = cached.frame
      end
    end
  end

  if not best_type then
    return { indicator = "", type = nil, color = nil }
  end

  local indicator = ""
  if best_type == "thinking" then
    local frames = cfg_indicators.thinking_frames
    if frames and #frames > 0 then
      indicator = frames[((best_frame or 0) % #frames) + 1]
    end
  elseif cfg_indicators[best_type] then
    indicator = cfg_indicators[best_type]
  end

  return { indicator = indicator, type = best_type, color = cfg_colors[best_type] }
end

--- Two projections are the same iff a viewer could not tell them apart.
--- Marker bytes, TTL metadata, pane ordering and cache identity are all
--- deliberately excluded: a change none of these three fields reflects is a
--- change no tab title would show.
local function same_visible_attention(a, b)
  return a.indicator == b.indicator and a.type == b.type and a.color == b.color
end

--- Return the panes and tab that contain pane_id, or nil if none does.
--- Uses only mux_win:tabs()/tab:panes() — the same WezTerm API surface poll()
--- already calls every tick — so it stays within the plugin's compatibility
--- floor (active_tab()/active_pane() don't exist on the oldest plugin builds).
local function tab_panes_containing(mux_win, pane_id)
  for _, tab in ipairs(mux_win:tabs()) do
    local panes = tab:panes()
    for _, p in ipairs(panes) do
      if tostring(p:pane_id()) == pane_id then
        return panes, tab
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
local function cache_marker_values(id, atype, frame, raw, observed_now)
  if not atype then
    attention_cache[id] = nil
    return
  end

  if atype == "thinking" and frame == nil then
    local cfg_indicators = M._active_indicators or defaults.indicators
    local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
    frame = frame_for_now(observed_now, #frames)
  end

  attention_cache[id] = {
    type        = atype,
    frame       = frame,
    observed_at = observed_now,
    raw         = raw,
  }
end

local function acknowledge_focused_pane(pane_id, opts)
  local dir = (opts and opts.dir) or M._active_dir or defaults.dir
  local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
  local observed_now = (opts and opts.now_ms) or now_ms()

  local id = tostring(pane_id)
  local cached = attention_cache[id]
  if not (cached and acknowledge_set[cached.type]) then return "absent" end

  local current_type, current_frame, _, _, raw, revision = read_marker(dir, id)
  if not current_type then
    clear_acknowledgement(dir, id)
    attention_cache[id] = nil
    return "absent"
  end

  if not acknowledge_set[current_type] then
    clear_acknowledgement(dir, id)
    cache_marker_values(id, current_type, current_frame, raw, observed_now)
    return "kept"
  end

  local write_ack = (opts and opts.write_acknowledgement) or write_acknowledgement
  if not write_ack(dir, id, marker_identity(revision, raw)) then
    cache_marker_values(id, current_type, current_frame, raw, observed_now)
    return "failed"
  end

  -- A writer may replace or clear the marker while the sidecar is being
  -- written. Re-read effective truth before updating the cache: only the exact
  -- identity that was viewed is suppressed.
  local effective_type, effective_frame, _, _, effective_raw = read_effective_marker(dir, id)
  cache_marker_values(id, effective_type, effective_frame, effective_raw, observed_now)
  return effective_type and "kept" or "acknowledged"
end

-- ── Focus and redraw ────────────────────────────────────────────────────────

-- update-status fires several times a second, so a missing WezTerm method is
-- reported once per process rather than once per tick.
local reported_missing = {}
local redraw_budget = {}
local redraw_budget_reported = false
local redraw_disabled = {}

local function report_missing_once(method_name)
  if reported_missing[method_name] then return end
  reported_missing[method_name] = true
  wezterm.log_error(
    "wezterm-attention: this WezTerm build does not expose window:" .. method_name ..
    "(); inactive tabs will update only on ordinary WezTerm redraws")
end

local function redraw_window_key(window)
  local window_id = window.window_id
  if type(window_id) == "function" then
    local ok, id = pcall(window_id, window)
    if ok and id ~= nil then return tostring(id) end
  end
  return tostring(window)
end

local function redraw_allowed(window, poll_now_ms)
  local limit = math.floor(
    M._active_max_redraws_per_second or defaults.max_redraws_per_second)
  if limit <= 0 then return false end

  local key = redraw_window_key(window)
  local second = math.floor(poll_now_ms / 1000)
  local budget = redraw_budget[key]
  if not budget or budget.second ~= second then
    budget = { second = second, count = 0 }
    redraw_budget[key] = budget
  end

  if budget.count >= limit then
    if not redraw_budget_reported then
      redraw_budget_reported = true
      wezterm.log_error("wezterm-attention: tab bar redraw budget exhausted; suppressing excess redraws")
    end
    return false
  end

  budget.count = budget.count + 1
  return true
end

--- True only when this GUI window currently has keyboard focus. A build that
--- cannot answer counts as unfocused, which costs a redraw and never acknowledges a
--- marker the user has not seen.
local function window_is_focused(window)
  local is_focused = window.is_focused
  if type(is_focused) ~= "function" then
    report_missing_once("is_focused")
    return false
  end
  local ok, focused = pcall(is_focused, window)
  return ok and focused == true
end

--- Resolve current pane truth at use time. The pane captured when
--- update-status was scheduled can be stale by the time its async callback
--- runs, so it is never destructive authority.
local function window_current_active_pane(window)
  local active_pane = window.active_pane
  if type(active_pane) ~= "function" then
    report_missing_once("active_pane")
    return nil
  end
  local ok, resolved = pcall(active_pane, window)
  if not ok then return nil end
  return resolved
end

local function pane_belongs_to_tabs(pane, mux_tabs)
  if not pane then return false end
  local target_id = tostring(pane:pane_id())
  for _, tab in ipairs(mux_tabs) do
    for _, candidate in ipairs(tab:panes()) do
      if tostring(candidate:pane_id()) == target_id then return true end
    end
  end
  return false
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
local function request_tab_bar_redraw(window, pane)
  if not pane then return false end

  local window_key = redraw_window_key(window)
  if redraw_disabled[window_key] then return false end

  local perform_action = window.perform_action
  if type(perform_action) ~= "function" then
    report_missing_once("perform_action")
    redraw_disabled[window_key] = true
    return false
  end

  local ok, err = pcall(perform_action, window, wezterm.action.ActivateTabRelative(0), pane)
  if not ok then
    redraw_disabled[window_key] = true
    wezterm.log_error("wezterm-attention: tab bar redraw failed: " .. tostring(err))
    return false
  end
  return true
end

-- ── Public API ──────────────────────────────────────────────────────────────

--- Read the cached attention state for a pane.
--- Returns (type, frame) or nil.
function M.get_attention(pane_id, opts)
  local id = tostring(pane_id)
  if opts and opts.dir then
    return read_effective_marker(opts.dir, id)
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

--- Poll marker files, update the cache, acknowledge what the user is looking
--- at, and ask WezTerm to redraw the tab bar if what it shows has changed.
--- Call this from your own update-status handler if you set auto_poll = false;
--- pass the handler's pane as opts.active_pane.
---
--- Only refreshes entries for panes in the current window. Cross-window cache
--- entries are left alone — pruning them here would cause cache thrash when
--- multiple windows fire update-status (each window would wipe the other's
--- entries every tick, producing visible tab-indicator blinking). Stale
--- thinking markers are removed here by TTL; closed panes are cleaned up by
--- the pane-destroyed handler.
---
--- The redraw exists because caching alone is not enough: WezTerm calls
--- format-tab-title when something it knows about changes, and a marker file
--- appearing on disk is not one of those things. Without the request below, a
--- background pane's new state sat in the cache, unrendered, until the user
--- happened to switch tabs — which is exactly when they no longer needed to be
--- told.
function M.poll(window, opts)
  local dir = (opts and opts.dir) or M._active_dir or defaults.dir
  local mux_win = window:mux_window()
  if not mux_win then return end

  -- Hold one tab list for the whole poll so the before/after snapshots below
  -- describe the same tabs in the same order.
  local mux_tabs = mux_win:tabs()
  local before = {}
  for i, tab in ipairs(mux_tabs) do
    before[i] = resolve_visible_attention(mux_tab_pane_ids(tab))
  end

  local now = opts and opts.now_ms
  if type(now) == "function" then now = now() end
  if type(now) ~= "number" or now ~= now then now = now_ms() end
  local cfg_indicators = M._active_indicators or defaults.indicators
  local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
  local frame_count = #frames

  for _, tab in ipairs(mux_tabs) do
    for _, p in ipairs(tab:panes()) do
      local id = tostring(p:pane_id())
      local atype, frame, updated_at, marker_ttl_ms, raw, revision = read_marker(dir, id)
      local acknowledged = acknowledgement_matches(dir, id, raw, revision)
      if atype then
        local cached = attention_cache[id]
        local observed_at = now
        if cached and cached.raw == raw and cached.observed_at then
          observed_at = cached.observed_at
        end

        local effective_updated_at = updated_at or observed_at
        local ttl = stale_ttl_ms(atype, marker_ttl_ms)
        if ttl and now - effective_updated_at > ttl then
          remove_marker(dir, id)
          attention_cache[id] = nil
        elseif acknowledged then
          attention_cache[id] = nil
        else
          if atype == "thinking" and frame == nil then
            -- A frame is a function of time, never of poll count. The redraw
            -- action can induce immediate update-status events; every poll in
            -- the same bucket therefore projects the same frame and the chain
            -- terminates at the visible-change comparison below.
            frame = frame_for_now(now, frame_count)
          end
          attention_cache[id] = {
            type        = atype,
            frame       = frame,
            observed_at = observed_at,
            raw         = raw,
          }
        end
      else
        attention_cache[id] = nil
      end
    end
  end

  -- Everything below is about the focused window only. An unfocused window
  -- must neither acknowledge a marker its user has not seen nor be sent a key
  -- action, so an unfocused poll ends here with the cache correct.
  if not window_is_focused(window) then return end

  local current_active_pane = window_current_active_pane(window)
  if pane_belongs_to_tabs(current_active_pane, mux_tabs) then
    acknowledge_focused_pane(current_active_pane:pane_id(), { dir = dir, now_ms = now })
  end

  -- The current pane is preferred for both acknowledgement and action. The
  -- event pane remains a compatibility transport only when this WezTerm build
  -- cannot resolve current pane state; it never authorizes acknowledgement.
  local action_pane = current_active_pane or (opts and opts.active_pane)

  -- Compare what the tab bar would show, not what the cache holds. A marker
  -- that changed behind a higher-priority sibling, or a frame number nothing
  -- renders, must not cost a redraw.
  local changed = false
  for i, tab in ipairs(mux_tabs) do
    local after = resolve_visible_attention(mux_tab_pane_ids(tab))
    if not same_visible_attention(before[i], after) then
      changed = true
      break
    end
  end

  if changed and redraw_allowed(window, now) then
    request_tab_bar_redraw(window, action_pane)
  end
end

--- Apply the shared attention indicator and color decoration to a base title.
local function decorate_tab_title(tab, visible, base)
  local text = " " .. visible.indicator .. (tab.tab_index + 1) .. ": " .. base .. " "
  if visible.color then
    return {
      { Background = { Color = visible.color } },
      { Text = text },
    }
  end
  return text
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
    -- Read-only. WezTerm may call this at any moment, including for a window
    -- the user is not looking at, so acknowledgement belongs in poll() where
    -- focus is known.
    local visible = resolve_visible_attention(gui_tab_pane_ids(tab))

    local ctx = {
      tabs         = tabs,
      panes        = panes,
      config       = config,
      hover        = hover,
      max_width    = max_width,
      default_title = default_title(tab),
      attention    = { visible.indicator, visible.type, visible.color },
    }

    return decorate_tab_title(tab, visible, base_fn(tab, ctx))
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

  local acknowledge_types = opts.auto_clear or defaults.auto_clear
  local priority   = opts.priority   or defaults.priority
  M._active_frame_interval_ms = positive_number(opts.frame_interval_ms, defaults.frame_interval_ms)
  M._active_max_redraws_per_second = positive_number(
    opts.max_redraws_per_second,
    defaults.max_redraws_per_second)
  local stale_after_ms = opts.stale_after_ms
  if stale_after_ms == nil then stale_after_ms = defaults.stale_after_ms end
  M._active_stale_after_ms = stale_after_ms

  -- Build lookup tables
  local acknowledge_set = {}
  for _, t in ipairs(acknowledge_types) do acknowledge_set[t] = true end
  M._active_acknowledge_set = acknowledge_set

  local priority_map = {}
  for i, t in ipairs(priority) do priority_map[t] = i end
  M._active_priority_map = priority_map

  -- ── Poller: update-status ─────────────────────────────────────────────

  if auto_poll then
    wezterm.on("update-status", function(window, pane)
      M.poll(window, { active_pane = pane })
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
      -- Read-only. WezTerm may call this at any moment, including for a window
      -- the user is not looking at, so acknowledgement belongs in poll() where
      -- focus is known.
      --
      -- Resolve what this tab shows, including an unfocused sibling marker
      -- on the active tab.
      local visible = resolve_visible_attention(gui_tab_pane_ids(tab))

      -- Build base title (user callback or default)
      local base
      if title_formatter then
        local ctx = {
          default_title = default_title(tab),
          attention     = { visible.indicator, visible.type, visible.color },
        }
        base = title_formatter(tab, ctx)
      else
        base = default_title(tab)
      end

      return decorate_tab_title(tab, visible, base)
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
      action = wezterm.action_callback(function(win, pane)
        -- The review indicator is tab-level: resolve_visible_attention lights the tab
        -- if ANY of its panes is flagged. So toggle across every pane in the
        -- focused pane's tab — toggling only the focused pane leaves a split
        -- tab stuck showing ◆ (the other pane is still flagged) and unclearable.
        -- Panes not in any tab (GUI overlays) fall back to per-pane behavior.
        local mux_win = win:mux_window()
        local target_id = tostring(pane:pane_id())
        local panes, mux_tab
        if mux_win then panes, mux_tab = tab_panes_containing(mux_win, target_id) end
        panes = panes or { pane }
        local visible_pane_ids = mux_tab and mux_tab_pane_ids(mux_tab)
        local before = visible_pane_ids and resolve_visible_attention(visible_pane_ids)

        local function redraw_if_visible_changed()
          if not visible_pane_ids then return end
          local after = resolve_visible_attention(visible_pane_ids)
          if not same_visible_attention(before, after) then
            request_tab_bar_redraw(win, pane)
          end
        end

        -- Decide and act on disk truth, never the cache. poll() rebuilds the
        -- cache from files every tick, so the cache can lag a marker an external
        -- process just rewrote. Trusting it here is unsafe two ways: a review on
        -- disk but not yet cached would be missed (the clear skips it and the
        -- next poll re-lights the tab), and — worse — a pane cached as review
        -- whose file was just overwritten with stop/notify would be deleted by
        -- the clear path, dropping a completion/failure notification. Read the
        -- file so this destructive toggle only ever removes a real review marker.
        local function is_review(id)
          return read_effective_marker(dir, id) == "review"
        end

        local has_review = false
        for _, p in ipairs(panes) do
          if is_review(tostring(p:pane_id())) then
            has_review = true
            break
          end
        end

        -- Tab already flagged → clear review from all its panes (sibling
        -- stop/notify/thinking markers are spared by the is_review guard).
        if has_review then
          for _, p in ipairs(panes) do
            local id = tostring(p:pane_id())
            if is_review(id) then
              remove_marker(dir, id)
              attention_cache[id] = nil
            end
          end
          -- The user pressed this key in this window, so it is focused.
          redraw_if_visible_changed()
          return
        end

        -- Tab not flagged → flag the focused pane (the active pane of its tab,
        -- always a member of `panes`, so flag and clear stay symmetric).
        --
        -- Never clobber a process-owned marker that may have landed since the
        -- last poll: review is a manual overlay, and stop/notify are terminal,
        -- so overwriting one would silently drop a completion/failure signal.
        -- Use physical writer truth here. An acknowledged terminal marker is
        -- intentionally absent from effective attention but is still owned by
        -- its writer and must not be replaced by the review overlay.
        local existing = read_marker(dir, target_id)
        if existing ~= nil and existing ~= "review" then
          return
        end

        -- Write atomically (tmp + rename) so a concurrent poll() — including
        -- one in another window — never reads a half-written marker, matching
        -- the atomic-write protocol the README recommends.
        local quoted_dir = dir:gsub("'", [['\'']])
        os.execute("mkdir -p '" .. quoted_dir .. "'")
        local path = dir .. "/" .. target_id
        local tmp = path .. ".tmp"
        local w = io.open(tmp, "w")
        if not w then
          wezterm.log_error("wezterm-attention: failed to write review marker " .. path)
          return
        end
        local revision = next_marker_revision()
        local raw = '{"type":"review","revision":"' .. revision .. '"}'
        w:write(raw)
        w:close()
        if os.rename(tmp, path) then
          attention_cache[target_id] = {
            type = "review",
            raw = raw,
          }
          redraw_if_visible_changed()
        else
          os.remove(tmp)
          wezterm.log_error("wezterm-attention: failed to place review marker " .. path)
        end
      end),
    })
  end
end

-- Internal seams, exposed for the LuaJIT specs only. Not public API.
M._internal = {
  acknowledge_focused_pane = acknowledge_focused_pane,
  clock_resolution_ms      = clock_resolution_ms,
  effective_frame_interval_ms = effective_frame_interval_ms,
  gui_tab_pane_ids         = gui_tab_pane_ids,
  mux_tab_pane_ids         = mux_tab_pane_ids,
  resolve_visible_attention = resolve_visible_attention,
  same_visible_attention   = same_visible_attention,
}

return M
