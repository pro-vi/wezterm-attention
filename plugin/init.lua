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

  -- These types are acknowledged when their pane becomes active
  acknowledge_types = { "stop", "notify" },

  -- Stale marker cleanup by type, in milliseconds. Prevents zombie busy tabs
  -- after a process exits without clearing its marker. Set to false to disable.
  stale_after_ms = { thinking = 30 * 60 * 1000 },

  -- Keybind to toggle "review" marker on active pane (false to disable)
  review_key = { key = "b", mods = "ALT" },

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false when the host already repaints titles on its own tick.
  request_redraw = true,
}

-- Known attention types (reject unknown values from marker files)
local valid_types = { thinking = true, stop = true, notify = true, review = true }

local function now_ms()
  return os.time() * 1000
end

local function frame_for_now(poll_now_ms, frame_count)
  if frame_count <= 0 then return 0 end
  return math.floor(poll_now_ms / 1000) % frame_count
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

local subagent_live_ms = 10 * 60 * 1000

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

local reported_errors = {}
local publication_counter = 0
local publication_session = tostring({}):gsub("[^%w]", "")

local function next_publication_id()
  publication_counter = publication_counter + 1
  return table.concat({
    tostring(math.floor(now_ms())), publication_session, publication_counter,
  }, "-")
end

local function acknowledgement_path(dir, pane_id)
  return dir .. "/" .. pane_id .. ".ack"
end

--- The in-flight sidecar this process writes before renaming it into place.
--- The name carries this process's session token, so two WezTerm processes
--- acknowledging the same pane never delete each other's in-flight file while
--- it is being renamed. A bare "<id>.ack.tmp" was shared state.
local function acknowledgement_tmp_path(dir, pane_id)
  return acknowledgement_path(dir, pane_id) .. "." .. publication_session .. ".tmp"
end

local function marker_identity(raw, publication_id)
  if publication_id then return "publication\n" .. publication_id end
  return "raw\n" .. (raw or "")
end

local function read_acknowledgement(dir, pane_id)
  local file = io.open(acknowledgement_path(dir, pane_id), "r")
  if not file then return nil end
  local identity = file:read("*a")
  file:close()
  return identity
end

--- Log a message once per key, so a persistent failure does not fill the log
--- on every poll tick.
local function report_error_once(key, message)
  if reported_errors[key] then return end
  reported_errors[key] = true
  wezterm.log_error("wezterm-attention: " .. message)
end

local function clear_acknowledgement(dir, pane_id)
  local path = acknowledgement_path(dir, pane_id)
  local existing = io.open(path, "r")
  if not existing then return true end
  existing:close()

  local ok, err = os.remove(path)
  if ok then return true end
  report_error_once(
    "clear:" .. pane_id,
    "failed to remove acknowledgement " .. path .. ": " .. tostring(err))
  return false
end

local function write_acknowledgement(dir, pane_id, identity)
  local path = acknowledgement_path(dir, pane_id)
  local tmp = acknowledgement_tmp_path(dir, pane_id)
  os.remove(tmp)
  local file, open_err = io.open(tmp, "w")
  if not file then
    report_error_once(
      "write:" .. pane_id,
      "failed to write acknowledgement " .. tmp .. ": " .. tostring(open_err))
    return false
  end

  local wrote, write_err = file:write(identity)
  local closed, close_err = file:close()
  if not wrote or not closed then
    os.remove(tmp)
    report_error_once(
      "write:" .. pane_id,
      "failed to finish acknowledgement " .. tmp .. ": " .. tostring(write_err or close_err))
    return false
  end

  -- No unlink of `path` here. os.rename replaces an existing file atomically on
  -- POSIX, so removing the old sidecar first would only open a window in which
  -- a concurrent reader sees no acknowledgement and re-displays a marker the
  -- user already looked at.
  local renamed, rename_err = os.rename(tmp, path)
  if not renamed then
    os.remove(tmp)
    report_error_once(
      "write:" .. pane_id,
      "failed to place acknowledgement " .. path .. ": " .. tostring(rename_err))
    return false
  end
  return true
end

local function acknowledgement_matches(dir, pane_id, raw, publication_id)
  local acknowledged = read_acknowledgement(dir, pane_id)
  if raw and acknowledged == marker_identity(raw, publication_id) then return true end

  -- Cleanup is best-effort. A stale sidecar never suppresses absent or
  -- mismatched canonical truth even when it cannot be removed.
  if acknowledged then clear_acknowledgement(dir, pane_id) end
  return false
end

local function read_effective_marker(dir, pane_id)
  -- A crash can strand only this process's own temporary sidecar. It was never
  -- authoritative. Another process's in-flight temp is not ours to remove: it
  -- may be one instant away from being renamed into place.
  os.remove(acknowledgement_tmp_path(dir, pane_id))
  local atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet =
    read_marker(dir, pane_id)
  if acknowledgement_matches(dir, pane_id, raw, publication_id) then return nil end
  return atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet
end

-- ── Review flag sidecar ─────────────────────────────────────────────────────
-- The Alt+B review flag is the user's own, and it lives in its own file:
--   <marker id>.review   {"publication_id":"<id>"}
-- It used to be written into the marker file as {"type":"review"}, which meant
-- it could only ever be set on a pane no process had written to. Every pane an
-- agent has run in carries a thinking/stop/notify marker, so on exactly the
-- panes the user wants to flag, Alt+B did nothing at all. As a sidecar the flag
-- coexists with writer-owned truth instead of competing for the same file.

local function review_path(dir, pane_id)
  return dir .. "/" .. pane_id .. ".review"
end

--- The in-flight flag this process writes before renaming it into place. Named
--- with this process's session token for the same reason the acknowledgement
--- temp is: two WezTerm processes flagging the same pane must not delete each
--- other's file mid-rename.
local function review_tmp_path(dir, pane_id)
  return review_path(dir, pane_id) .. "." .. publication_session .. ".tmp"
end

--- Is this pane flagged for review? The file's presence is the flag. Its body
--- is read by nothing, so content that no parser would accept still counts as
--- flagged: a truncated write must never silently drop a flag the user set by
--- hand.
local function review_flagged(dir, pane_id)
  local file = io.open(review_path(dir, pane_id), "r")
  if not file then return false end
  file:close()
  return true
end

local function write_review_flag(dir, pane_id)
  local path = review_path(dir, pane_id)
  local tmp = review_tmp_path(dir, pane_id)
  os.remove(tmp)

  local file, open_err = io.open(tmp, "w")
  if not file then
    wezterm.log_error(
      "wezterm-attention: failed to write review flag " .. tmp .. ": " .. tostring(open_err))
    return false
  end

  local wrote, write_err = file:write(
    '{"publication_id":"' .. next_publication_id() .. '"}')
  local closed, close_err = file:close()
  if not wrote or not closed then
    os.remove(tmp)
    wezterm.log_error(
      "wezterm-attention: failed to finish review flag " .. tmp .. ": "
        .. tostring(write_err or close_err))
    return false
  end

  local renamed, rename_err = os.rename(tmp, path)
  if not renamed then
    os.remove(tmp)
    wezterm.log_error(
      "wezterm-attention: failed to place review flag " .. path .. ": " .. tostring(rename_err))
    return false
  end
  return true
end

local function clear_review_flag(dir, pane_id)
  os.remove(review_tmp_path(dir, pane_id))
  local path = review_path(dir, pane_id)
  local existing = io.open(path, "r")
  if not existing then return true end
  existing:close()

  local ok, err = os.remove(path)
  if ok then return true end
  report_error_once(
    "clear-review:" .. pane_id,
    "failed to remove review flag " .. path .. ": " .. tostring(err))
  return false
end

--- Remove the writer's marker and the acknowledgement that referred to it, and
--- nothing else. This is what an expired marker costs: the subagent sidecar and
--- the user's review flag have their own lifetimes and neither of them aged out
--- because a spinner did.
local function remove_expired_marker(dir, pane_id)
  os.remove(dir .. "/" .. pane_id)
  clear_acknowledgement(dir, pane_id)
  os.remove(acknowledgement_tmp_path(dir, pane_id))
end

--- Remove every file this plugin knows a pane by. The subagent sidecar and the
--- review flag go with the marker: the pane they described is gone (the absence
--- sweep) or the caller asked for that pane's state to be cleared, and a
--- surviving sidecar would keep a "+N" or a ◆ on a tab whose pane is no longer
--- there to retract it.
local function remove_marker(dir, pane_id)
  remove_expired_marker(dir, pane_id)
  os.remove(subagents_path(dir, pane_id))
  clear_review_flag(dir, pane_id)
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
  if not value:match("^%d+$") then return nil end
  if #value > 1 and value:sub(1, 1) == "0" then return nil end
  return value
end

local function pane_method(pane, name)
  local fn = pane and pane[name]
  if type(fn) ~= "function" then return nil end
  local ok, result = pcall(fn, pane)
  if not ok then return nil end
  return result
end

--- The id under which this pane's markers are written, or nil when the pane
--- has published nothing and its local id cannot be trusted to name them.
function M.pane_marker_id(pane)
  if not pane then return nil end

  local vars = pane_method(pane, "get_user_vars")
  local published = type(vars) == "table" and canonical_pane_id(vars.WEZTERM_PANE) or nil
  if published then return published end

  -- The GUI's own local domain numbers its panes exactly as $WEZTERM_PANE
  -- does, so there the local id is the marker id even unpublished.
  if pane_method(pane, "get_domain_name") == "local" then
    return tostring(pane:pane_id())
  end
  return nil
end

-- ── In-memory cache ─────────────────────────────────────────────────────────
-- format-tab-title must not poll every pane from disk (it blocks the GUI
-- thread). update-status fills this cache; acknowledgement performs one confirmation
-- read only when acknowledging a cached terminal marker.

--- { [marker_id] = { type, frame?, observed_at?, raw, identity, source?,
---                    puppet, subagents } }
--- `type` is nil for a pane that has no marker but does have live subagents:
--- the entry exists only to carry the count, so every reader checks `type`
--- before drawing a marker glyph or a tint from it.
local attention_cache = {}

--- Local GUI pane id → marker id, or false for a pane that has published none.
--- format-tab-title is handed PaneInformation tables, which carry a pane_id and
--- no methods, so a marker id cannot be resolved there. poll() records the
--- mapping for every pane it walks and the renderer reads it back.
local marker_id_by_local = {}

--- Per window, the marker ids its panes carried on the previous poll, each
--- mapped to that pane's domain name. WezTerm has no pane-destroyed event, so
--- this is how poll() notices a pane closed; the domain is what tells a closed
--- pane apart from a detached domain.
local seen_marker_ids_by_window = {}

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

--- Marker IDs of one tab, translated from the local pane ids the GUI hands to
--- format-tab-title.
local function gui_tab_pane_ids(tab)
  local ids = {}
  for _, p in ipairs(tab.panes) do
    local local_id = tostring(p.pane_id)
    local mapped = marker_id_by_local[local_id]
    if mapped then
      ids[#ids + 1] = mapped
    elseif mapped == nil then
      -- No poll has walked this pane yet. The cache is empty for it either
      -- way, and on a local pane the local id is the marker id, so the
      -- untranslated id keeps single-machine setups rendering immediately.
      ids[#ids + 1] = local_id
    end
    -- mapped == false: a mux-client pane that has not published its
    -- $WEZTERM_PANE. Its local id names some other pane's markers, so it
    -- contributes nothing rather than something wrong.
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

  local subagents = 0

  for _, id in ipairs(pane_ids) do
    local cached = attention_cache[id]
    if cached then
      subagents = subagents + (cached.subagents or 0)
      -- A count-only entry has no type. It contributes its subagents and never
      -- competes for the tab's marker glyph.
      if cached.type then
        local pri = cfg_priority[cached.type] or 0
        if pri > best_priority then
          best_type     = cached.type
          best_priority = pri
          best_frame    = cached.frame
        end
      end
    end
  end

  if not best_type then
    -- No marker anywhere in the tab, but subagents of one of its panes are
    -- still running: show the count alone, tinted as stop. There is no marker
    -- type to name here, so `type` stays nil.
    if subagents > 0 then
      return { indicator = "+" .. subagents .. " ", type = nil, color = cfg_colors.stop }
    end
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

  -- The count rides inside the indicator's own trailing space, so "✓ " with two
  -- subagents renders "✓+2 " and the tab gains one column, not four.
  if subagents > 0 then
    indicator = indicator:gsub("%s+$", "") .. "+" .. subagents .. " "
  end

  return { indicator = indicator, type = best_type, color = cfg_colors[best_type] }
end

local function same_cached_attention(a, b)
  if not a or not b then return a == b end
  return a.type == b.type
    and a.frame == b.frame
    and (a.subagents or 0) == (b.subagents or 0)
    and (a.review == true) == (b.review == true)
end

--- Return the panes of the tab holding marker_id in a captured tab list.
local function tab_panes_containing(tabs, marker_id)
  if not marker_id then return nil end
  for _, tab in ipairs(tabs) do
    local panes = tab:panes()
    for _, p in ipairs(panes) do
      if M.pane_marker_id(p) == marker_id then
        return panes
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
--- Does the user's review flag outrank what the marker file says? Absent and
--- acknowledged markers (both arrive here as a nil type) are outranked by
--- anything, and a marker the configured priority order puts below `review`
--- — `thinking` by default — is too. `stop` and `notify` outrank it, so a
--- flagged pane that finishes still shows its ✓ first and falls back to the ◆
--- once that ✓ has been acknowledged.
local function review_outranks(atype)
  if not atype then return true end
  local priority = M._active_priority_map or {}
  return (priority[atype] or 0) < (priority.review or 0)
end

local function cache_marker_values(
    id, atype, frame, raw, publication_id, observed_now, source, puppet, subagents, flagged)
  subagents = tonumber(subagents) or 0
  -- A marker file whose own type is "review" was written by an older Alt+B,
  -- before the flag moved to its own file. It is the same user flag.
  local review = flagged == true or atype == "review"

  local effective = atype
  if review and review_outranks(atype) then
    effective = "review"
    frame = nil
  end

  if not effective then
    -- Nothing to show. A pane whose subagents are still working keeps a
    -- count-only entry so its tab can render "+N"; with nothing left to say,
    -- the entry goes away entirely, as it always has.
    if subagents > 0 then
      attention_cache[id] = {
        type        = nil,
        observed_at = observed_now,
        puppet      = false,
        subagents   = subagents,
        review      = review,
      }
    else
      attention_cache[id] = nil
    end
    return
  end

  if effective == "thinking" and frame == nil then
    local cfg_indicators = M._active_indicators or defaults.indicators
    local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
    frame = frame_for_now(observed_now, #frames)
  end

  -- `raw`, `identity`, `source` and `puppet` always describe the marker file,
  -- even when the review flag has taken over `type`. The marker is still the
  -- thing acknowledgement compares against and TTL ages out; the flag only
  -- decides what the tab shows.
  attention_cache[id] = {
    type        = effective,
    frame       = frame,
    observed_at = observed_now,
    raw         = raw,
    identity    = marker_identity(raw, publication_id),
    source      = source,
    puppet      = puppet == true,
    subagents   = subagents,
    review      = review,
  }
end

--- Re-read one pane's files and rebuild its cache entry. The Alt+B handler
--- changes a pane's state outside the poll loop, and dropping the entry instead
--- would blank a sibling's ✓ or a "+N" until the next tick.
local function refresh_cached_pane(dir, id, now)
  local atype, frame, _, _, raw, publication_id, source, puppet =
    read_effective_marker(dir, id)
  cache_marker_values(
    id, atype, frame, raw, publication_id, now, source, puppet,
    count_live_subagents(dir, id, now), review_flagged(dir, id))
end

local function acknowledge_focused_pane(pane_id, opts)
  local dir = (opts and opts.dir) or M._active_dir or defaults.dir
  local acknowledge_set = M._active_acknowledge_set or { stop = true, notify = true }
  local observed_now = (opts and opts.now_ms) or now_ms()

  local id = tostring(pane_id)
  local cached = attention_cache[id]
  if not (cached and acknowledge_set[cached.type]) then return "absent" end

  -- The count this tick's poll already read from disk. Acknowledgement is about
  -- the marker only, so it must hand the count back unchanged; recomputing zero
  -- here would drop the "+N" and make every tick a visible change.
  local subagents = cached.subagents or 0
  -- The user's review flag survives acknowledgement, and every cache write
  -- below has to carry it: a flagged pane whose ✓ the user just looked at falls
  -- back to showing the ◆, it does not go quiet.
  local flagged = cached.review == true

  local current_type, current_frame, _, _, raw, publication_id, source, puppet =
    read_marker(dir, id)
  if not current_type then
    clear_acknowledgement(dir, id)
    cache_marker_values(id, nil, nil, nil, nil, observed_now, nil, false, subagents, flagged)
    return "absent"
  end

  local current_identity = marker_identity(raw, publication_id)
  if cached.identity ~= current_identity then
    cache_marker_values(
      id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
      subagents, flagged)
    return "kept"
  end

  if not acknowledge_set[current_type] then
    clear_acknowledgement(dir, id)
    cache_marker_values(
      id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
      subagents, flagged)
    return "kept"
  end

  local write_ack = (opts and opts.write_acknowledgement) or write_acknowledgement
  if not write_ack(dir, id, current_identity) then
    cache_marker_values(
      id, current_type, current_frame, raw, publication_id, observed_now, source, puppet,
      subagents, flagged)
    return "failed"
  end

  -- A writer may replace or clear the marker while the sidecar is being
  -- written. Re-read effective truth before updating the cache: only the exact
  -- identity that was viewed is suppressed.
  local effective_type, effective_frame, _, _, effective_raw, effective_publication_id,
    effective_source, effective_puppet = read_effective_marker(dir, id)
  cache_marker_values(
    id, effective_type, effective_frame, effective_raw, effective_publication_id, observed_now,
    effective_source, effective_puppet, subagents, flagged)
  return effective_type and "kept" or "acknowledged"
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

-- ── Public API ──────────────────────────────────────────────────────────────

--- Read the cached attention state for a marker id (see M.pane_marker_id).
--- Returns (type, frame, source, puppet, subagents, review) or nil. `source` is
--- the marker's JSON `source` string when it carried one; `puppet` is true only
--- when the marker set `"puppet": true`; `subagents` is how many of the pane's
--- subagents ran a tool call in the last ten minutes, 0 when none; `review` is
--- true when the user has flagged the pane with Alt+B.
---
--- `type` is the effective one: it is `review` when the flag outranks the
--- marker file, and the marker's own type when that outranks the flag — in
--- which case the flag is still reported by the sixth return.
---
--- A pane with live subagents and no marker returns (nil, nil, nil, false, n):
--- the count is real even though there is no marker type to report.
function M.get_attention(marker_id, opts)
  local id = tostring(marker_id)
  if opts and opts.dir then
    local atype, frame, _, _, _, _, source, puppet = read_effective_marker(opts.dir, id)
    local now = (opts and opts.now_ms) or now_ms()
    local flagged = review_flagged(opts.dir, id) or atype == "review"
    if flagged and review_outranks(atype) then
      atype, frame = "review", nil
    end
    return atype, frame, source, puppet, count_live_subagents(opts.dir, id, now), flagged
  end
  local cached = attention_cache[id]
  if cached then
    return cached.type, cached.frame, cached.source, cached.puppet, cached.subagents or 0,
      cached.review == true
  end
  return nil
end

--- Remove the attention marker for a marker id (see M.pane_marker_id).
function M.remove_marker(marker_id, opts)
  local dir = (opts and opts.dir) or M._active_dir or defaults.dir
  local id = tostring(marker_id)
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
--- thinking markers are removed here by TTL; a closed pane's marker is removed
--- by the observed-then-gone sweep below.
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

  local mux_tabs = mux_win:tabs()
  local pane_ids = {}
  local before = {}
  local seen = {}             -- marker id → the domain name of the pane holding it
  local domains_present = {}  -- domain names this window still holds a pane of

  local now = (opts and opts.now_ms) or now_ms()
  local cfg_indicators = M._active_indicators or defaults.indicators
  local frames = cfg_indicators.thinking_frames or defaults.indicators.thinking_frames
  local frame_count = #frames

  for _, tab in ipairs(mux_tabs) do
    for _, p in ipairs(tab:panes()) do
      local domain = pane_method(p, "get_domain_name") or "?"
      domains_present[domain] = true

      local id = M.pane_marker_id(p)
      -- The renderer sees PaneInformation tables and cannot resolve this
      -- itself, so record the translation for it. `false` is a pane whose
      -- marker id is unknown: the renderer must skip it, not fall back.
      marker_id_by_local[tostring(p:pane_id())] = id or false

      -- A pane whose marker id is unknown is skipped entirely: reading or
      -- writing under its local id would address another pane's markers.
      if id then
        seen[id] = domain
        pane_ids[#pane_ids + 1] = id
        before[id] = attention_cache[id]
        local atype, frame, updated_at, marker_ttl_ms, raw, publication_id, source, puppet =
          read_marker(dir, id)
        -- One read of each sidecar per pane per tick, with this tick's clock,
        -- whether or not the pane has a marker.
        local subagents = count_live_subagents(dir, id, now)
        local flagged = review_flagged(dir, id)
        local acknowledged = acknowledgement_matches(dir, id, raw, publication_id)
        if atype then
          local cached = attention_cache[id]
          local observed_at = now
          if cached and cached.raw == raw and cached.observed_at then
            observed_at = cached.observed_at
          end

          local effective_updated_at = updated_at or observed_at
          local ttl = stale_ttl_ms(atype, marker_ttl_ms)
          if ttl and now - effective_updated_at > ttl then
            -- Only the writer's own state expires. The user's review flag and
            -- the pane's subagents kept their own time and are still true, so
            -- the pane keeps whatever they say about it.
            remove_expired_marker(dir, id)
            cache_marker_values(id, nil, nil, nil, nil, now, nil, false, subagents, flagged)
          elseif acknowledged then
            -- The marker was already seen. Its subagents may still be working
            -- and the user's flag still stands, so the entry survives as
            -- whatever they leave behind.
            cache_marker_values(
              id, nil, nil, nil, nil, observed_at, nil, false, subagents, flagged)
          else
            if atype == "thinking" and frame == nil then
              -- A frame is a function of time, never of poll count. The redraw
              -- action can induce immediate update-status events; every poll in
              -- the same bucket therefore projects the same frame and the chain
              -- terminates at the visible-change comparison below.
              frame = frame_for_now(now, frame_count)
            end
            cache_marker_values(
              id, atype, frame, raw, publication_id, observed_at, source, puppet, subagents,
              flagged)
          end
        else
          cache_marker_values(id, nil, nil, nil, nil, now, nil, false, subagents, flagged)
        end
      end
    end
  end

  -- WezTerm emits no pane-destroyed event, so a closed pane is detected by
  -- absence: an id this window reported on the previous poll and does not
  -- report now belonged to a pane that is gone, and its marker would otherwise
  -- outlive it forever.
  --
  -- One case is not a closed pane. Detaching a mux domain drops every one of
  -- its panes from this window in a single tick while those panes, and the
  -- processes writing their markers, keep running on the server. So an id is
  -- only swept when this window still holds some pane of that id's domain.
  local window_key = redraw_window_key(window)
  local previously_seen = seen_marker_ids_by_window[window_key]
  if previously_seen then
    for gone_id, gone_domain in pairs(previously_seen) do
      if not seen[gone_id] and domains_present[gone_domain] then
        pane_ids[#pane_ids + 1] = gone_id
        before[gone_id] = attention_cache[gone_id]
        remove_marker(dir, gone_id)
        attention_cache[gone_id] = nil
      end
    end
  end
  seen_marker_ids_by_window[window_key] = seen

  -- Everything below is about the focused window only. An unfocused window
  -- must neither acknowledge a marker its user has not seen nor be sent a key
  -- action, so an unfocused poll ends here with the cache correct.
  if not window:is_focused() then return end

  local current_active_pane = window:active_pane()
  if current_active_pane then
    local active_id = M.pane_marker_id(current_active_pane)
    if active_id and tab_panes_containing(mux_tabs, active_id) then
      acknowledge_focused_pane(active_id, { dir = dir, now_ms = now })
    end
  end

  -- The event pane can transport a redraw when no current pane is available,
  -- but it never authorizes acknowledgement.
  local action_pane = current_active_pane or (opts and opts.active_pane)

  local changed = false
  for _, id in ipairs(pane_ids) do
    if not same_cached_attention(before[id], attention_cache[id]) then
      changed = true
      break
    end
  end

  if changed then request_tab_bar_redraw(window, action_pane) end
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
  local request_redraw = opts.request_redraw
  if request_redraw == nil then request_redraw = defaults.request_redraw end
  M._active_request_redraw = request_redraw ~= false

  -- Once, at config load. The Alt+B handler used to do this on the GUI thread
  -- on every press; the directory does not change between presses.
  local quoted_dir = dir:gsub("'", [['\'']])
  os.execute("mkdir -p '" .. quoted_dir .. "'")

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

  local acknowledge_types = opts.auto_clear or defaults.acknowledge_types
  local priority   = opts.priority   or defaults.priority
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

  -- Nothing registers on "pane-destroyed": WezTerm emits no such event, so the
  -- handler that used to be here never once fired and closed panes' markers
  -- were never removed. poll() detects a closed pane by absence instead.

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
        local target_id = M.pane_marker_id(pane)
        if not target_id then
          -- A mux-client pane that has not published its $WEZTERM_PANE. Its
          -- local id names some other pane's marker file, so there is nothing
          -- here that can be safely written or removed.
          report_error_once("review-unknown-pane",
            "cannot toggle review: this pane has not published its WEZTERM_PANE user var")
          return
        end
        local panes
        if mux_win then
          panes = tab_panes_containing(mux_win:tabs(), target_id)
        end
        panes = panes or { pane }

        -- Decide and act on disk truth, never the cache. poll() rebuilds the
        -- cache from files every tick, so the cache can lag a flag another
        -- window's handler just wrote, and a clear that skipped a pane would
        -- leave the tab lit and unclearable on the next poll.
        --
        -- A pane is flagged if its own sidecar is there, or if its marker file
        -- itself says "review" — that is how an older version of this plugin
        -- wrote the flag, and it is still the same user flag. Effective truth
        -- is what counts for the legacy case: a review marker that was somehow
        -- acknowledged shows nothing, and a press must then flag the pane
        -- rather than silently clear what the user cannot see.
        local function is_flagged(id)
          if review_flagged(dir, id) then return true end
          return read_effective_marker(dir, id) == "review"
        end

        local has_flag = false
        for _, p in ipairs(panes) do
          local id = M.pane_marker_id(p)
          if id and is_flagged(id) then
            has_flag = true
            break
          end
        end

        -- Tab already flagged → clear the flag from all its panes. Only the
        -- flag is removed: a sibling's thinking/stop/notify marker belongs to
        -- its writer, and dropping one would drop a completion or failure the
        -- user never saw.
        if has_flag then
          for _, p in ipairs(panes) do
            local id = M.pane_marker_id(p)
            if id then
              local cleared = review_flagged(dir, id) and clear_review_flag(dir, id)
              if read_effective_marker(dir, id) == "review" then
                -- The legacy shape. Removing the marker file is the only way to
                -- clear a flag that was written into it, and a marker whose
                -- type is "review" can only ever have been this keybind's.
                remove_expired_marker(dir, id)
                cleared = true
              end
              if cleared then refresh_cached_pane(dir, id, now_ms()) end
            end
          end
          request_tab_bar_redraw(win, pane)
          return
        end

        -- Tab not flagged → flag the focused pane (the active pane of its tab,
        -- always a member of `panes`, so flag and clear stay symmetric).
        --
        -- The flag is written to its own "<id>.review" file, so it never
        -- touches writer-owned truth and needs no permission from it. It used
        -- to be written into the marker file, guarded against overwriting a
        -- process marker — which meant it could only be set on a pane no agent
        -- had ever run in, and pressing Alt+B in an agent pane did nothing.
        if write_review_flag(dir, target_id) then
          refresh_cached_pane(dir, target_id, now_ms())
          request_tab_bar_redraw(win, pane)
        end
      end),
    })
  end
end

-- Internal seams, exposed for the LuaJIT specs only. Not public API.
M._internal = {
  acknowledge_focused_pane = acknowledge_focused_pane,
  resolve_visible_attention = resolve_visible_attention,
  acknowledgement_tmp_path = acknowledgement_tmp_path,
  review_tmp_path = review_tmp_path,
}

return M
