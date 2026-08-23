local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = source:match("^(.*)/tests/auto_clear_spec.lua$") or "."

local function shell_quote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

local test_dir = os.tmpname()
os.remove(test_dir)
assert(os.execute("mkdir -p " .. shell_quote(test_dir)) == 0)

local logged_errors = {}

local function drain_errors()
  local drained = {}
  for i, message in ipairs(logged_errors) do
    drained[i] = message
    logged_errors[i] = nil
  end
  return drained
end

local handlers = {}
local wezterm = {
  home_dir = test_dir,
  action_callback = function(callback) return callback end,
  action = {
    -- Recorded, not executed. What the real action does to a live WezTerm is
    -- the subject of the focused-window rehearsal, not of this file.
    ActivateTabRelative = function(delta)
      return { ActivateTabRelative = delta }
    end,
  },
  json_parse = function(content)
    return {
      type = content:match('"type"%s*:%s*"([^"]+)"'),
      frame = tonumber(content:match('"frame"%s*:%s*(%d+)')),
      updated_at = tonumber(content:match('"updated_at"%s*:%s*(%d+)')),
      updated_at_ms = tonumber(content:match('"updated_at_ms"%s*:%s*(%d+)')),
      ttl_ms = tonumber(content:match('"ttl_ms"%s*:%s*(%d+)')),
      publication_id = content:match('"publication_id"%s*:%s*"([^"]+)"'),
    }
  end,
  log_error = function(message)
    table.insert(logged_errors, message)
  end,
  on = function(event, callback)
    handlers[event] = handlers[event] or {}
    table.insert(handlers[event], callback)
  end,
}

package.preload.wezterm = function() return wezterm end

local attention = dofile(repo_root .. "/plugin/init.lua")
attention.apply_to_config({}, {
  auto_poll = false,
  dir = test_dir,
  review_key = false,
})

local format_tab_title = assert(
  handlers["format-tab-title"] and handlers["format-tab-title"][1],
  "format-tab-title handler was not registered"
)

local pane_destroyed = assert(
  handlers["pane-destroyed"] and handlers["pane-destroyed"][1],
  "pane-destroyed handler was not registered"
)

local internal = assert(attention._internal, "internal seams were not exposed")

-- ── Fixtures ────────────────────────────────────────────────────────────────

local function write_marker(pane_id, marker_type, publication_id)
  local file = assert(io.open(test_dir .. "/" .. pane_id, "w"))
  if publication_id then
    file:write(string.format(
      '{"type":"%s","publication_id":"%s"}', marker_type, publication_id))
  else
    file:write(string.format('{"type":"%s"}', marker_type))
  end
  file:close()
end

local function path_exists(path)
  local file = io.open(path, "r")
  if not file then return false end
  file:close()
  return true
end

local function marker_exists(pane_id)
  return path_exists(test_dir .. "/" .. pane_id)
end

local function acknowledgement_exists(pane_id)
  return path_exists(test_dir .. "/" .. pane_id .. ".ack")
end

local function write_acknowledgement_file(pane_id, identity)
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".ack", "w"))
  file:write(identity)
  file:close()
end

local function mux_pane(pane_id)
  return {
    pane_id = function() return pane_id end,
  }
end

local function gui_pane(pane_id)
  return {
    pane_id = pane_id,
    title = "pane-" .. pane_id,
  }
end

--- A GUI window double. It records the actions the plugin performs and every
--- status or title write it attempts, so a test can assert what the plugin
--- left alone as readily as what it changed.
---
--- spec = {
---   tabs           = { { pane_id, ... }, ... },
---   focused        = boolean,
---   active_pane_id = pane_id | nil,
---   action_error   = string | nil,   -- make perform_action throw
---   on_action      = function | nil, -- observe or re-enter perform_action
---   omit           = { method_name = true },  -- pretend an older build
--- }
local next_window_id = 0

local function window_double(spec)
  next_window_id = next_window_id + 1
  local assigned_window_id = spec.window_id or next_window_id
  local mux_tabs = {}
  for _, pane_ids in ipairs(spec.tabs or {}) do
    local panes = {}
    for _, pane_id in ipairs(pane_ids) do
      table.insert(panes, mux_pane(pane_id))
    end
    table.insert(mux_tabs, {
      panes = function() return panes end,
      panes_with_info = function()
        local infos = {}
        for _, pane in ipairs(panes) do
          infos[#infos + 1] = { pane = pane, is_zoomed = false }
        end
        return infos
      end,
    })
  end

  local w = {
    actions           = {},
    status_writes     = {},
    title_writes      = {},
    active_pane_calls = 0,
    action_calls      = 0,
  }

  function w.mux_window()
    return { tabs = function() return mux_tabs end }
  end

  function w.window_id()
    return assigned_window_id
  end

  function w.is_focused()
    if spec.on_focus_check then spec.on_focus_check() end
    return spec.focused == true
  end

  function w.active_pane()
    w.active_pane_calls = w.active_pane_calls + 1
    if spec.active_pane_id == nil then return nil end
    return mux_pane(spec.active_pane_id)
  end

  function w.perform_action(_, action, pane)
    w.action_calls = w.action_calls + 1
    if spec.action_error then error(spec.action_error, 0) end
    table.insert(w.actions, { action = action, pane_id = pane and pane:pane_id() })
    if spec.on_action then spec.on_action(w, action, pane) end
  end

  function w.set_left_status(_, text) table.insert(w.status_writes, text) end
  function w.set_right_status(_, text) table.insert(w.status_writes, text) end
  function w.set_title(_, text) table.insert(w.title_writes, text) end

  for name in pairs(spec.omit or {}) do w[name] = nil end

  return w
end

--- Poll a single unfocused tab. Used wherever a test only needs the cache
--- filled and wants no acknowledgement or redraw in the way.
local function poll(pane_ids)
  attention.poll(window_double({ tabs = { pane_ids }, focused = false }))
end

--- Poll as the focused window, returning the double so the test can inspect
--- the recorded actions.
local function poll_focused(spec)
  local w = window_double({
    tabs           = spec.tabs,
    focused        = true,
    active_pane_id = spec.active_pane_id,
    action_error   = spec.action_error,
    on_action      = spec.on_action,
    omit           = spec.omit,
    on_focus_check = spec.on_focus_check,
  })
  attention.poll(w, spec.opts)
  return w
end

local function tab(active_pane_id, sibling_pane_id, is_active)
  local active_pane = gui_pane(active_pane_id)
  local sibling_pane = gui_pane(sibling_pane_id)
  return {
    active_pane = active_pane,
    is_active = is_active,
    panes = { active_pane, sibling_pane },
    tab_index = 0,
  }
end

local passed = 0
local failed = 0

local function test(name, callback)
  drain_errors()
  local ok, err = pcall(callback)
  if ok and #logged_errors > 0 then
    ok, err = false, "unexpected wezterm.log_error: " .. tostring(logged_errors[1])
  end
  if ok then
    passed = passed + 1
    io.write("ok - " .. name .. "\n")
  else
    failed = failed + 1
    io.write("not ok - " .. name .. "\n  " .. tostring(err) .. "\n")
  end
end

-- ── U1: visible-attention projection ────────────────────────────────────────

test("GUI and mux adapters project the same pane IDs in the same order", function()
  local gui_tab = tab(701, 702, false)
  local mux_tab = {
    panes = function() return { mux_pane(701), mux_pane(702) } end,
    panes_with_info = function()
      return {
        { pane = mux_pane(701), is_zoomed = false },
        { pane = mux_pane(702), is_zoomed = false },
      }
    end,
  }

  local gui_ids = internal.gui_tab_pane_ids(gui_tab)
  local mux_ids = internal.mux_tab_pane_ids(mux_tab)

  assert(#gui_ids == 2 and #mux_ids == 2, "both adapters should return two pane IDs")
  assert(gui_ids[1] == mux_ids[1] and gui_ids[2] == mux_ids[2], "adapter outputs should agree")
  assert(gui_ids[1] == "701", "pane IDs should be strings, got " .. tostring(gui_ids[1]))
end)

test("mux projection matches the GUI pane set while a split is zoomed", function()
  local mux_tab = {
    panes = function() return { mux_pane(703), mux_pane(704) } end,
    panes_with_info = function()
      return {
        { pane = mux_pane(703), is_zoomed = false },
        { pane = mux_pane(704), is_zoomed = true },
      }
    end,
  }
  local gui_tab = { panes = { gui_pane(704) } }

  local gui_ids = internal.gui_tab_pane_ids(gui_tab)
  local mux_ids = internal.mux_tab_pane_ids(mux_tab)

  assert(#mux_ids == 1 and mux_ids[1] == "704",
    "a zoomed mux tab should project only pane 704")
  assert(#gui_ids == 1 and gui_ids[1] == mux_ids[1],
    "GUI and mux projections should agree under zoom")
end)

test("mux projection falls back once when zoom metadata is unavailable", function()
  local legacy_tab = { panes = function() return { mux_pane(705), mux_pane(706) } end }

  local first = internal.mux_tab_pane_ids(legacy_tab)
  local second = internal.mux_tab_pane_ids(legacy_tab)
  assert(#first == 2 and #second == 2, "legacy fallback should retain every pane")

  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("panes_with_info", 1, true),
    "the compatibility fallback should log once, got " .. tostring(errors[1]))
end)

test("projection returns the highest-priority cached pane and ignores uncached ones", function()
  write_marker(711, "thinking")
  write_marker(712, "notify")
  poll({ 711, 712 })

  local visible = internal.resolve_visible_attention({ "711", "712", "799" })
  assert(visible.type == "notify", "notify outranks thinking, got " .. tostring(visible.type))
  assert(visible.indicator == "! ", "indicator should be the notify glyph")
  assert(visible.color == "#240f16", "color should be the notify tint")

  local empty = internal.resolve_visible_attention({ "799" })
  assert(empty.type == nil and empty.indicator == "" and empty.color == nil,
    "a pane with no cache entry should project no attention")
end)

test("visible equality hides changes no tab title would show", function()
  write_marker(721, "notify")
  write_marker(722, "notify")
  poll({ 721, 722 })

  local one  = internal.resolve_visible_attention({ "721" })
  local both = internal.resolve_visible_attention({ "721", "722" })
  assert(internal.same_visible_attention(one, both),
    "a second pane of the same type changes nothing a viewer can see")

  -- 722 drops from notify to stop while 721 still shows notify: the losing
  -- pane changed, the tab did not.
  write_marker(722, "stop")
  poll({ 721, 722 })
  local masked = internal.resolve_visible_attention({ "721", "722" })
  assert(internal.same_visible_attention(both, masked),
    "a change behind the winning pane should compare equal")

  -- 721 drops from notify to review, so stop on 722 becomes the winner.
  write_marker(721, "review")
  poll({ 721, 722 })
  local changed = internal.resolve_visible_attention({ "721", "722" })
  assert(changed.type == "stop", "stop should outrank review, got " .. tostring(changed.type))
  assert(not internal.same_visible_attention(masked, changed),
    "a change to the winning type should compare unequal")
end)

test("generated thinking frames are stable inside one wall-clock bucket", function()
  write_marker(731, "thinking")
  local w = window_double({ tabs = { { 731 } }, focused = false })

  attention.poll(w, { now_ms = 1000 })
  local _, f0 = attention.get_attention(731)
  attention.poll(w, { now_ms = 1500 })
  local _, f1 = attention.get_attention(731)
  attention.poll(w, { now_ms = 1999 })
  local _, f2 = attention.get_attention(731)
  assert(f0 == 1 and f1 == 1 and f2 == 1,
    "one bucket should keep one frame but got "
      .. tostring(f0) .. "," .. tostring(f1) .. "," .. tostring(f2))

  attention.poll(w, { now_ms = 2000 })
  local _, next_frame = attention.get_attention(731)
  assert(next_frame == 2, "crossing the bucket should advance to frame 2, got " .. tostring(next_frame))
end)

test("time-derived frames preserve the public get_attention return shape", function()
  write_marker(732, "thinking")
  local w = window_double({ tabs = { { 732 } }, focused = false })

  attention.poll(w, { now_ms = 3000 })
  local state, frame = attention.get_attention(732)

  assert(state == "thinking", "the first return remains the marker type")
  assert(frame == 3, "the second return remains the derived frame, got " .. tostring(frame))
end)

test("Lua accepts the publication ID marker shape published by Pi", function()
  local file = assert(io.open(test_dir .. "/733", "w"))
  file:write('{"type":"notify","source":"pi","publication_id":"pi-publication",'
    .. '"updated_at":1000,"label":"done","unknown":"ignored"}')
  file:close()

  attention.poll(
    window_double({ tabs = { { 733 } }, focused = false }),
    { now_ms = 1000000 })

  assert(attention.get_attention(733) == "notify",
    "Pi's publication ID and extra fields must not change the marker type")
end)

test("a coarse clock clamps generated frames to one-second buckets", function()
  assert(internal.clock_resolution_ms() == 1000,
    "the LuaJIT harness has no wezterm.time and should bind the coarse fallback")
  assert(internal.effective_frame_interval_ms() == 1000,
    "the frame interval must not claim finer resolution than its clock")
end)

test("a later high-resolution clock failure binds the coarse fallback once", function()
  local clock_calls = 0
  wezterm.time = {
    now = function()
      return {
        format_utc = function()
          clock_calls = clock_calls + 1
          if clock_calls == 1 then return "1000000000000" end
          error("clock unavailable", 0)
        end,
      }
    end,
  }

  local ok, err = pcall(function()
    local clocked = dofile(repo_root .. "/plugin/init.lua")
    clocked.apply_to_config({}, {
      auto_poll = false,
      dir = test_dir,
      review_key = false,
      frame_interval_ms = 10,
    })
    assert(clocked._internal.clock_resolution_ms() == 1, "load probe should bind the fine clock")

    write_marker(734, "thinking")
    local w = window_double({ tabs = { { 734 } }, focused = false })
    clocked.poll(w)
    assert(clocked._internal.clock_resolution_ms() == 1000,
      "runtime failure should switch to coarse resolution")
    local errors = drain_errors()
    assert(#errors == 1 and errors[1]:find("high-resolution clock failed", 1, true),
      "the fallback should log once")

    clocked.poll(w)
    assert(clock_calls == 2, "the failed high-resolution clock must not be retried")
    assert(#drain_errors() == 0, "the bound fallback should not log again")
  end)
  wezterm.time = nil
  if not ok then error(err, 0) end
end)

-- ── U2: read-only rendering ─────────────────────────────────────────────────

test("neither renderer clears a marker, even on the active tab", function()
  write_marker(101, "stop")
  write_marker(102, "notify")
  poll({ 101, 102 })

  format_tab_title(tab(101, 102, true))
  attention.wrap_title_formatter(function() return "custom title" end)(tab(101, 102, true))

  assert(marker_exists(101), "the active pane's marker must survive rendering")
  assert(marker_exists(102), "the sibling pane's marker must survive rendering")
  assert(attention.get_attention(101) == "stop", "rendering must not touch the cache")
  assert(attention.get_attention(102) == "notify", "rendering must not touch the cache")
end)

test("the tab renders the highest-priority pane, sibling included", function()
  write_marker(111, "review")
  write_marker(112, "stop")
  poll({ 111, 112 })

  local rendered = format_tab_title(tab(111, 112, true))

  assert(type(rendered) == "table", "an attention tab should carry a tint")
  assert(rendered[1].Background.Color == "#12271c", "tint should be the stop color")
  assert(rendered[2].Text:find("✓ ", 1, true), "stop outranks review and should be shown")
end)

test("both renderers project the same attention for the same tab", function()
  write_marker(741, "review")
  write_marker(742, "stop")
  poll({ 741, 742 })

  local wrapper_attention
  local wrapper = attention.wrap_title_formatter(function(_, ctx)
    wrapper_attention = ctx.attention
    return "custom title"
  end)
  local default_rendered = format_tab_title(tab(741, 742, false))
  local wrapped_rendered = wrapper(tab(741, 742, false))

  assert(type(default_rendered) == "table" and type(wrapped_rendered) == "table",
    "both renderers should tint the tab")
  assert(default_rendered[1].Background.Color == wrapped_rendered[1].Background.Color,
    "both renderers should pick the same tint")
  assert(default_rendered[2].Text:find("✓ ", 1, true), "default renderer should show stop")
  assert(wrapped_rendered[2].Text:find("✓ ", 1, true), "wrapper should show stop")
  assert(wrapper_attention[1] == "✓ " and wrapper_attention[2] == "stop"
    and wrapper_attention[3] == "#12271c",
    "ctx.attention should still be indicator, type, color")
end)

-- ── U2: focus-aware acknowledgement ─────────────────────────────────────────

test("a focused poll acknowledges only the active pane", function()
  write_marker(801, "notify", "rev-801")
  write_marker(802, "stop")

  local w = poll_focused({ tabs = { { 801, 802 } }, active_pane_id = 801 })

  assert(marker_exists(801), "acknowledgement must never remove the canonical marker")
  assert(acknowledgement_exists(801), "the viewed marker identity should be recorded")
  assert(marker_exists(802), "an unfocused sibling marker should remain")
  assert(attention.get_attention(801) == nil, "the acknowledged cache entry should be gone")
  assert(attention.get_attention(802) == "stop", "the sibling cache entry should remain")
  assert(#w.status_writes == 0 and #w.title_writes == 0,
    "the plugin must not write status or title text")
end)

test("an unfocused poll acknowledges nothing and performs no action", function()
  write_marker(811, "notify")
  write_marker(812, "stop")

  local w = window_double({ tabs = { { 811, 812 } }, focused = false, active_pane_id = 811 })
  attention.poll(w)

  assert(marker_exists(811), "a background window must not acknowledge its active pane")
  assert(marker_exists(812), "a background window must not acknowledge any pane")
  assert(attention.get_attention(811) == "notify", "the cache should still be filled")
  assert(#w.actions == 0, "an unfocused window must never be sent a key action")
end)

test("visiting the sibling pane acknowledges its retained marker", function()
  write_marker(401, "stop")
  write_marker(402, "notify")

  poll_focused({ tabs = { { 401, 402 } }, active_pane_id = 401 })
  assert(marker_exists(402), "the unvisited sibling marker should remain")

  poll_focused({ tabs = { { 401, 402 } }, active_pane_id = 402 })
  assert(marker_exists(402), "acknowledgement must leave canonical writer truth intact")
  assert(acknowledgement_exists(402), "the visited sibling should gain an acknowledgement")
  assert(attention.get_attention(402) == nil, "the acknowledged sibling should disappear from effective cache")
end)

test("a new publication ID releases an older acknowledgement", function()
  write_marker(931, "notify", "publication-a")
  poll_focused({ tabs = { { 930, 931 } }, active_pane_id = 931 })
  assert(attention.get_attention(931) == nil, "publication A should be acknowledged")

  write_marker(931, "notify", "publication-b")
  poll({ 930, 931 })

  assert(attention.get_attention(931) == "notify", "publication B must be visible")
  assert(not acknowledgement_exists(931), "the stale publication-A acknowledgement should be removed")
end)

test("legacy raw identity remains compatible and changed bytes become visible", function()
  write_marker(932, "stop")
  poll_focused({ tabs = { { 930, 932 } }, active_pane_id = 932 })
  assert(attention.get_attention(932) == nil, "legacy stop should be acknowledged by raw identity")

  write_marker(932, "notify")
  poll({ 930, 932 })

  assert(attention.get_attention(932) == "notify", "changed legacy bytes must become visible")
  assert(not acknowledgement_exists(932), "changed raw identity should release the sidecar")
end)

test("a stale acknowledgement never suppresses mismatched canonical truth when cleanup fails", function()
  write_marker(933, "notify", "publication-b")
  write_acknowledgement_file(933, "publication\npublication-a")

  local ack_path = test_dir .. "/933.ack"
  local real_remove = os.remove
  os.remove = function(path)
    if path == ack_path then return nil, "permission denied" end
    return real_remove(path)
  end
  poll({ 933 })
  os.remove = real_remove

  assert(attention.get_attention(933) == "notify", "mismatched current truth must remain visible")
  assert(acknowledgement_exists(933), "precondition: failed cleanup leaves the stale sidecar")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("failed to remove acknowledgement", 1, true),
    "cleanup failure should log once, got " .. tostring(errors[1]))
end)

test("canonical absence clears acknowledgement without resurrecting state", function()
  write_marker(941, "stop", "publication-a")
  poll_focused({ tabs = { { 940, 941 } }, active_pane_id = 941 })
  assert(acknowledgement_exists(941), "the sidecar should exist after acknowledgement")

  os.remove(test_dir .. "/941")
  poll({ 940, 941 })

  assert(attention.get_attention(941) == nil, "absence remains clear")
  assert(not acknowledgement_exists(941), "absence should release stale acknowledgement")
end)

test("direct disk reads respect acknowledgement identity", function()
  write_marker(942, "notify", "publication-a")
  poll_focused({ tabs = { { 940, 942 } }, active_pane_id = 942 })

  assert(attention.get_attention(942, { dir = test_dir }) == nil,
    "a direct read should expose effective attention, not acknowledged physical state")
end)

test("acknowledgement survives a plugin reload without moving canonical truth", function()
  write_marker(947, "notify", "publication-a")
  poll_focused({ tabs = { { 940, 947 } }, active_pane_id = 947 })
  assert(acknowledgement_exists(947), "precondition: sidecar exists")

  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  reloaded.poll(window_double({ tabs = { { 940, 947 } }, focused = false }))

  assert(marker_exists(947), "reload must leave canonical marker untouched")
  assert(reloaded.get_attention(947) == nil, "reload should honor the durable acknowledgement")
end)

test("acknowledgement write failure leaves marker visible and cache truthful", function()
  write_marker(943, "notify", "publication-a")
  poll({ 943 })

  local outcome = internal.acknowledge_focused_pane(943, {
    dir = test_dir,
    now_ms = 1000,
    write_acknowledgement = function() return false end,
  })

  assert(outcome == "failed", "the failure should be explicit, got " .. tostring(outcome))
  assert(marker_exists(943), "acknowledgement failure must retain canonical truth")
  assert(not acknowledgement_exists(943), "a failed write must not install a sidecar")
  assert(attention.get_attention(943) == "notify", "the marker must remain visible")
end)

test("acknowledgement rename failure removes its temp and leaves truth visible", function()
  write_marker(948, "notify", "publication-a")
  poll({ 948 })

  local ack_path = test_dir .. "/948.ack"
  local real_rename = os.rename
  os.rename = function(from, to)
    if to == ack_path then return nil, "permission denied" end
    return real_rename(from, to)
  end
  local ok, outcome = pcall(internal.acknowledge_focused_pane, 948, {
    dir = test_dir,
    now_ms = 1000,
  })
  os.rename = real_rename

  assert(ok and outcome == "failed", "rename failure should return failed")
  assert(marker_exists(948), "canonical truth must remain")
  assert(not acknowledgement_exists(948), "failed rename must not install acknowledgement")
  assert(not path_exists(ack_path .. ".tmp"), "failed rename must remove its temp file")
  assert(attention.get_attention(948) == "notify", "failed acknowledgement must remain visible")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("failed to place acknowledgement", 1, true),
    "the real rename failure should be logged")
end)

test("public removal clears canonical marker and acknowledgement sidecar", function()
  write_marker(944, "stop", "publication-a")
  poll_focused({ tabs = { { 940, 944 } }, active_pane_id = 944 })
  assert(acknowledgement_exists(944), "precondition: sidecar exists")

  attention.remove_marker(944, { dir = test_dir })

  assert(not marker_exists(944), "public removal should remove canonical marker")
  assert(not acknowledgement_exists(944), "public removal should remove sidecar")
  assert(attention.get_attention(944) == nil, "public removal should clear cache")
end)

test("pane destruction clears canonical marker and acknowledgement sidecar", function()
  write_marker(945, "notify", "publication-a")
  poll_focused({ tabs = { { 940, 945 } }, active_pane_id = 945 })
  assert(acknowledgement_exists(945), "precondition: sidecar exists")

  pane_destroyed(nil, mux_pane(945))

  assert(not marker_exists(945), "pane destruction should remove canonical marker")
  assert(not acknowledgement_exists(945), "pane destruction should remove sidecar")
  assert(attention.get_attention(945) == nil, "pane destruction should clear cache")
end)

test("TTL cleanup removes canonical marker and acknowledgement sidecar", function()
  local file = assert(io.open(test_dir .. "/946", "w"))
  file:write('{"type":"notify","publication_id":"publication-a",'
    .. '"updated_at_ms":1000000000000,"ttl_ms":1000}')
  file:close()
  local w = window_double({ tabs = { { 940, 946 } }, focused = true, active_pane_id = 946 })
  attention.poll(w, { now_ms = 1000000000000 })
  assert(acknowledgement_exists(946), "precondition: sidecar exists")

  attention.poll(
    window_double({ tabs = { { 940, 946 } }, focused = false }),
    { now_ms = 1000000001001 })

  assert(not marker_exists(946), "expired canonical marker should be removed")
  assert(not acknowledgement_exists(946), "TTL cleanup should remove sidecar")
  assert(attention.get_attention(946) == nil, "expired attention should leave cache")
end)

test("a stale update-status pane is never destructive authority", function()
  write_marker(951, "notify", "publication-a")
  local w = window_double({ tabs = { { 951, 952 } }, focused = true, active_pane_id = 952 })

  attention.poll(w, { active_pane = mux_pane(951) })

  assert(marker_exists(951), "the stale event pane's marker must remain")
  assert(not acknowledgement_exists(951), "the stale event pane must not be acknowledged")
  assert(attention.get_attention(951) == "notify", "the unseen notification must remain visible")
end)

test("acknowledgement never deletes a newer non-clearable marker", function()
  write_marker(501, "stop", "publication-a")
  write_marker(502, "notify")

  local rewrote = false
  poll_focused({
    tabs = { { 501, 502 } },
    active_pane_id = 501,
    on_focus_check = function()
      if rewrote then return end
      rewrote = true
      -- The poll has already cached publication A. Replace it before current
      -- active-pane acknowledgement reads physical truth.
      write_marker(501, "thinking", "publication-b")
    end,
  })

  assert(marker_exists(501), "the newer thinking marker should remain on disk")
  assert(attention.get_attention(501) == "thinking", "the cache should adopt the newer marker")
  assert(not acknowledgement_exists(501), "a non-clearable replacement must not be acknowledged")
end)

test("a focused window with no active pane acknowledges nothing", function()
  write_marker(821, "notify")

  local w = poll_focused({ tabs = { { 821 } }, active_pane_id = nil })

  assert(marker_exists(821), "with no active pane there is nothing to acknowledge")
  assert(attention.get_attention(821) == "notify", "the cache should still be filled")
  assert(#w.actions == 0, "there is no pane to perform an action through")
end)

-- ── U2: focus-safe redraw ───────────────────────────────────────────────────

test("a focused visible change requests exactly one redraw through the active pane", function()
  write_marker(831, "thinking")

  local w = poll_focused({ tabs = { { 830, 831 } }, active_pane_id = 830 })

  assert(#w.actions == 1, "one visible change should cost one action, got " .. #w.actions)
  assert(w.actions[1].action.ActivateTabRelative == 0,
    "the action must re-activate the current tab, changing no selection")
  assert(w.actions[1].pane_id == 830, "the action must run through the active pane")
  assert(#w.status_writes == 0 and #w.title_writes == 0,
    "redrawing must not write status or title text")
end)

test("an unchanged tab bar requests no redraw", function()
  write_marker(841, "thinking")
  poll({ 840, 841 })

  -- Pin the frame so the animation cannot manufacture a visible change.
  local before_frame = select(2, attention.get_attention(841))
  local file = assert(io.open(test_dir .. "/841", "w"))
  file:write(string.format('{"type":"thinking","frame":%d}', before_frame))
  file:close()
  poll({ 840, 841 })

  local w = poll_focused({ tabs = { { 840, 841 } }, active_pane_id = 840 })

  assert(#w.actions == 0, "nothing visible changed, so nothing should be redrawn")
end)

test("a change hidden behind a higher-priority pane requests no redraw", function()
  write_marker(851, "notify")
  write_marker(852, "notify")
  poll({ 850, 851, 852 })

  -- 852 falls from notify to stop; 851 still shows notify, so the tab does not
  -- change. 850 is the active pane and carries no marker, so nothing is
  -- acknowledged either.
  write_marker(852, "stop")
  local w = poll_focused({ tabs = { { 850, 851, 852 } }, active_pane_id = 850 })

  assert(#w.actions == 0, "a masked change should not cost a redraw, got " .. #w.actions)
  assert(attention.get_attention(852) == "stop", "the cache should still have followed the marker")
end)

test("a marker disappearing requests a redraw", function()
  write_marker(861, "notify")
  poll({ 860, 861 })

  os.remove(test_dir .. "/861")
  local w = poll_focused({ tabs = { { 860, 861 } }, active_pane_id = 860 })

  assert(attention.get_attention(861) == nil, "the cache should drop the removed marker")
  assert(#w.actions == 1, "attention vanishing is a visible change, got " .. #w.actions)
end)

test("animation redraws once per wall-clock bucket, not once per poll", function()
  write_marker(871, "thinking")

  local w = window_double({ tabs = { { 870, 871 } }, focused = true, active_pane_id = 870 })
  attention.poll(w, { now_ms = 1000 })
  assert(#w.actions == 1, "the marker appearing is the first visible change")

  attention.poll(w, { now_ms = 1000 })
  attention.poll(w, { now_ms = 1999 })
  assert(#w.actions == 1, "induced polls in one bucket must not redraw again")

  attention.poll(w, { now_ms = 2000 })
  assert(#w.actions == 2, "the next bucket is a new indicator, so one new redraw")
  assert(select(2, attention.get_attention(871)) == 2, "the frame should come from the new bucket")
end)

test("redraw-induced polls terminate inside the current frame bucket", function()
  write_marker(873, "thinking")

  local reentries = 0
  local current_now = 1000
  local w = window_double({
    tabs = { { 870, 873 } },
    focused = true,
    active_pane_id = 870,
    on_action = function(window)
      for _ = 1, 2 do
        reentries = reentries + 1
        attention.poll(window, { now_ms = current_now })
      end
    end,
  })

  attention.poll(w, { now_ms = 1000 })
  assert(reentries == 2, "the compatibility action should induce two nested polls in this double")
  assert(w.action_calls == 1, "neither nested poll may request another action")

  current_now = 2000
  attention.poll(w, { now_ms = current_now })
  assert(reentries == 4 and w.action_calls == 2,
    "the next bucket should permit exactly one more action")
end)

test("the per-window redraw budget caps feedback and logs once", function()
  local w = window_double({ tabs = { { 870, 872 } }, focused = true, active_pane_id = 870 })
  for i = 1, 8 do
    if i % 2 == 1 then
      write_marker(872, "notify")
    else
      os.remove(test_dir .. "/872")
    end
    attention.poll(w, { now_ms = 1000 })
  end

  assert(#w.actions == 4, "the default budget permits four redraws, got " .. #w.actions)
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("budget exhausted", 1, true),
    "budget refusal should log once, got " .. tostring(errors[1]))
end)

test("a failed redraw action leaves marker and cache truth intact", function()
  write_marker(881, "notify")

  local w = poll_focused({
    tabs           = { { 880, 881 } },
    active_pane_id = 880,
    action_error   = "wezterm exploded",
  })

  assert(#w.actions == 0, "the failed action should record nothing")
  assert(marker_exists(881), "a failed redraw must not touch the marker")
  assert(attention.get_attention(881) == "notify", "a failed redraw must not touch the cache")

  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("redraw failed", 1, true),
    "the failure should be logged once, got " .. tostring(errors[1]))

  write_marker(881, "stop")
  attention.poll(w, { now_ms = 2000 })
  assert(w.action_calls == 1, "a failed window should not retry the compatibility action")
  assert(#drain_errors() == 0, "a disabled window should not repeat the runtime error")
end)

test("a build without perform_action skips the redraw and mutates nothing", function()
  write_marker(891, "notify")

  local w = poll_focused({
    tabs           = { { 890, 891 } },
    active_pane_id = 890,
    omit           = { perform_action = true },
  })

  assert(marker_exists(891), "an unsupported redraw must not touch the marker")
  assert(attention.get_attention(891) == "notify", "the cache should still be correct")
  assert(#w.status_writes == 0 and #w.title_writes == 0,
    "status and title are never a fallback for a missing redraw API")

  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("perform_action", 1, true),
    "the missing method should be reported once, got " .. tostring(errors[1]))
end)

-- ── U2: window scoping and composition root ─────────────────────────────────

test("polling one window never removes another window's cache entries", function()
  write_marker(901, "thinking")
  write_marker(902, "thinking")

  poll_focused({ tabs = { { 901 } }, active_pane_id = 901 })
  assert(attention.get_attention(901) == "thinking", "window A's pane should be cached")

  poll_focused({ tabs = { { 902 } }, active_pane_id = 902 })
  assert(attention.get_attention(901) == "thinking", "window A's entry must survive window B's poll")
  assert(attention.get_attention(902) == "thinking", "window B's pane should be cached")
end)

test("poll resolves current pane even when the event pane is supplied", function()
  write_marker(911, "notify", "publication-a")

  local w = window_double({ tabs = { { 911 } }, focused = true, active_pane_id = 911 })
  attention.poll(w, { active_pane = mux_pane(911) })

  assert(w.active_pane_calls == 1, "destructive authority must be resolved at use time")
  assert(marker_exists(911), "acknowledgement must retain canonical writer truth")
  assert(acknowledgement_exists(911), "the current active pane should be acknowledged")
end)

test("the registered update-status handler resolves current pane before acknowledgement", function()
  -- A second, independent copy of the plugin: apply_to_config only registers
  -- handlers once per module instance.
  local second = dofile(repo_root .. "/plugin/init.lua")
  second.apply_to_config({}, { dir = test_dir, review_key = false })

  local update_status = handlers["update-status"][#handlers["update-status"]]
  write_marker(921, "notify", "publication-a")

  local w = window_double({ tabs = { { 921 } }, focused = true, active_pane_id = 921 })
  update_status(w, mux_pane(921))

  assert(w.active_pane_calls == 1, "the handler must not trust its captured event pane")
  assert(marker_exists(921), "the canonical marker should remain")
  assert(acknowledgement_exists(921), "the current active pane should be acknowledged")
end)

test("review toggles redraw only when the affected tab projection changes", function()
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir })
  local toggle = assert(config.keys and config.keys[1] and config.keys[1].action,
    "review key action was not registered")

  write_marker(971, "notify", "notify-971")
  write_marker(972, "review", "review-972")
  local masked = window_double({ tabs = { { 971, 972 } }, focused = true, active_pane_id = 972 })
  review.poll(window_double({ tabs = { { 971, 972 } }, focused = false }), { now_ms = 1000 })
  toggle(masked, mux_pane(972))
  assert(not marker_exists(972), "the review marker should still be removed")
  assert(masked.action_calls == 0, "notify still wins, so the tab did not visibly change")

  write_marker(973, "review", "review-973")
  local visible = window_double({ tabs = { { 973 } }, focused = true, active_pane_id = 973 })
  review.poll(window_double({ tabs = { { 973 } }, focused = false }), { now_ms = 1000 })
  toggle(visible, mux_pane(973))
  assert(not marker_exists(973), "the visible review marker should be removed")
  assert(visible.action_calls == 1, "removing visible review attention should redraw once")

  write_marker(974, "notify", "notify-974")
  local acknowledged = window_double({ tabs = { { 974 } }, focused = true, active_pane_id = 974 })
  review.poll(acknowledged, { now_ms = 2000 })
  assert(acknowledgement_exists(974), "precondition: notify is acknowledged")
  toggle(acknowledged, mux_pane(974))
  local marker = assert(io.open(test_dir .. "/974", "r"))
  local marker_bytes = marker:read("*a")
  marker:close()
  assert(marker_bytes:find('"type":"notify"', 1, true),
    "review must not overwrite acknowledged writer truth")

  local review_path = test_dir .. "/975"
  local real_rename = os.rename
  os.rename = function(from, to)
    if to == review_path then return nil, "permission denied" end
    return real_rename(from, to)
  end
  local failed = window_double({ tabs = { { 975 } }, focused = true, active_pane_id = 975 })
  local ok, toggle_err = pcall(toggle, failed, mux_pane(975))
  os.rename = real_rename
  assert(ok, "review rename failure should be contained: " .. tostring(toggle_err))
  assert(not marker_exists(975), "failed review publication must not create canonical state")
  assert(not path_exists(review_path .. ".tmp"), "failed review publication must remove its temp")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("failed to place review marker", 1, true),
    "the real review rename failure should be logged")
end)

os.execute("rm -rf " .. shell_quote(test_dir))

io.write(string.format("%d passed, %d failed\n", passed, failed))
if failed > 0 then os.exit(1) end
