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

--- The subagent sidecar's nested shape, for the json_parse double below.
--- Returns nil when the content carries no readable "agents" object, which is
--- what an empty or truncated file looks like to a real parser.
local function parse_agents(content)
  local body = content:match('"agents"%s*:%s*(%b{})')
  if not body then return nil end
  local agents = {}
  for agent_id, entry in body:gmatch('"([^"]+)"%s*:%s*(%b{})') do
    agents[agent_id] = {
      type    = entry:match('"type"%s*:%s*"([^"]+)"'),
      last_ms = tonumber(entry:match('"last_ms"%s*:%s*(%-?%d+)')),
    }
  end
  return agents
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
    -- The real json_parse raises on content that is not JSON. The plugin wraps
    -- every call in pcall, and that guard is only exercised if this double
    -- refuses the same input.
    if not content:match("^%s*{") then
      error("invalid json: " .. tostring(content), 0)
    end
    return {
      agents = parse_agents(content),
      type = content:match('"type"%s*:%s*"([^"]+)"'),
      frame = tonumber(content:match('"frame"%s*:%s*(%d+)')),
      updated_at = tonumber(content:match('"updated_at"%s*:%s*(%d+)')),
      updated_at_ms = tonumber(content:match('"updated_at_ms"%s*:%s*(%d+)')),
      ttl_ms = tonumber(content:match('"ttl_ms"%s*:%s*(%d+)')),
      publication_id = content:match('"publication_id"%s*:%s*"([^"]+)"'),
      source = content:match('"source"%s*:%s*"([^"]+)"'),
      puppet = content:match('"puppet"%s*:%s*true') ~= nil,
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

-- WezTerm emits no "pane-destroyed" event. Registering for one was a handler
-- that could never fire, so the plugin must not register it any more.
assert(handlers["pane-destroyed"] == nil,
  "the plugin must not register a handler for an event WezTerm never emits")

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

local function subagents_exists(pane_id)
  return path_exists(test_dir .. "/" .. pane_id .. ".agents")
end

local function review_flag_exists(pane_id)
  return path_exists(test_dir .. "/" .. pane_id .. ".review")
end

--- Write the review flag sidecar the way the Alt+B handler does.
local function write_review_flag_file(pane_id, publication_id)
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".review", "w"))
  file:write(string.format(
    '{"publication_id":"%s"}', publication_id or ("flag-" .. pane_id)))
  file:close()
end

--- Write the flag sidecar byte for byte, for shapes a fixture cannot express.
local function write_raw_review_flag(pane_id, content)
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".review", "w"))
  file:write(content)
  file:close()
end

--- Write a pane's subagent activity sidecar.
--- `entries` is a list of { id = string, type = string?, last_ms = number }.
local function write_subagents(pane_id, entries)
  local parts = {}
  for _, entry in ipairs(entries) do
    parts[#parts + 1] = string.format(
      '"%s":{"type":"%s","last_ms":%d}', entry.id, entry.type or "general-purpose", entry.last_ms)
  end
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".agents", "w"))
  file:write('{"agents":{' .. table.concat(parts, ",") .. "}}")
  file:close()
end

--- Write a sidecar byte for byte, for the shapes a fixture cannot express.
local function write_raw_subagents(pane_id, content)
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".agents", "w"))
  file:write(content)
  file:close()
end

local function write_acknowledgement_file(pane_id, identity)
  local file = assert(io.open(test_dir .. "/" .. pane_id .. ".ack", "w"))
  file:write(identity)
  file:close()
end

--- A mux pane double.
---
--- `spec.domain` is the pane's domain name ("local" unless a test says
--- otherwise) and `spec.published` is the value the pane has published as its
--- WEZTERM_PANE user var. A plain number therefore describes the ordinary
--- case: a local pane whose local id is also its marker id.
local function mux_pane(pane_id, spec)
  spec = spec or {}
  local user_vars = {}
  if spec.published ~= nil then user_vars.WEZTERM_PANE = tostring(spec.published) end
  return {
    pane_id = function() return pane_id end,
    get_domain_name = function() return spec.domain or "local" end,
    get_user_vars = function() return user_vars end,
  }
end

--- Tab entries in a window double are either a bare local pane id or
--- { id, domain, published }.
local function pane_from_entry(entry)
  if type(entry) == "table" then
    return mux_pane(entry.id, { domain = entry.domain, published = entry.published })
  end
  return mux_pane(entry)
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
--- }
local next_window_id = 0

local function window_double(spec)
  next_window_id = next_window_id + 1
  local assigned_window_id = spec.window_id or next_window_id
  local mux_tabs = {}
  for _, pane_ids in ipairs(spec.tabs or {}) do
    local panes = {}
    for _, entry in ipairs(pane_ids) do
      table.insert(panes, pane_from_entry(entry))
    end
    table.insert(mux_tabs, {
      panes = function() return panes end,
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
    return pane_from_entry(spec.active_pane_id)
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
    on_focus_check = spec.on_focus_check,
    window_id      = spec.window_id,
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
  assert(not path_exists(internal.acknowledgement_tmp_path(test_dir, "948")),
    "failed rename must remove its temp file")
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

test("public removal without opts uses the configured marker directory", function()
  local configured_dir = test_dir .. "/configured"
  assert(os.execute("mkdir -p " .. shell_quote(configured_dir)) == 0)
  local marker = assert(io.open(configured_dir .. "/949", "w"))
  marker:write('{"type":"notify","publication_id":"publication-a"}')
  marker:close()
  local ack = assert(io.open(configured_dir .. "/949.ack", "w"))
  ack:write("publication\npublication-a")
  ack:close()

  local configured = dofile(repo_root .. "/plugin/init.lua")
  configured.apply_to_config({}, {
    auto_poll = false,
    dir = configured_dir,
    review_key = false,
  })
  configured.remove_marker(949)

  assert(not path_exists(configured_dir .. "/949"), "configured marker should be removed")
  assert(not path_exists(configured_dir .. "/949.ack"),
    "configured acknowledgement should be removed")
end)

test("a pane that vanishes between polls has its marker and sidecar removed", function()
  write_marker(945, "notify", "publication-a")
  local window_id = 9450
  poll_focused({
    tabs = { { 940, 945 } },
    active_pane_id = 945,
    window_id = window_id,
  })
  assert(acknowledgement_exists(945), "precondition: sidecar exists")

  -- 945 is gone; 940 is still here, so its domain is still represented and the
  -- disappearance reads as a closed pane rather than a detached domain.
  attention.poll(window_double({
    tabs = { { 940 } }, focused = false, window_id = window_id,
  }))

  assert(not marker_exists(945), "a closed pane should lose its canonical marker")
  assert(not acknowledgement_exists(945), "a closed pane should lose its sidecar")
  assert(attention.get_attention(945) == nil, "a closed pane should leave the cache")
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

test("a same-type replacement during acknowledgement remains visible", function()
  write_marker(981, "notify", "publication-a")
  local replaced = false
  poll_focused({
    tabs = { { 980, 981 } },
    active_pane_id = 981,
    on_focus_check = function()
      if replaced then return end
      replaced = true
      write_marker(981, "notify", "publication-b")
    end,
  })

  assert(attention.get_attention(981) == "notify", "replacement B must remain visible")
  assert(not acknowledgement_exists(981), "replacement B must not be acknowledged unseen")
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

test("a masked cache change requests a harmless redraw", function()
  write_marker(851, "notify")
  write_marker(852, "notify")
  poll({ 850, 851, 852 })

  -- 852 falls from notify to stop; 851 still shows notify, so the tab does not
  -- change. 850 is the active pane and carries no marker, so nothing is
  -- acknowledged either.
  write_marker(852, "stop")
  local w = poll_focused({ tabs = { { 850, 851, 852 } }, active_pane_id = 850 })

  assert(#w.actions == 1, "a cache change should request one redraw, got " .. #w.actions)
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
  assert(reentries == 2, "the redraw action should induce two nested polls in this double")
  assert(w.action_calls == 1, "neither nested poll may request another action")

  current_now = 2000
  attention.poll(w, { now_ms = current_now })
  assert(reentries == 4 and w.action_calls == 2,
    "the next bucket should permit exactly one more action")
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
  assert(w.action_calls == 1, "a failed window should not retry the redraw action")
  assert(#drain_errors() == 0, "a disabled window should not repeat the runtime error")
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

test("review toggles redraw after a successful marker mutation", function()
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
  assert(masked.action_calls == 1, "a successful review removal should redraw once")

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
  assert(review_flag_exists(974), "the flag is written beside the marker instead")

  local flag_path = test_dir .. "/975.review"
  local real_rename = os.rename
  os.rename = function(from, to)
    if to == flag_path then return nil, "permission denied" end
    return real_rename(from, to)
  end
  local failed = window_double({ tabs = { { 975 } }, focused = true, active_pane_id = 975 })
  local ok, toggle_err = pcall(toggle, failed, mux_pane(975))
  os.rename = real_rename
  assert(ok, "review rename failure should be contained: " .. tostring(toggle_err))
  assert(not review_flag_exists(975), "a failed flag write must not create canonical state")
  assert(not marker_exists(975), "and must not fall back to writing the marker file")
  assert(not path_exists(internal.review_tmp_path(test_dir, "975")),
    "failed flag publication must remove its temp")
  assert(failed.action_calls == 0, "a failed flag write must not claim a redraw")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("failed to place review flag", 1, true),
    "the real flag rename failure should be logged, got " .. tostring(errors[1]))
end)

-- ── U3: which id names the marker file ──────────────────────────────────────

test("a published WEZTERM_PANE user var names the marker, not the local pane id", function()
  write_marker(7001, "notify")

  -- A mux-client pane: the GUI numbered it 12, but the process inside it reads
  -- 7001 from $WEZTERM_PANE and writes its marker under that name.
  attention.poll(window_double({
    tabs    = { { { id = 12, domain = "unix", published = 7001 } } },
    focused = false,
  }))

  assert(attention.get_attention(7001) == "notify",
    "the published id should be the cache key")
  assert(attention.get_attention(12) == nil,
    "the GUI's local id must never be read as a marker id")
end)

test("a local pane with no user var falls back to its own pane id", function()
  local pane = mux_pane(7002)
  assert(attention.pane_marker_id(pane) == "7002",
    "a local pane's id is its marker id even unpublished")

  write_marker(7002, "stop")
  poll({ 7002 })
  assert(attention.get_attention(7002) == "stop", "the local fallback should address the marker")
end)

test("a non-decimal user var is rejected in favour of the local fallback", function()
  local escaping = mux_pane(31, { published = "../escape" })
  assert(attention.pane_marker_id(escaping) == "31",
    "a user var that is not a plain decimal must not become a path segment")

  local padded = mux_pane(32, { published = "007" })
  assert(attention.pane_marker_id(padded) == "32",
    "a leading-zero id is not the canonical form WezTerm hands to $WEZTERM_PANE")
end)

test("a mux pane that has published nothing has no marker id and is skipped", function()
  local unpublished = mux_pane(7003, { domain = "unix" })
  assert(attention.pane_marker_id(unpublished) == nil,
    "an unpublished remote pane's local id names some other pane's markers")

  -- Cache marker "7003" from a genuinely local pane in one window.
  write_marker(7003, "notify")
  poll({ 7003 })
  assert(attention.get_attention(7003) == "notify", "precondition: marker 7003 is cached")

  -- Another window holds a remote pane the GUI also numbered 7003. Polling it
  -- must neither read nor remove marker 7003.
  attention.poll(window_double({
    tabs    = { { { id = 7003, domain = "unix" } } },
    focused = false,
  }))
  assert(marker_exists(7003), "an unpublished remote pane must not touch a marker file")
  assert(attention.get_attention(7003) == "notify", "nor the cache entry that marker owns")

  -- And its tab renders nothing, rather than the other pane's notify.
  local rendered = format_tab_title(tab(7003, 7003, true))
  assert(type(rendered) == "string",
    "a tab of unresolvable panes must carry no tint")
  assert(not rendered:find("! ", 1, true),
    "a tab of unresolvable panes must not borrow another pane's indicator")
end)

-- ── U3: a closed pane versus a detached domain ──────────────────────────────

test("a detached domain keeps the markers of panes still running on the server", function()
  write_marker(7101, "notify")
  local window_id = 7100

  attention.poll(window_double({
    tabs      = { { { id = 5, domain = "unix", published = 7101 }, 40 } },
    focused   = false,
    window_id = window_id,
  }))
  assert(attention.get_attention(7101) == "notify", "precondition: the remote marker is cached")

  -- The unix domain is detached: every one of its panes leaves this window in
  -- one tick, while the processes writing their markers keep running.
  attention.poll(window_double({
    tabs      = { { 40 } },
    focused   = false,
    window_id = window_id,
  }))

  assert(marker_exists(7101), "a detached domain's markers must survive the detach")
  assert(attention.get_attention(7101) == "notify", "and stay visible for the reattach")
end)

-- ── U3: marker metadata on the public read ──────────────────────────────────

test("get_attention reports the marker's source and puppet flag", function()
  local file = assert(io.open(test_dir .. "/7201", "w"))
  file:write('{"type":"notify","source":"codex","puppet":true,"publication_id":"pub-7201"}')
  file:close()
  poll({ 7201 })

  local atype, frame, source, puppet = attention.get_attention(7201)
  assert(atype == "notify", "the first two returns keep their meaning")
  assert(frame == nil, "a notify marker carries no frame")
  assert(source == "codex", "source should be the marker's source string, got " .. tostring(source))
  assert(puppet == true, "puppet should be true when the marker says so")

  write_marker(7202, "stop")
  poll({ 7202 })
  local _, _, plain_source, plain_puppet = attention.get_attention(7202)
  assert(plain_source == nil, "a marker with no source reports none")
  assert(plain_puppet == false, "a marker with no puppet flag is not a puppet")

  local _, _, direct_source, direct_puppet = attention.get_attention(7201, { dir = test_dir })
  assert(direct_source == "codex" and direct_puppet == true,
    "a direct disk read should report the same source and puppet flag")
end)

-- ── U3: hosts that repaint their own titles ─────────────────────────────────

test("request_redraw = false performs no action when attention changes", function()
  local quiet = dofile(repo_root .. "/plugin/init.lua")
  quiet.apply_to_config({}, {
    auto_poll      = false,
    dir            = test_dir,
    review_key     = false,
    request_redraw = false,
  })

  write_marker(7301, "notify")
  local w = window_double({
    tabs = { { 7300, 7301 } }, focused = true, active_pane_id = 7300,
  })
  quiet.poll(w, { now_ms = 1000 })

  assert(quiet.get_attention(7301) == "notify", "polling still fills the cache")
  assert(w.action_calls == 0, "no redraw action should be attempted at all")
  assert(#w.actions == 0, "and none recorded")
end)

-- ── U3: acknowledgement replaces its sidecar atomically ─────────────────────

test("acknowledgement renames its sidecar into place without unlinking it first", function()
  write_marker(7401, "notify", "pub-b")
  poll({ 7401 })
  -- A sidecar from an earlier publication is already sitting there.
  write_acknowledgement_file(7401, "publication\npub-a")

  local ack_path = test_dir .. "/7401.ack"
  local tmp_path = internal.acknowledgement_tmp_path(test_dir, "7401")
  assert(tmp_path ~= ack_path .. ".tmp",
    "the in-flight name must be private to this process, not a shared '<id>.ack.tmp'")

  local removed, renamed = {}, {}
  local real_remove, real_rename = os.remove, os.rename
  os.remove = function(path)
    table.insert(removed, path)
    return real_remove(path)
  end
  os.rename = function(from, to)
    table.insert(renamed, from .. " -> " .. to)
    return real_rename(from, to)
  end
  local ok, outcome = pcall(internal.acknowledge_focused_pane, 7401, {
    dir = test_dir,
    now_ms = 1000,
  })
  os.remove, os.rename = real_remove, real_rename

  assert(ok and outcome == "acknowledged", "the acknowledgement should succeed: " .. tostring(outcome))
  for _, path in ipairs(removed) do
    assert(path ~= ack_path,
      "the sidecar must be replaced by rename, never unlinked first")
  end
  assert(renamed[1] == tmp_path .. " -> " .. ack_path,
    "the only rename should place this process's temp over the sidecar, got "
      .. tostring(renamed[1]))
  assert(acknowledgement_exists(7401), "the sidecar should now hold the new publication")
  assert(attention.get_attention(7401) == nil, "and the acknowledged marker should be suppressed")
end)

-- ── U4: the subagent activity sidecar ───────────────────────────────────────

--- One fixed clock for this section. Every poll below is handed it, so a
--- subagent's liveness is decided by the entry's own last_ms and nothing else.
local SIDECAR_NOW = 1000000000000

local function poll_at(pane_ids, spec)
  spec = spec or {}
  local w = window_double({
    tabs           = { pane_ids },
    focused        = spec.focused == true,
    active_pane_id = spec.active_pane_id,
    window_id      = spec.window_id,
  })
  attention.poll(w, { now_ms = SIDECAR_NOW })
  return w
end

test("the sidecar reports live entries only, on the poll's own clock", function()
  write_marker(7501, "stop")
  write_subagents(7501, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 599000 },
    -- Older than the ten-minute window, so it is not counted.
    { id = "agent-c", last_ms = SIDECAR_NOW - 700000 },
  })

  poll_at({ 7500, 7501 })

  local atype, _, _, _, subagents = attention.get_attention(7501)
  assert(atype == "stop", "the marker still reports its own type, got " .. tostring(atype))
  assert(subagents == 2,
    "two of the three entries are live, got " .. tostring(subagents))
end)

test("an unreadable sidecar counts zero and leaves the marker alone", function()
  write_marker(7502, "notify")
  write_raw_subagents(7502, '{"agents":{"agent-a":{"last_ms":')
  poll_at({ 7500, 7502 })
  local truncated_type, _, _, _, truncated_count = attention.get_attention(7502)
  assert(truncated_type == "notify", "a corrupt sidecar must not disturb its marker")
  assert(truncated_count == 0, "a truncated sidecar counts zero, got " .. tostring(truncated_count))

  write_marker(7503, "notify")
  write_raw_subagents(7503, "not json at all")
  poll_at({ 7500, 7503 })
  local garbage_type, _, _, _, garbage_count = attention.get_attention(7503)
  assert(garbage_type == "notify", "nor must content that is not JSON at all")
  assert(garbage_count == 0, "unparseable content counts zero, got " .. tostring(garbage_count))

  write_marker(7504, "notify")
  write_raw_subagents(7504, "")
  poll_at({ 7500, 7504 })
  assert(select(5, attention.get_attention(7504)) == 0, "an empty sidecar counts zero")
end)

test("live subagents keep a pane visible with no marker of its own", function()
  write_subagents(7511, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 2000 },
  })

  poll_at({ 7510, 7511 })

  local atype, frame, source, puppet, subagents = attention.get_attention(7511)
  assert(atype == nil, "a pane with no marker reports no type, got " .. tostring(atype))
  assert(frame == nil and source == nil, "and no frame or source")
  assert(puppet == false, "and is not a puppet, got " .. tostring(puppet))
  assert(subagents == 2, "but its live subagents are reported, got " .. tostring(subagents))
  assert(not marker_exists(7511), "the sidecar must not manufacture a marker file")
end)

test("an acknowledged marker leaves its pane's subagent count behind", function()
  write_marker(7561, "stop", "publication-a")
  write_subagents(7561, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 2000 },
  })

  poll_at({ 7560, 7561 }, { focused = true, active_pane_id = 7561 })
  assert(acknowledgement_exists(7561), "precondition: the viewed stop is acknowledged")

  -- A second, unfocused tick: the acknowledged branch of the poll must keep the
  -- count as surely as the acknowledgement itself did.
  poll_at({ 7560, 7561 })

  local atype, _, _, _, subagents = attention.get_attention(7561)
  assert(atype == nil, "the acknowledged marker is no longer effective")
  assert(subagents == 2, "but its subagents are still working, got " .. tostring(subagents))
  assert(internal.resolve_visible_attention({ "7561" }).indicator == "+2 ",
    "so its tab shows the count alone")
end)

test("the tab indicator carries the subagent count", function()
  write_marker(7521, "stop")
  write_subagents(7522, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 2000 },
  })

  poll_at({ 7521, 7522 })

  local visible = internal.resolve_visible_attention({ "7521", "7522" })
  assert(visible.indicator == "✓+2 ",
    "the count rides in the indicator's own trailing space, got " .. tostring(visible.indicator))
  assert(visible.type == "stop", "the marker type is unchanged")
  assert(visible.color == "#12271c", "and so is its tint")

  local rendered = format_tab_title(tab(7521, 7522, true))
  assert(type(rendered) == "table", "the tab still carries a tint")
  assert(rendered[2].Text:find("✓+2 ", 1, true),
    "the rendered title should carry the count, got " .. tostring(rendered[2].Text))

  local count_only = internal.resolve_visible_attention({ "7522" })
  assert(count_only.indicator == "+2 ",
    "with no marker the count is the whole indicator, got " .. tostring(count_only.indicator))
  assert(count_only.type == nil, "and it names no marker type")
  assert(count_only.color == "#12271c", "a bare count is tinted as stop")
end)

test("a change in the subagent count alone requests a redraw", function()
  write_marker(7531, "stop")
  write_subagents(7531, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 2000 },
  })

  -- 7530 is the active pane and carries no marker, so nothing is acknowledged
  -- and the stop marker on 7531 stays byte-identical throughout.
  local w = window_double({
    tabs = { { 7530, 7531 } }, focused = true, active_pane_id = 7530,
  })
  attention.poll(w, { now_ms = SIDECAR_NOW })
  assert(#w.actions == 1, "the marker appearing is the first visible change, got " .. #w.actions)

  attention.poll(w, { now_ms = SIDECAR_NOW })
  assert(#w.actions == 1, "an unchanged tick must not redraw again, got " .. #w.actions)

  write_subagents(7531, {
    { id = "agent-a", last_ms = SIDECAR_NOW - 1000 },
    { id = "agent-b", last_ms = SIDECAR_NOW - 2000 },
    { id = "agent-c", last_ms = SIDECAR_NOW - 3000 },
  })
  attention.poll(w, { now_ms = SIDECAR_NOW })

  assert(select(5, attention.get_attention(7531)) == 3, "the third subagent should be counted")
  assert(#w.actions == 2,
    "the count changing is itself a visible change, got " .. #w.actions)
end)

test("removal takes the subagent sidecar with the marker", function()
  write_marker(7541, "stop", "publication-a")
  write_subagents(7541, { { id = "agent-a", last_ms = SIDECAR_NOW - 1000 } })
  poll_at({ 7540, 7541 })
  assert(subagents_exists(7541), "precondition: the sidecar exists")

  attention.remove_marker(7541, { dir = test_dir })

  assert(not marker_exists(7541), "the marker should be gone")
  assert(not subagents_exists(7541), "and the subagent sidecar with it")
  assert(attention.get_attention(7541) == nil, "and the cache entry")
end)

test("a pane that vanishes between polls loses its subagent sidecar too", function()
  write_marker(7551, "stop")
  write_subagents(7551, { { id = "agent-a", last_ms = SIDECAR_NOW - 1000 } })
  poll_at({ 7550, 7551 }, { window_id = 7550 })
  assert(subagents_exists(7551), "precondition: the sidecar exists")

  -- 7551 is gone and 7550 remains, so its domain is still represented and the
  -- disappearance reads as a closed pane rather than a detached domain.
  poll_at({ 7550 }, { window_id = 7550 })

  assert(not marker_exists(7551), "a closed pane loses its marker")
  assert(not subagents_exists(7551), "and its subagent sidecar")
  assert(attention.get_attention(7551) == nil, "and its cache entry")
end)

-- ── U5: the manual review flag ──────────────────────────────────────────────

test("the review flag outranks a thinking marker without replacing it", function()
  write_marker(7601, "thinking")
  write_review_flag_file(7601)

  poll_at({ 7600, 7601 })

  local atype, frame, _, _, _, flagged = attention.get_attention(7601)
  assert(atype == "review", "the flag should outrank thinking, got " .. tostring(atype))
  assert(frame == nil, "a review indicator carries no spinner frame, got " .. tostring(frame))
  assert(flagged == true, "the entry should record the flag, got " .. tostring(flagged))

  local marker = assert(io.open(test_dir .. "/7601", "r"))
  local bytes = marker:read("*a")
  marker:close()
  assert(bytes:find('"type":"thinking"', 1, true),
    "the writer's marker must be untouched, got " .. bytes)
  assert(internal.resolve_visible_attention({ "7601" }).indicator == "◆ ",
    "and the tab should show the review glyph")
end)

test("a stop marker outranks the flag until that stop is acknowledged", function()
  write_marker(7611, "stop", "stop-7611")
  write_review_flag_file(7611)

  poll_at({ 7610, 7611 })
  local atype, _, _, _, _, flagged = attention.get_attention(7611)
  assert(atype == "stop", "stop outranks review, got " .. tostring(atype))
  assert(flagged == true, "but the entry still carries the flag, got " .. tostring(flagged))
  assert(internal.resolve_visible_attention({ "7611" }).indicator == "✓ ",
    "and the tab shows the stop glyph")

  poll_at({ 7610, 7611 }, { focused = true, active_pane_id = 7611 })
  assert(acknowledgement_exists(7611), "precondition: the viewed stop is acknowledged")

  local after, _, _, _, _, after_flagged = attention.get_attention(7611)
  assert(after == "review",
    "with the stop acknowledged the flag becomes visible, got " .. tostring(after))
  assert(after_flagged == true, "and is still recorded, got " .. tostring(after_flagged))
  assert(marker_exists(7611), "acknowledgement never removes writer truth")
  assert(review_flag_exists(7611), "and never removes the flag")
end)

test("Alt+B flags a pane a process already owns, and one press clears the tab", function()
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir })
  local toggle = assert(config.keys and config.keys[1] and config.keys[1].action,
    "review key action was not registered")

  write_marker(7621, "thinking")
  write_marker(7622, "notify", "notify-7622")
  local tabs = { { 7621, 7622 } }
  review.poll(window_double({ tabs = tabs, focused = false }), { now_ms = 1000 })

  local w = window_double({ tabs = tabs, focused = true, active_pane_id = 7621 })
  toggle(w, mux_pane(7621))

  assert(review_flag_exists(7621),
    "Alt+B must flag a pane whose marker file a process already owns")
  assert(review.get_attention(7621) == "review",
    "and the flag must take the pane's indicator without another poll")
  assert(w.action_calls == 1, "a successful flag should redraw once, got " .. w.action_calls)

  -- The sibling was flagged by something else; one press must clear the tab.
  write_review_flag_file(7622)
  toggle(w, mux_pane(7621))

  assert(not review_flag_exists(7621), "one press clears the flag from the pressed pane")
  assert(not review_flag_exists(7622), "and from every other pane of its tab")
  assert(marker_exists(7621) and marker_exists(7622),
    "clearing the flag must leave both process markers on disk")
  local sibling = assert(io.open(test_dir .. "/7622", "r"))
  local sibling_bytes = sibling:read("*a")
  sibling:close()
  assert(sibling_bytes:find('"type":"notify"', 1, true),
    "the sibling's notify must survive byte for byte, got " .. sibling_bytes)
  assert(review.get_attention(7621) == "thinking",
    "the pressed pane falls back to its own marker, got "
      .. tostring(review.get_attention(7621)))
  assert(review.get_attention(7622) == "notify", "and so does the sibling")
end)

test("an expired marker takes only its own state, not the flag or the subagents", function()
  local file = assert(io.open(test_dir .. "/7631", "w"))
  file:write('{"type":"thinking","publication_id":"pub-7631",'
    .. '"updated_at_ms":1000000000000,"ttl_ms":1000}')
  file:close()
  write_review_flag_file(7631)
  write_subagents(7631, { { id = "agent-a", last_ms = 1000000000000 } })

  attention.poll(
    window_double({ tabs = { { 7630, 7631 } }, focused = false }),
    { now_ms = 1000000001001 })

  assert(not marker_exists(7631), "the expired marker should be removed")
  assert(review_flag_exists(7631), "the user's flag did not age out with an agent's spinner")
  assert(subagents_exists(7631), "and neither did the subagent sidecar")

  local atype, _, _, _, subagents, flagged = attention.get_attention(7631)
  assert(atype == "review", "the pane still shows the flag, got " .. tostring(atype))
  assert(flagged == true, "which is still recorded, got " .. tostring(flagged))
  assert(subagents == 1, "and its live subagent is still counted, got " .. tostring(subagents))
end)

test("a pane that vanishes between polls loses its review flag too", function()
  write_marker(7641, "stop")
  write_review_flag_file(7641)
  poll_at({ 7640, 7641 }, { window_id = 7640 })
  assert(review_flag_exists(7641), "precondition: the flag exists")

  -- 7641 is gone and 7640 remains, so its domain is still represented and the
  -- disappearance reads as a closed pane rather than a detached domain.
  poll_at({ 7640 }, { window_id = 7640 })

  assert(not marker_exists(7641), "a closed pane loses its marker")
  assert(not review_flag_exists(7641), "and the flag that pointed at it")
  assert(attention.get_attention(7641) == nil, "and its cache entry")
end)

test("a review marker written by an older version is read as the same flag", function()
  write_marker(7651, "review", "legacy-7651")

  poll_at({ 7650, 7651 })

  local atype, _, _, _, _, flagged = attention.get_attention(7651)
  assert(atype == "review", "a legacy review marker still shows review, got " .. tostring(atype))
  assert(flagged == true,
    "and reports itself as flagged, so a clear can find it, got " .. tostring(flagged))
end)

test("an unreadable review flag still counts as flagged", function()
  write_marker(7661, "thinking")
  write_raw_review_flag(7661, "not json at all")
  poll_at({ 7660, 7661 })
  assert(attention.get_attention(7661) == "review",
    "a flag whose body is corrupt must not silently disappear, got "
      .. tostring(attention.get_attention(7661)))

  write_raw_review_flag(7662, "")
  poll_at({ 7660, 7662 })
  local atype, _, _, _, _, flagged = attention.get_attention(7662)
  assert(atype == "review" and flagged == true,
    "an empty flag file is still a flag, got " .. tostring(atype))
end)

test("flagging a pane whose stop is already shown requests a redraw", function()
  write_marker(7671, "stop")

  -- 7670 is the active pane and carries no marker, so nothing is acknowledged
  -- and the stop marker on 7671 stays byte-identical throughout.
  local w = window_double({
    tabs = { { 7670, 7671 } }, focused = true, active_pane_id = 7670,
  })
  attention.poll(w, { now_ms = SIDECAR_NOW })
  assert(#w.actions == 1, "the marker appearing is the first visible change, got " .. #w.actions)
  attention.poll(w, { now_ms = SIDECAR_NOW })
  assert(#w.actions == 1, "an unchanged tick must not redraw again, got " .. #w.actions)

  write_review_flag_file(7671)
  attention.poll(w, { now_ms = SIDECAR_NOW })

  assert(select(6, attention.get_attention(7671)) == true, "the flag should be recorded")
  assert(#w.actions == 2, "the flag arriving is itself a change, got " .. #w.actions)
end)

os.execute("rm -rf " .. shell_quote(test_dir))

io.write(string.format("%d passed, %d failed\n", passed, failed))
if failed > 0 then os.exit(1) end
