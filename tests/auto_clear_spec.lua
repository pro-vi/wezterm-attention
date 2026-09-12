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

--- Small strict JSON decoder for the LuaJIT harness. Production uses
--- wezterm.json_parse; this decoder lets the same nested protocol fixture run
--- here without weakening it into the old regular-expression double.
local function decode_json(content)
  local index = 1

  local function skip_space()
    local _, finish = content:find("^[ \t\r\n]*", index)
    index = (finish or index - 1) + 1
  end

  local parse_value

  local function parse_string()
    assert(content:sub(index, index) == '"', "expected JSON string")
    index = index + 1
    local parts = {}
    while index <= #content do
      local char = content:sub(index, index)
      if char == '"' then
        index = index + 1
        return table.concat(parts)
      end
      if char == "\\" then
        local escaped = content:sub(index + 1, index + 1)
        local simple = {
          ['"'] = '"', ["\\"] = "\\", ["/"] = "/",
          b = "\b", f = "\f", n = "\n", r = "\r", t = "\t",
        }
        if escaped == "u" then
          local hex = content:sub(index + 2, index + 5)
          assert(hex:match("^[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]$"),
            "invalid JSON unicode escape")
          local code = tonumber(hex, 16)
          assert(code < 128, "test JSON decoder only accepts ASCII unicode escapes")
          parts[#parts + 1] = string.char(code)
          index = index + 6
        else
          assert(simple[escaped], "invalid JSON escape")
          parts[#parts + 1] = simple[escaped]
          index = index + 2
        end
      else
        assert(char:byte() >= 32, "control character in JSON string")
        parts[#parts + 1] = char
        index = index + 1
      end
    end
    error("unterminated JSON string", 0)
  end

  local function parse_number()
    local token = content:match("^-?%d+%.?%d*[eE]?[+-]?%d*", index)
    assert(token and token ~= "", "invalid JSON number")
    local value = tonumber(token)
    assert(value, "invalid JSON number")
    index = index + #token
    return value
  end

  local function parse_array()
    index = index + 1
    local result = {}
    skip_space()
    if content:sub(index, index) == "]" then index = index + 1 return result end
    while true do
      result[#result + 1] = parse_value()
      skip_space()
      local char = content:sub(index, index)
      if char == "]" then index = index + 1 return result end
      assert(char == ",", "expected comma in JSON array")
      index = index + 1
      skip_space()
    end
  end

  local function parse_object()
    index = index + 1
    local result = {}
    skip_space()
    if content:sub(index, index) == "}" then index = index + 1 return result end
    while true do
      assert(content:sub(index, index) == '"', "expected JSON object key")
      local key = parse_string()
      skip_space()
      assert(content:sub(index, index) == ":", "expected colon in JSON object")
      index = index + 1
      skip_space()
      result[key] = parse_value()
      skip_space()
      local char = content:sub(index, index)
      if char == "}" then index = index + 1 return result end
      assert(char == ",", "expected comma in JSON object")
      index = index + 1
      skip_space()
    end
  end

  parse_value = function()
    skip_space()
    local char = content:sub(index, index)
    if char == '"' then return parse_string() end
    if char == "{" then return parse_object() end
    if char == "[" then return parse_array() end
    if content:sub(index, index + 3) == "true" then index = index + 4 return true end
    if content:sub(index, index + 4) == "false" then index = index + 5 return false end
    if content:sub(index, index + 3) == "null" then index = index + 4 return nil end
    return parse_number()
  end

  local value = parse_value()
  skip_space()
  assert(index > #content, "trailing content after JSON value")
  return value
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
  json_parse = decode_json,
  glob = function(pattern)
    local directory = pattern:match("^(.*)/%*%.json$")
    if not directory then return {} end
    local pipe = io.popen(
      "find " .. shell_quote(directory) .. " -maxdepth 1 -type f -name '*.json' -print 2>/dev/null")
    if not pipe then return {} end
    local paths = {}
    for path in pipe:lines() do paths[#paths + 1] = path end
    pipe:close()
    table.sort(paths)
    return paths
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

local function read_json_fixture(path)
  local file = assert(io.open(path, "r"))
  local content = assert(file:read("*a"))
  assert(file:close())
  return decode_json(content)
end

local protocol_fixture = read_json_fixture(
  repo_root .. "/tests/fixtures/v2/protocol-cases.json")

local function encode_json_string(value)
  return '"' .. value:gsub('[%z\1-\31\\"]', function(char)
    local escapes = {
      ['"'] = '\\"', ["\\"] = "\\\\", ["\b"] = "\\b", ["\f"] = "\\f",
      ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t",
    }
    return escapes[char] or string.format("\\u%04x", char:byte())
  end) .. '"'
end

local function encode_json(value)
  local kind = type(value)
  if kind == "string" then return encode_json_string(value) end
  if kind == "number" or kind == "boolean" then return tostring(value) end
  if value == nil then return "null" end
  assert(kind == "table", "unsupported JSON fixture value")

  local array = #value > 0
  local parts = {}
  if array then
    for _, item in ipairs(value) do parts[#parts + 1] = encode_json(item) end
    return "[" .. table.concat(parts, ",") .. "]"
  end
  for key, item in pairs(value) do
    parts[#parts + 1] = encode_json_string(key) .. ":" .. encode_json(item)
  end
  table.sort(parts)
  return "{" .. table.concat(parts, ",") .. "}"
end

local function dirname(path)
  return assert(path:match("^(.*)/[^/]+$"), "path has no parent: " .. path)
end

local function write_json_path(path, value)
  assert(os.execute("mkdir -p " .. shell_quote(dirname(path))) == 0)
  local file = assert(io.open(path, "w"))
  assert(file:write(encode_json(value)))
  assert(file:close())
end

local function materialize_state_case(state_case)
  for _, entry in ipairs(state_case.files) do
    write_json_path(test_dir .. "/" .. entry.path,
      protocol_fixture.record_samples[entry.sample])
  end
end

local function materialize_v2_fixture(pane_id, realm_id)
  local original_realm = protocol_fixture.wire_sample.address.realm_id
  local address = decode_json(encode_json(protocol_fixture.wire_sample.address))
  address.pane_id = tostring(pane_id)
  if realm_id then address.realm_id = realm_id end
  for _, entry in ipairs(protocol_fixture.state_case.files) do
    local path = entry.path:gsub(original_realm, address.realm_id, 1)
      :gsub("/panes/42/", "/panes/" .. address.pane_id .. "/", 1)
    local record = decode_json(encode_json(protocol_fixture.record_samples[entry.sample]))
    if record.realm_id then record.realm_id = address.realm_id end
    if record.address then record.address = decode_json(encode_json(address)) end
    write_json_path(test_dir .. "/" .. path, record)
  end
  local wire = decode_json(encode_json(protocol_fixture.wire_sample))
  wire.address = address
  return wire
end

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

local function read_path(path)
  local file = io.open(path, "r")
  if not file then return nil end
  local content = file:read("*a")
  file:close()
  return content
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
  if spec.attention ~= nil then
    user_vars.WEZTERM_ATTENTION = type(spec.attention) == "string"
      and spec.attention or encode_json(spec.attention)
  end
  return {
    pane_id = function() return pane_id end,
    get_domain_name = function() return spec.domain or "local" end,
    get_user_vars = function() return user_vars end,
    get_title = function() return spec.title end,
  }
end

--- Tab entries in a window double are either a bare local pane id or
--- { id, domain, published }.
local function pane_from_entry(entry)
  if type(entry) == "table" then
    return mux_pane(entry.id, {
      domain = entry.domain,
      published = entry.published,
      attention = entry.attention,
      title = entry.title,
    })
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

test("GUI doctor reports unpublished mux panes without filesystem probes", function()
  local pane = mux_pane(7004, { domain = "unix" })
  local diagnostics = attention.doctor(window_double({
    tabs = { { pane } }, focused = false,
  }))
  assert(#diagnostics == 1 and diagnostics[1].code == "identity_unpublished",
    "GUI doctor must report the user-var half the CLI cannot observe")
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
  assert(count_only.color == nil, "a bare count keeps the tab's default colors")
end)

test("count-only uses default colors for v1 and native v2 views", function()
  local sentinel = { colors = { stop = "SENTINEL" } }
  internal.attention_cache["7591"] = { type = nil, subagents = 2, puppet = false }
  local v1_visible = internal.resolve_visible_attention({ "7591" }, sentinel)
  assert(v1_visible.indicator == "+2 " and v1_visible.type == nil and v1_visible.color == nil,
    "the v1 adapter must not turn a count into stop state")

  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local review_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/reviews/" .. samples.review.owner_key .. ".json"
  os.remove(review_path)
  local binding_dir = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id
  local clear = decode_json(encode_json(samples.activity_clear))
  clear.observed_mono_ns = "00000000004000000000"
  write_json_path(binding_dir .. "/activity-clear.json", clear)
  attention.poll(window_double({ tabs = { { {
    id = 7592, domain = "unix", attention = protocol_fixture.wire_sample,
  } } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  local key = internal.address_cache_key(protocol_fixture.wire_sample.address)
  local v2_visible = internal.resolve_visible_attention({ key }, sentinel)
  assert(v2_visible.indicator == "+1 " and v2_visible.type == nil and v2_visible.color == nil,
    "native v2 count-only state must also keep default colors")
  internal.attention_cache["7591"] = nil
end)

test("built-in and manual formatter contexts expose identical named attention", function()
  local built_handlers_before = #(handlers["format-tab-title"] or {})
  local built_ctx
  local built = dofile(repo_root .. "/plugin/init.lua")
  built.apply_to_config({}, {
    auto_poll = false, dir = test_dir, review_key = false, show_provider = true,
    title_formatter = function(_, ctx) built_ctx = ctx; return "built" end,
  })
  built._internal.attention_cache["7601"] = {
    type = "notify", activity_type = "notify", frame = nil, source = "claude",
    provider = "claude", puppet = false, subagents = 2, review = false,
    binding_health = "valid",
  }
  local built_handler = handlers["format-tab-title"][built_handlers_before + 1]
  local built_rendered = built_handler(tab(7601, 7602, false), { "tabs" }, { "panes" }, {}, false, 80)

  local manual_ctx
  local manual = dofile(repo_root .. "/plugin/init.lua")
  manual.apply_to_config({}, {
    renderer = "manual", auto_poll = false, dir = test_dir, review_key = false,
    show_provider = true,
  })
  manual._internal.attention_cache["7601"] = {
    type = "notify", activity_type = "notify", frame = nil, source = "claude",
    provider = "claude", puppet = false, subagents = 2, review = false,
    binding_health = "valid",
  }
  local manual_rendered = manual.wrap_title_formatter(function(_, ctx)
    manual_ctx = ctx
    return "manual"
  end)(tab(7601, 7602, false), { "tabs" }, { "panes" }, {}, false, 80)

  for _, field in ipairs({
    "indicator", "attention_type", "attention_color", "subagents", "source",
    "provider", "puppet", "review", "binding_health",
  }) do
    assert(built_ctx[field] == manual_ctx[field], field .. " differs between formatter contexts")
  end
  assert(built_ctx.attention[1] == built_ctx.attention.indicator
      and built_ctx.attention[2] == built_ctx.attention.type
      and built_ctx.attention[3] == built_ctx.attention.color,
    "the additive named context must preserve the positional attention tuple")
  assert(type(built_rendered) == "table" and type(manual_rendered) == "table",
    "both renderers must carry the marker tint")
  assert(built_rendered[2].Text:find("!+2 built · Claude", 1, true),
    "built-in output must include the marker, count, base, and closed provider suffix")
  assert(manual_rendered[2].Text:find("!+2 manual · Claude", 1, true),
    "manual output must apply the same decoration")
end)

test("puppet filtering preserves review and count as independent inputs", function()
  local filtered = dofile(repo_root .. "/plugin/init.lua")
  filtered.apply_to_config({}, {
    renderer = "manual", auto_poll = false, dir = test_dir, review_key = false,
    show_puppet = false,
  })
  filtered._internal.attention_cache["7611"] = {
    type = "notify", puppet = true, subagents = 2, review = false,
  }
  local count = filtered._internal.resolve_visible_attention({ "7611" })
  assert(count.type == nil and count.indicator == "+2 " and count.color == nil,
    "hidden puppet activity must not hide its independent child count or tint it")
  filtered._internal.attention_cache["7611"].review = true
  local review = filtered._internal.resolve_visible_attention({ "7611" })
  assert(review.type == "review" and review.puppet == false,
    "a user review remains visible when the underlying activity is puppet-owned")
end)

test("all rendered view fields participate in redraw equality", function()
  local baseline = {
    type = "notify", frame = 0, activity_type = "notify", event_id = "a",
    source = "claude", provider = "claude", puppet = false, subagents = 1,
    review = false, binding_phase = "active", pane_presence = "present",
    reader_confidence = "confirmed", binding_health = "valid",
    base_title = "base", settled_title = "settled",
  }
  for _, field in ipairs({
    "type", "frame", "activity_type", "event_id", "source", "provider", "puppet",
    "subagents", "review", "binding_phase", "pane_presence", "reader_confidence",
    "binding_health", "base_title", "settled_title",
  }) do
    local changed = {}
    for key, value in pairs(baseline) do changed[key] = value end
    changed[field] = type(changed[field]) == "boolean" and not changed[field]
      or type(changed[field]) == "number" and changed[field] + 1
      or tostring(changed[field]) .. "-changed"
    assert(not internal.same_cached_attention(baseline, changed),
      field .. " must participate in visible equality")
  end
end)

test("formatter callback failure keeps the last valid base and logs once", function()
  local calls = 0
  local resilient = dofile(repo_root .. "/plugin/init.lua")
  local before = #(handlers["format-tab-title"] or {})
  resilient.apply_to_config({}, {
    auto_poll = false, dir = test_dir, review_key = false,
    title_formatter = function()
      calls = calls + 1
      if calls == 1 then return "stable" end
      error("formatter failed")
    end,
  })
  local handler = handlers["format-tab-title"][before + 1]
  local one = handler(tab(7621, 7622, false))
  local two = handler(tab(7621, 7622, false))
  local three = handler(tab(7621, 7622, false))
  assert(one:find("stable", 1, true) and two:find("stable", 1, true)
      and three:find("stable", 1, true),
    "formatter failures must retain the last valid base title")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("title formatter failed", 1, true),
    "the repeated formatter failure must log once")
end)

test("static pane title settles on the second poll and clears on change", function()
  local first = window_double({
    tabs = { { { id = 17631, title = "stable-title" } } }, focused = false,
  })
  attention.poll(first)
  local before_settle = format_tab_title(tab(17631, 17632, false))
  assert(not before_settle:find("stable-title", 1, true),
    "one raw pane-title sample must not enter the formatter")
  attention.poll(first)
  local settled = format_tab_title(tab(17631, 17632, false))
  assert(settled:find("stable-title", 1, true),
    "two equal samples must establish the settled fallback")

  local changed_window = window_double({
    tabs = { { { id = 17631, title = "changing-title" } } }, focused = false,
  })
  attention.poll(changed_window)
  local changed = format_tab_title(tab(17631, 17632, false))
  assert(not changed:find("stable-title", 1, true)
      and not changed:find("changing-title", 1, true),
    "a title change must clear fallback until the new value settles")
end)

test("server tab name and directory outrank settled pane title", function()
  local window = window_double({
    tabs = { { { id = 17641, title = "settled-pane" } } }, focused = false,
  })
  attention.poll(window)
  attention.poll(window)
  local server_tab = tab(17641, 17642, false)
  server_tab.tab_title = "server-name"
  local server_rendered = format_tab_title(server_tab)
  assert(server_rendered:find("server-name", 1, true)
      and not server_rendered:find("settled-pane", 1, true),
    "server tab name must be the highest base source")

  local directory_tab = tab(17641, 17642, false)
  directory_tab.active_pane.current_working_dir = { file_path = "/tmp/project-dir" }
  local directory_rendered = format_tab_title(directory_tab)
  assert(directory_rendered:find("project-dir", 1, true)
      and not directory_rendered:find("settled-pane", 1, true),
    "directory must outrank settled pane title when no server name exists")
end)

test("title churn logs once per full launch and never writes a title", function()
  local churn = dofile(repo_root .. "/plugin/init.lua")
  churn.apply_to_config({}, {
    renderer = "manual", auto_poll = false, dir = test_dir, review_key = false,
  })
  local sample = churn._internal.sample_settled_title
  sample("v2:realm:incarnation:42", "launch-a", "one", "codex")
  sample("v2:realm:incarnation:42", "launch-a", "two", "codex")
  sample("v2:realm:incarnation:42", "launch-a", "three", "codex")
  sample("v2:realm:incarnation:42", "launch-a", "four", "codex")
  local first_errors = drain_errors()
  assert(#first_errors == 1 and first_errors[1]:find("Codex pane title", 1, true),
    "consecutive changes must emit one provider-specific hint per launch")
  sample("v2:realm:incarnation:42", "launch-b", "one", "codex")
  sample("v2:realm:incarnation:42", "launch-b", "two", "codex")
  sample("v2:realm:incarnation:42", "launch-b", "three", "codex")
  local second_errors = drain_errors()
  assert(#second_errors == 1, "a new launch must receive its own one-time churn hint")

  local window = window_double({
    tabs = {
      { { id = 17651, title = "a" } },
      { { id = 17652, title = "x" } },
    },
    focused = false,
  })
  attention.poll(window)
  assert(#window.title_writes == 0, "polling must never write pane or window titles")
end)

test("invalid pane title cannot destroy a higher base source", function()
  local title_tab = tab(17661, 17662, false)
  title_tab.tab_title = "server-safe"
  internal.sample_settled_title("17661", "v1", "valid", nil)
  internal.sample_settled_title("17661", "v1", "valid", nil)
  internal.sample_settled_title("17661", "v1", "bad\nvalue", nil)
  local rendered = format_tab_title(title_tab)
  assert(rendered:find("server-safe", 1, true),
    "an invalid fallback sample must not affect the server-owned title")
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

-- ── Attention v2 U1: protocol, identity, and wall-age reader ────────────────

test("Lua accepts and rejects every shared protocol fixture row", function()
  assert(internal.sha256("") ==
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "SHA-256 empty-string vector must match")
  assert(internal.sha256("abc") ==
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "SHA-256 abc vector must match")
  local results = internal.parse_fixture_cases(protocol_fixture)
  assert(#results == #protocol_fixture.parse_cases, "every parse row must run")
  for _, result in ipairs(results) do
    assert(result.actual == result.expected,
      result.id .. " expected " .. tostring(result.expected) .. ", got " .. tostring(result.actual))
  end
end)

test("Lua and Python fixture semantics cover exact wall-age boundaries", function()
  local results = internal.fixture_eligibility_cases(protocol_fixture)
  assert(#results == #protocol_fixture.eligibility_cases, "every eligibility row must run")
  for _, result in ipairs(results) do
    assert(result.actual == result.expected,
      result.id .. " eligibility mismatch: " .. tostring(result.actual))
    assert(result.diagnostic == result.expected_diagnostic,
      result.id .. " diagnostic expected " .. tostring(result.expected_diagnostic)
        .. ", got " .. tostring(result.diagnostic))
  end
end)

test("20-digit clocks preserve adjacent nanoseconds without whole-value conversion", function()
  local before = "09999999999999999998"
  local after = "09999999999999999999"
  assert(internal.compare_ns20(before, after) == -1, "adjacent observations must remain ordered")
  local seconds, nanos = internal.unix_ns_parts(after)
  assert(seconds == 9999999999 and nanos == 999999999,
    "the safe seconds and nanos parts must be parsed separately")

  local exact, exact_error = internal.age_exceeds_ms(
    "00000000610000000000", "00000000010000000000", 600000)
  local late, late_error = internal.age_exceeds_ms(
    "00000000610000000001", "00000000010000000000", 600000)
  assert(exact == false and exact_error == nil, "exact TTL equality remains eligible")
  assert(late == true and late_error == nil, "one nanosecond later is ineligible")
end)

test("full addresses isolate same-basename mux realms", function()
  local first = protocol_fixture.wire_sample.address
  local second = {
    realm_id = "9999999999999999999999999999999999999999999999999999999999999999",
    incarnation_id = first.incarnation_id,
    pane_id = first.pane_id,
  }
  assert(internal.address_cache_key(first) ~= internal.address_cache_key(second),
    "realm digest must participate in the cache key even when display basenames match")
end)

test("a complete v2 state tree reaches both pure renderers", function()
  materialize_state_case(protocol_fixture.state_case)
  local scheduled = {}
  local pane_spec = {
    id = 4242,
    domain = "unix",
    published = 42,
    attention = protocol_fixture.wire_sample,
  }
  attention.poll(window_double({ tabs = { { pane_spec } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function(delay, callback)
      scheduled[#scheduled + 1] = { delay = delay, callback = callback }
    end,
  })

  local key = internal.address_cache_key(protocol_fixture.wire_sample.address)
  local view = assert(internal.attention_cache[key], "full-address view must be cached")
  local expected = protocol_fixture.state_case.expected_view
  assert(view.activity_type == expected.activity_type, "activity type must reach AttentionView")
  assert(view.binding_phase == expected.binding_phase, "matching end must set ended phase")
  assert(view.pane_presence == expected.pane_presence, "the observed pane is present")
  assert(view.reader_confidence == expected.reader_confidence, "matching live claim confirms view")
  assert(view.binding_health == expected.binding_health, "fixture should remain valid")
  assert(view.provider == expected.provider,
    "provider belongs to the binding-backed view")
  assert(view.subagents == expected.subagents, "one exact active child must count")
  assert(view.review == expected.review, "review overlay composes with activity and end")

  local default_rendered = format_tab_title(tab(4242, 4243, false))
  local manual_attention
  local manual_rendered = attention.wrap_title_formatter(function(_, ctx)
    manual_attention = ctx.attention
    return "manual"
  end)(tab(4242, 4243, false))
  local expected_render = protocol_fixture.state_case.expected_render
  assert(type(default_rendered) == "table" and type(manual_rendered) == "table",
    "both renderers should return a tinted tab")
  assert(default_rendered[1].Background.Color == expected_render.color,
    "built-in renderer must use the winning marker tint")
  assert(default_rendered[2].Text:find(expected_render.indicator, 1, true),
    "built-in renderer must append the exact child count")
  assert(manual_attention[1] == expected_render.indicator
      and manual_attention[2] == expected_render.type
      and manual_attention[3] == expected_render.color,
    "manual renderer context must match the built-in projection")
  assert(#scheduled == 1 and scheduled[1].delay > 0,
    "an eligible TTL record must schedule one future reread")
end)

test("activity TTL uses written Unix time while event order stays monotonic", function()
  local samples = protocol_fixture.record_samples
  local activity = decode_json(encode_json(samples.activity))
  activity.ttl_ms = 600000
  local activity_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/activity.json"
  write_json_path(activity_path, activity)

  local read = internal.resolve_pane_read(mux_pane(4251, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local exact = internal.read_attention_view(read, "00000000610000000000", {
    dir = test_dir, glob = wezterm.glob,
  })
  local late = internal.read_attention_view(read, "00000000610000000001", {
    dir = test_dir, glob = wezterm.glob,
  })
  assert(exact.activity_type == "notify", "activity remains eligible at exact TTL equality")
  assert(late.activity_type == nil, "activity expires one nanosecond after its wall boundary")

  local older_order = "00000000000000000001"
  local newer_order = "00000000000000000002"
  assert(internal.compare_ns20(older_order, newer_order) == -1,
    "newer observed_mono_ns wins even if its written_at_unix_ns is earlier")
  write_json_path(activity_path, samples.activity)
end)

test("an activity clear watermark hides older activity and permits newer activity", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local binding_dir = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id
  local clear = decode_json(encode_json(samples.activity_clear))
  clear.observed_mono_ns = "00000000004000000000"
  write_json_path(binding_dir .. "/activity-clear.json", clear)

  local read = internal.resolve_pane_read(mux_pane(4280, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local hidden = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(hidden.activity_type == nil, "the clear watermark must hide an older activity")

  local newer = decode_json(encode_json(samples.activity))
  newer.event_id = "00000000-0000-4000-8000-000000000013"
  newer.observed_mono_ns = "00000000005000000000"
  write_json_path(binding_dir .. "/activity.json", newer)
  local visible = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(visible.activity_type == "notify", "strictly newer activity must reappear after clear")
end)

test("an unreadable activity clear watermark fails closed", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local clear_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/activity-clear.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == clear_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local read = internal.resolve_pane_read(mux_pane(4281, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local ok, view = pcall(internal.read_attention_view, read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  io.open = real_open
  assert(ok, "activity-clear read failure must stay inside the poll boundary")
  assert(view.activity_type == nil and view.reader_confidence == "unconfirmed",
    "unavailable activity-clear evidence must omit activity")
end)

test("a newer same-binding confirmation reopens an older end snapshot", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local binding_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/binding.json"
  local binding = decode_json(encode_json(samples.binding))
  binding.observed_mono_ns = "00000000005000000000"
  binding.event_id = "00000000-0000-4000-8000-000000000014"
  write_json_path(binding_path, binding)
  local read = internal.resolve_pane_read(mux_pane(4282, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local view = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(view.binding_phase == "active", "an end older than the latest binding confirmation must not remain terminal")
end)

test("raw subagent and review ids never become fixture paths", function()
  local state = protocol_fixture.state_case
  local samples = protocol_fixture.record_samples
  for _, entry in ipairs(state.files) do
    assert(not entry.path:find(samples.subagent_presence.agent_id, 1, true),
      "raw agent_id leaked into a state path")
    assert(not entry.path:find(samples.review.owner_id, 1, true),
      "raw review owner leaked into a state path")
  end
end)

test("one v2 poll samples UTC once and formatting samples no clock", function()
  local samples = 0
  local scheduled = 0
  local pane_spec = {
    id = 4244,
    domain = "unix",
    published = 42,
    attention = protocol_fixture.wire_sample,
  }
  attention.poll(window_double({ tabs = { { pane_spec } }, focused = false }), {
    utc_now = function()
      samples = samples + 1
      return protocol_fixture.state_case.now_unix_ns
    end,
    call_after = function() scheduled = scheduled + 1 end,
  })
  assert(samples == 1, "one poll must use one UTC sample for every record")
  format_tab_title(tab(4244, 4245, false))
  attention.wrap_title_formatter(function() return "manual" end)(tab(4244, 4245, false))
  assert(samples == 1, "formatters must remain clock-free")
  assert(scheduled <= 1, "one poll must not stack TTL timers")
end)

test("call_after only wakes a fresh TTL read", function()
  local callback
  local clock_value = "00000000610000000000"
  local pane_spec = {
    id = 4250,
    domain = "unix",
    published = 42,
    attention = protocol_fixture.wire_sample,
  }
  local window = window_double({ tabs = { { pane_spec } }, focused = false })
  attention.poll(window, {
    now_unix_ns = clock_value,
    call_after = function(_, scheduled_callback) callback = scheduled_callback end,
  })
  assert(type(callback) == "function", "the exact-boundary child must schedule a reread")
  assert(select(5, attention.get_attention(42)) == 1,
    "scheduling alone must not expire the child")

  clock_value = "00000000610000000001"
  wezterm.time = {
    now = function()
      local seconds = clock_value:sub(1, 11):gsub("^0+", "")
      if seconds == "" then seconds = "0" end
      local nanos = clock_value:sub(12, 20)
      return {
        format_utc = function(_, format)
          assert(format == "%s%9f", "the plugin requested an unexpected UTC format")
          return seconds .. nanos
        end,
      }
    end,
    call_after = function() error("expired state must not schedule another TTL wakeup") end,
  }
  callback()
  wezterm.time = nil
  assert(select(5, attention.get_attention(42)) == 0,
    "the fresh read one nanosecond later must derive expiry")
end)

test("unavailable UTC omits TTL children but preserves non-TTL activity", function()
  local pane_spec = {
    id = 4246,
    domain = "unix",
    published = 42,
    attention = protocol_fixture.wire_sample,
  }
  attention.poll(window_double({ tabs = { { pane_spec } }, focused = false }), {
    utc_now = function() return nil, "probe_unavailable" end,
  })
  local atype, _, _, _, subagents, review = attention.get_attention(42)
  assert(atype == "notify", "non-TTL activity must survive an unavailable TTL clock")
  assert(subagents == 0, "TTL-bearing child state must fail closed")
  assert(review == true, "the exact review overlay must remain independent of the clock")
  local errors = drain_errors()
  assert(#errors >= 1 and errors[1]:find("probe_unavailable", 1, true),
    "the unavailable clock must be diagnosed")
end)

test("invalid and future v2 identity never downgrade to a plausible v1 marker", function()
  write_marker(7781, "notify")
  local invalid_pane = {
    id = 7780, domain = "unix", published = 7781, attention = "not-json",
  }
  attention.poll(window_double({ tabs = { { invalid_pane } }, focused = false }))
  assert(attention.get_attention(7781) == nil,
    "malformed v2 identity must not read the valid-looking v1 file")
  local rendered = format_tab_title(tab(7780, 7782, false))
  assert(type(rendered) == "string" and not rendered:find("! ", 1, true),
    "malformed v2 identity must render no borrowed v1 marker")
  local malformed_errors = drain_errors()
  assert(#malformed_errors == 1 and malformed_errors[1]:find("record_invalid", 1, true),
    "malformed identity must be diagnosed distinctly")

  local future = decode_json(encode_json(protocol_fixture.wire_sample))
  future.wire = 3
  local future_pane = { id = 7783, domain = "unix", published = 7781, attention = future }
  attention.poll(window_double({ tabs = { { future_pane } }, focused = false }))
  assert(attention.get_attention(7781) == nil, "future identity must not downgrade to v1")
  local future_errors = drain_errors()
  assert(#future_errors == 1 and future_errors[1]:find("future_schema", 1, true),
    "future identity must be diagnosed distinctly")
end)

test("a core interior-address mismatch makes the v2 view invalid", function()
  local mismatched_wire = decode_json(encode_json(protocol_fixture.wire_sample))
  mismatched_wire.address.pane_id = "43"
  local bad_claim_path = test_dir .. "/v2/realms/" .. mismatched_wire.address.realm_id
    .. "/incarnations/" .. mismatched_wire.address.incarnation_id
    .. "/panes/43/claim.json"
  write_json_path(bad_claim_path, protocol_fixture.record_samples.claim)

  local pane_spec = {
    id = 4247, domain = "unix", published = 43, attention = mismatched_wire,
  }
  attention.poll(window_double({ tabs = { { pane_spec } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  local key = internal.address_cache_key(mismatched_wire.address)
  local view = assert(internal.attention_cache[key], "invalid view remains diagnosable")
  assert(view.binding_health == "invalid" and view.activity_type == nil,
    "mismatched interior identity must expose no activity")
  local errors = drain_errors()
  assert(#errors >= 1 and errors[1]:find("record_invalid", 1, true),
    "interior mismatch must be diagnosed")
end)

test("a future core record blocks v1 downgrade and stays visible as future schema", function()
  local future_wire = decode_json(encode_json(protocol_fixture.wire_sample))
  future_wire.address.pane_id = "44"
  local future_claim = decode_json(encode_json(protocol_fixture.record_samples.claim))
  future_claim.schema = 3
  future_claim.address.pane_id = "44"
  local claim_path = test_dir .. "/v2/realms/" .. future_wire.address.realm_id
    .. "/incarnations/" .. future_wire.address.incarnation_id
    .. "/panes/44/claim.json"
  write_json_path(claim_path, future_claim)
  write_marker(7791, "notify")

  local pane_spec = {
    id = 4253, domain = "unix", published = 7791, attention = future_wire,
  }
  attention.poll(window_double({ tabs = { { pane_spec } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  local key = internal.address_cache_key(future_wire.address)
  local view = assert(internal.attention_cache[key], "future view must remain diagnosable")
  assert(view.binding_health == "future_schema" and view.activity_type == nil,
    "future claim must expose no v2 activity")
  assert(attention.get_attention(7791) == nil,
    "the plausible v1 marker must remain ineligible")
  local errors = drain_errors()
  assert(#errors >= 1 and errors[1]:find("future_schema", 1, true),
    "future schema must be reported")
end)

test("an unreadable core v2 record is unavailable rather than absent", function()
  local read = assert(internal.resolve_pane_read(mux_pane(4248, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  })))
  local claim_path = test_dir .. "/v2/realms/" .. read.address.realm_id
    .. "/incarnations/" .. read.address.incarnation_id
    .. "/panes/" .. read.address.pane_id .. "/claim.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == claim_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local ok, view = pcall(internal.read_attention_view, read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  io.open = real_open
  assert(ok, "an unreadable record must not escape the poll boundary")
  assert(view.binding_health == "invalid" and view.reader_confidence == "unconfirmed"
      and view.pane_presence == "unavailable",
    "unavailable claim evidence must not become confirmed absence")
  local found = false
  for _, item in ipairs(view.diagnostics) do
    if item.code == "probe_unavailable" then found = true end
  end
  assert(found, "permission failure must remain a probe_unavailable diagnostic")
end)

test("a poll read failure preserves the last v2 view as unavailable", function()
  local pane_spec = {
    id = 4252, domain = "unix", published = 42, attention = protocol_fixture.wire_sample,
  }
  local window = window_double({ tabs = { { pane_spec } }, focused = false })
  attention.poll(window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  local key = internal.address_cache_key(protocol_fixture.wire_sample.address)
  assert(internal.attention_cache[key].activity_type == "notify", "precondition: valid view")

  local samples = protocol_fixture.record_samples
  local claim_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/claim.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == claim_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local ok, poll_error = pcall(attention.poll, window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  io.open = real_open
  assert(ok, "poll must contain the read failure: " .. tostring(poll_error))
  local preserved = internal.attention_cache[key]
  assert(preserved.activity_type == "notify", "last valid activity must remain cached")
  assert(preserved.pane_presence == "unavailable"
      and preserved.reader_confidence == "unconfirmed",
    "the preserved view must expose unavailable, unconfirmed evidence")
  local errors = drain_errors()
  assert(#errors >= 1 and errors[1]:find("probe_unavailable", 1, true),
    "the poll must report the unavailable read")
end)

test("invalid child wall ages do not poison a valid sibling", function()
  local samples = protocol_fixture.record_samples
  local agents_dir = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/agents"

  local future = decode_json(encode_json(samples.subagent_presence))
  future.agent_id = "child-future"
  future.agent_key = "731bc325223228990d07e8b7adcbb75f761c5059011cad92b0aee8d3d3125bdc"
  future.event_id = "00000000-0000-4000-8000-000000000012"
  future.observed_mono_ns = "00000000310000000000"
  future.written_at_unix_ns = "00000000620000000000"
  write_json_path(agents_dir .. "/" .. future.agent_key .. ".json", future)

  local malformed = decode_json(encode_json(samples.subagent_presence))
  malformed.agent_id = "child-malformed"
  malformed.agent_key = "1c68b06d2f8d5136a4e534149c07ef8f118b51f7e39ecbf5a041acfb38330abc"
  malformed.event_id = "00000000-0000-4000-8000-000000000013"
  malformed.observed_mono_ns = "00000000320000000000"
  malformed.written_at_unix_ns = "bad"
  write_json_path(agents_dir .. "/" .. malformed.agent_key .. ".json", malformed)

  local read = internal.resolve_pane_read(mux_pane(4249, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local view = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(view.subagents == 1, "the original valid child must remain counted")
  local codes = {}
  for _, item in ipairs(view.diagnostics) do codes[item.code] = true end
  assert(codes.clock_skew == true, "the future child must report clock_skew")
  assert(codes.record_invalid == true, "the malformed child must report record_invalid")
end)

-- ── U1 review regressions ───────────────────────────────────────────────────

test("cache recovery never crosses a launch identity boundary", function()
  materialize_state_case(protocol_fixture.state_case)
  local original_wire = protocol_fixture.wire_sample
  local original_window = window_double({ tabs = { { {
    id = 4260, domain = "unix", attention = original_wire,
  } } }, focused = false })
  attention.poll(original_window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })

  local next_wire = decode_json(encode_json(original_wire))
  next_wire.launch_id = "00000000-0000-4000-8000-000000000020"
  local claim_path = test_dir .. "/v2/realms/" .. next_wire.address.realm_id
    .. "/incarnations/" .. next_wire.address.incarnation_id
    .. "/panes/42/claim.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == claim_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local next_window = window_double({ tabs = { { {
    id = 4260, domain = "unix", attention = next_wire,
  } } }, focused = false })
  local ok, poll_error = pcall(attention.poll, next_window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  io.open = real_open
  assert(ok, "launch-change read failure escaped: " .. tostring(poll_error))
  local key = internal.address_cache_key(next_wire.address)
  local view = assert(internal.attention_cache[key], "new launch must retain a diagnostic view")
  assert(view.launch_id == next_wire.launch_id, "recovery must not restore the prior launch identity")
  assert(view.activity_type == nil and view.subagents == 0,
    "recovery must not restore the prior launch's !+N")
  drain_errors()
end)

test("cache recovery recalculates TTL and never rearms an expired deadline", function()
  materialize_state_case(protocol_fixture.state_case)
  local pane_spec = {
    id = 4261, domain = "unix", attention = protocol_fixture.wire_sample,
  }
  local window = window_double({ tabs = { { pane_spec } }, focused = false })
  attention.poll(window, {
    now_unix_ns = "00000000610000000000",
    call_after = function() end,
  })
  assert(select(5, attention.get_attention(42)) == 1, "precondition: exact-boundary child counts")

  local samples = protocol_fixture.record_samples
  local claim_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/claim.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == claim_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local scheduled = {}
  local ok, poll_error = pcall(attention.poll, window, {
    now_unix_ns = "00000000610000000001",
    call_after = function(delay, callback)
      scheduled[#scheduled + 1] = { delay = delay, callback = callback }
    end,
  })
  io.open = real_open
  assert(ok, "expired-boundary read failure escaped: " .. tostring(poll_error))
  assert(select(5, attention.get_attention(42)) == 0,
    "recovery must derive that the cached child is now expired")
  assert(#scheduled == 0, "an expired deadline must not schedule delay=0 callbacks")
  drain_errors()
end)

test("matching forged child filenames cannot duplicate one raw agent id", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local forged = decode_json(encode_json(samples.subagent_presence))
  forged.agent_key = "9999999999999999999999999999999999999999999999999999999999999999"
  local agents_dir = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/agents"
  write_json_path(agents_dir .. "/" .. forged.agent_key .. ".json", forged)

  local read = internal.resolve_pane_read(mux_pane(4262, {
    domain = "unix", attention = protocol_fixture.wire_sample,
  }))
  local view = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(view.subagents == 1, "one raw agent id must contribute at most one child")
  local invalid_hash = false
  for _, item in ipairs(view.diagnostics) do
    if item.code == "record_invalid" and item.message:find("agent_key", 1, true) then
      invalid_hash = true
    end
  end
  assert(invalid_hash, "the forged agent_key relationship must be diagnosed")
end)

test("v1 identity clears a stale scalar-to-v2 cache projection", function()
  write_marker(42, "thinking")
  attention.poll(window_double({ tabs = { { 42 } }, focused = false }))
  assert(attention.get_attention(42) == "thinking",
    "the public scalar query must return the pane's current v1 cache")
end)

test("an unbound launch reads and applies its exact acknowledgement", function()
  local samples = protocol_fixture.record_samples
  local wire = decode_json(encode_json(protocol_fixture.wire_sample))
  wire.address.pane_id = "45"
  wire.launch_id = "00000000-0000-4000-8000-000000000021"
  local claim = decode_json(encode_json(samples.claim))
  claim.address.pane_id = wire.address.pane_id
  claim.launch_id = wire.launch_id
  local activity = decode_json(encode_json(samples.activity))
  activity.address.pane_id = wire.address.pane_id
  activity.launch_id = wire.launch_id
  activity.target = { kind = "launch" }
  activity.event_id = "00000000-0000-4000-8000-000000000022"
  local acknowledgement = decode_json(encode_json(samples.acknowledgement))
  acknowledgement.address.pane_id = wire.address.pane_id
  acknowledgement.launch_id = wire.launch_id
  acknowledgement.target = { kind = "launch" }
  acknowledgement.activity_event_id = activity.event_id

  local pane_root = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id
    .. "/panes/" .. wire.address.pane_id
  local launch_root = pane_root .. "/launches/" .. wire.launch_id
  write_json_path(pane_root .. "/claim.json", claim)
  write_json_path(launch_root .. "/activity.json", activity)
  write_json_path(launch_root .. "/ack.json", acknowledgement)

  local read = internal.resolve_pane_read(mux_pane(4263, {
    domain = "unix", attention = wire,
  }))
  local view = internal.read_attention_view(read,
    protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob })
  assert(view.activity_type == nil, "matching launch acknowledgement must suppress activity")
end)

local function canonical_fixture_glob(pattern)
  local paths = wezterm.glob(pattern)
  local selected = {}
  for _, path in ipairs(paths) do
    if path:find(protocol_fixture.record_samples.review.owner_key, 1, true)
        or path:find(protocol_fixture.record_samples.subagent_presence.agent_key, 1, true) then
      selected[#selected + 1] = path
    end
  end
  return selected
end

local function seed_record_recovery(window_id)
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local pane_root = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id .. "/panes/42"
  local launch_root = pane_root .. "/launches/" .. samples.claim.launch_id
  local records_root = launch_root .. "/bindings/" .. samples.binding.binding_id
  local window = window_double({ window_id = window_id, tabs = { { {
    id = window_id, domain = "unix", attention = protocol_fixture.wire_sample,
  } } }, focused = false })
  attention.poll(window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    glob = canonical_fixture_glob,
    call_after = function() end,
  })
  local key = internal.address_cache_key(samples.claim.address)
  assert(internal.attention_cache[key].subagents == 1, "seed child must be active")
  assert(internal.attention_cache[key].activity_type == "notify", "seed activity must be visible")
  return window, key, pane_root, launch_root, records_root
end

test("a fresh binding pointer prevents recovery of an older binding", function()
  local window, key, _, launch_root = seed_record_recovery(9011)
  local pointer = decode_json(encode_json(protocol_fixture.record_samples.current_binding))
  local old_binding_id = pointer.binding_id
  pointer.binding_id = string.rep("a", 64)
  write_json_path(launch_root .. "/current-binding.json", pointer)
  local failed_path = launch_root .. "/bindings/" .. pointer.binding_id .. "/binding.json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == failed_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local ok, poll_error = pcall(attention.poll, window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    glob = canonical_fixture_glob,
    call_after = function() end,
  })
  io.open = real_open
  assert(ok, "new-binding failure escaped: " .. tostring(poll_error))
  local view = internal.attention_cache[key]
  assert(view.binding_id ~= old_binding_id and view.activity_type == nil and view.subagents == 0,
    "new pointer identity must not recover old binding activity or children")
  drain_errors()
end)

test("fresh acknowledgement and stopped child survive an unrelated review read failure", function()
  local window, key, pane_root, _, records_root = seed_record_recovery(9012)
  local samples = protocol_fixture.record_samples
  local stopped = decode_json(encode_json(samples.subagent_presence))
  stopped.status = "stopped"
  stopped.event_id = "00000000-0000-4000-8000-000000000030"
  stopped.observed_mono_ns = "00000000301000000000"
  stopped.written_at_unix_ns = protocol_fixture.state_case.now_unix_ns
  write_json_path(records_root .. "/agents/" .. stopped.agent_key .. ".json", stopped)
  local acknowledgement = decode_json(encode_json(samples.acknowledgement))
  acknowledgement.activity_event_id = samples.activity.event_id
  write_json_path(records_root .. "/ack.json", acknowledgement)

  local failed_path = pane_root .. "/reviews/" .. samples.review.owner_key .. ".json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == failed_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local ok, poll_error = pcall(attention.poll, window, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    glob = canonical_fixture_glob,
    call_after = function() end,
  })
  io.open = real_open
  assert(ok, "review read failure escaped: " .. tostring(poll_error))
  local view = internal.attention_cache[key]
  assert(view.activity_type == nil and view.subagents == 0,
    "unrelated review failure must not revive acknowledged activity or stopped child")
  drain_errors()
end)

test("record recovery rejects cached TTL state when UTC precedes its write", function()
  local window, key, pane_root = seed_record_recovery(9013)
  local failed_path = pane_root .. "/reviews/"
    .. protocol_fixture.record_samples.review.owner_key .. ".json"
  local real_open = io.open
  io.open = function(path, mode)
    if path == failed_path then return nil, "permission denied" end
    return real_open(path, mode)
  end
  local ok, poll_error = pcall(attention.poll, window, {
    now_unix_ns = "00000000009000000000",
    glob = canonical_fixture_glob,
    call_after = function() end,
  })
  io.open = real_open
  assert(ok, "clock-skew recovery escaped: " .. tostring(poll_error))
  local view = internal.attention_cache[key]
  assert(view.subagents == 0, "cached child must fail closed when now is before written time")
  local saw_clock_skew = false
  for _, item in ipairs(view.diagnostics or {}) do
    if item.code == "clock_skew" then saw_clock_skew = true end
  end
  assert(saw_clock_skew, "clock-skew recovery must keep the fresh diagnostic")
  drain_errors()
end)

test("focused v2 acknowledgement targets only the active pane's exact event", function()
  local wire = materialize_v2_fixture(51)
  local samples = protocol_fixture.record_samples
  local binding_root_path = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/51/launches/"
    .. wire.launch_id .. "/bindings/" .. samples.binding.binding_id
  os.remove(binding_root_path .. "/ack.json")
  local sibling_wire = materialize_v2_fixture(52)
  local sibling_root = test_dir .. "/v2/realms/" .. sibling_wire.address.realm_id
    .. "/incarnations/" .. sibling_wire.address.incarnation_id .. "/panes/52/launches/"
    .. sibling_wire.launch_id .. "/bindings/" .. samples.binding.binding_id
  os.remove(sibling_root .. "/ack.json")
  local active = { id = 9051, domain = "unix", attention = wire }
  local sibling = { id = 9052, domain = "unix", attention = sibling_wire }
  attention.poll(window_double({
    tabs = { { active, sibling } }, focused = true, active_pane_id = active,
  }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })

  local ack_raw = assert(read_path(binding_root_path .. "/ack.json"))
  local ack = assert(internal.parse_v2_record_json(ack_raw, "acknowledgement"))
  assert(ack.activity_event_id == samples.activity.event_id,
    "the acknowledgement must name the event the active pane displayed")
  assert(ack.target.kind == "binding" and ack.target.binding_id == samples.binding.binding_id,
    "the acknowledgement must name the selected binding")
  assert(not path_exists(sibling_root .. "/ack.json"),
    "focusing one v2 pane must not acknowledge its sibling")
  local key = internal.address_cache_key(wire.address)
  assert(internal.attention_cache[key].activity_type == nil,
    "the exact acknowledged activity must be suppressed on the same poll")
end)

test("Alt+B uses full v2 addresses and clear-all preserves activity", function()
  local realm_b = string.rep("9", 64)
  local wire_a = materialize_v2_fixture(61)
  local wire_b = materialize_v2_fixture(61, realm_b)
  local samples = protocol_fixture.record_samples
  local function pane_root_for(wire)
    return test_dir .. "/v2/realms/" .. wire.address.realm_id
      .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/61"
  end
  local pane_a_root = pane_root_for(wire_a)
  local pane_b_root = pane_root_for(wire_b)
  os.remove(pane_a_root .. "/reviews/" .. samples.review.owner_key .. ".json")
  os.remove(pane_b_root .. "/reviews/" .. samples.review.owner_key .. ".json")

  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir })
  local toggle = assert(config.keys and config.keys[#config.keys].action,
    "Alt+B action was not registered")
  local pane_a = pane_from_entry({ id = 9061, domain = "unix-a", attention = wire_a })
  local pane_b = pane_from_entry({ id = 9062, domain = "unix-b", attention = wire_b })
  local window = window_double({ tabs = { {
    { id = 9061, domain = "unix-a", attention = wire_a },
    { id = 9062, domain = "unix-b", attention = wire_b },
  } }, focused = true, active_pane_id = { id = 9061, domain = "unix-a", attention = wire_a } })
  local user_key = internal.sha256("user")
  local user_path = pane_a_root .. "/reviews/" .. user_key .. ".json"

  toggle(window, pane_a)
  local user_raw = assert(read_path(user_path))
  local user_record = assert(internal.parse_v2_record_json(user_raw, "review"))
  assert(user_record.owner_id == "user" and user_record.address.realm_id == wire_a.address.realm_id,
    "Alt+B must write the exact user owner record under the active full address")
  assert(not path_exists(pane_b_root .. "/reviews/" .. user_key .. ".json"),
    "a same-number pane in another realm must not be flagged")

  local other = decode_json(encode_json(samples.review))
  other.address = decode_json(encode_json(wire_b.address))
  other.owner_id = "pi-bus"
  other.owner_key = internal.sha256(other.owner_id)
  other.event_id = "00000000-0000-4000-8000-000000000041"
  write_json_path(pane_b_root .. "/reviews/" .. other.owner_key .. ".json", other)
  local activity_path = pane_b_root .. "/launches/" .. wire_b.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/activity.json"
  local activity_before = assert(read_path(activity_path))

  toggle(window, pane_a)
  assert(not path_exists(user_path), "clear-all must remove the active pane's user claim")
  assert(not path_exists(pane_b_root .. "/reviews/" .. other.owner_key .. ".json"),
    "clear-all must remove a sibling's valid claim through its full address")
  assert(read_path(activity_path) == activity_before,
    "clearing a masked review claim must preserve unrelated activity bytes")
end)

test("v2 user actions never replace future acknowledgement or review records", function()
  local wire = materialize_v2_fixture(71)
  local samples = protocol_fixture.record_samples
  local pane_root = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/71"
  local binding_root_path = pane_root .. "/launches/" .. wire.launch_id
    .. "/bindings/" .. samples.binding.binding_id
  local ack_path = binding_root_path .. "/ack.json"
  local future_ack = decode_json(encode_json(samples.acknowledgement))
  future_ack.address = decode_json(encode_json(wire.address))
  future_ack.schema = 999
  write_json_path(ack_path, future_ack)
  local ack_before = assert(read_path(ack_path))
  local pane_spec = { id = 9071, domain = "unix", attention = wire }
  attention.poll(window_double({
    tabs = { { pane_spec } }, focused = true, active_pane_id = pane_spec,
  }), { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  assert(read_path(ack_path) == ack_before,
    "focus must preserve an existing future acknowledgement byte for byte")
  local key = internal.address_cache_key(wire.address)
  assert(internal.attention_cache[key].activity_type == "notify",
    "a refused acknowledgement must leave the notification visible")

  local user_key = internal.sha256("user")
  local review_path = pane_root .. "/reviews/" .. user_key .. ".json"
  local future_review = decode_json(encode_json(samples.review))
  future_review.address = decode_json(encode_json(wire.address))
  future_review.schema = 999
  write_json_path(review_path, future_review)
  local review_before = assert(read_path(review_path))
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir })
  local toggle = assert(config.keys and config.keys[#config.keys].action)
  toggle(window_double({ tabs = { { pane_spec } }, focused = true,
    active_pane_id = pane_spec }), pane_from_entry(pane_spec))
  assert(read_path(review_path) == review_before,
    "Alt+B must preserve an existing future user review byte for byte")
  drain_errors()
end)

test("unpublished mux pane schedules one realm publish from the resolved plugin root", function()
  local spawned = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  local config = {
    unix_domains = { { name = "u2-test", socket_path = "/tmp/attention-u2-test.sock" } },
  }
  reloaded.apply_to_config(config, {
    auto_poll = false,
    dir = test_dir,
    review_key = false,
  })
  local pane = { id = 9901, domain = "u2-test" }
  local window = window_double({ tabs = { { pane } }, focused = false })
  reloaded.poll(window, { call_after = function() end })
  reloaded.poll(window, { call_after = function() end })
  wezterm.background_child_process = original_background

  local expected_root = assert(internal.protocol_path:match("^(.*)/protocol/v2%.json$"))
  assert(#spawned == 1, "one realm should schedule one background publication")
  assert(spawned[1][1] == "env"
      and spawned[1][2] == "WEZTERM_ATTENTION_DIR=" .. test_dir
      and spawned[1][3] == expected_root .. "/bin/attention",
    "publication must use the resolved checkout command")
  assert(table.concat(spawned[1], " "):find(
    "hooks publish --realm /tmp/attention-u2-test.sock --quiet", 1, true),
    "publication must use the nested quiet realm command")
  assert(config.set_environment_variables.WEZTERM_ATTENTION_ROOT == expected_root,
    "apply_to_config must expose the resolved plugin root")
  assert(config.set_environment_variables.WEZTERM_ATTENTION_DIR == test_dir,
    "apply_to_config must expose the configured state root")
end)

test("unpublished mux panes retry on the bounded schedule and stop when resolved", function()
  local spawned = {}
  local scheduled = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "retry-test", socket_path = "/tmp/attention-retry.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local call_after = function(delay, callback)
    scheduled[#scheduled + 1] = { delay = delay, callback = callback }
  end
  local unpublished = window_double({
    window_id = 815,
    tabs = { { { id = 9911, domain = "retry-test" } } }, focused = false,
  })
  reloaded.poll(unpublished, { call_after = call_after })
  assert(#spawned == 0, "the first pane count observation must not publish")
  reloaded.poll(unpublished, { call_after = call_after })
  assert(#spawned == 1 and scheduled[1].delay == 2,
    "the second stable poll must publish and arm the 2-second retry")
  scheduled[1].callback()
  assert(#spawned == 2 and scheduled[2].delay == 5, "the first retry must use 5 seconds next")
  scheduled[2].callback()
  assert(#spawned == 3 and scheduled[3].delay == 10, "the second retry must use 10 seconds next")
  scheduled[3].callback()
  assert(#spawned == 4 and scheduled[4].delay == 30, "later retries must reach 30 seconds")

  local resolved = window_double({
    window_id = 815,
    tabs = { { { id = 9911, domain = "retry-test", published = 42 } } }, focused = false,
  })
  reloaded.poll(resolved, {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = call_after,
  })
  scheduled[4].callback()
  assert(#spawned == 4, "a resolved domain must cancel its stale retry callback")
  wezterm.background_child_process = original_background
end)

test("a pane-count change restarts stabilization without doubling the schedule", function()
  local spawned = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "stable-test", socket_path = "/tmp/attention-stable.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local call_after = function() end
  reloaded.poll(window_double({
    tabs = { { { id = 9921, domain = "stable-test" } } }, focused = false,
  }), { call_after = call_after })
  local two_panes = window_double({
    tabs = { { { id = 9921, domain = "stable-test" },
      { id = 9922, domain = "stable-test" } } }, focused = false,
  })
  reloaded.poll(two_panes, { call_after = call_after })
  assert(#spawned == 0, "a changed pane count must restart stabilization")
  reloaded.poll(two_panes, { call_after = call_after })
  reloaded.poll(two_panes, { call_after = call_after })
  assert(#spawned == 1, "stable polls and a second window must share one socket schedule")
  wezterm.background_child_process = original_background
end)

test("unequal pane counts in alternating windows start one realm publication", function()
  local spawned = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "window-stable", socket_path = "/tmp/attention-window-stable.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local first = window_double({
    window_id = 811,
    tabs = { { { id = 9923, domain = "window-stable" } } },
    focused = false,
  })
  local second = window_double({
    window_id = 812,
    tabs = { { { id = 9924, domain = "window-stable" },
      { id = 9925, domain = "window-stable" } } },
    focused = false,
  })
  local options = { call_after = function() end }
  reloaded.poll(first, options)
  reloaded.poll(second, options)
  assert(#spawned == 0, "one observation per window must not publish")
  reloaded.poll(first, options)
  reloaded.poll(second, options)
  assert(#spawned == 1,
    "one stable window must start the shared socket schedule despite another pane count")
  wezterm.background_child_process = original_background
end)

test("a resolved window cannot cancel another window's unpublished realm", function()
  local spawned = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "mixed-window", socket_path = "/tmp/attention-mixed-window.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local unpublished = window_double({
    window_id = 813,
    tabs = { { { id = 9926, domain = "mixed-window" } } },
    focused = false,
  })
  local resolved = window_double({
    window_id = 814,
    tabs = { { { id = 9927, domain = "mixed-window", published = 42 } } },
    focused = false,
  })
  local options = {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  }
  reloaded.poll(unpublished, options)
  reloaded.poll(resolved, options)
  reloaded.poll(unpublished, options)
  assert(#spawned == 1,
    "a resolved sibling window must not erase another window's stabilization")
  wezterm.background_child_process = original_background
end)

test("a window leaving a realm cancels its stale publication retry", function()
  local spawned = {}
  local scheduled = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "departed-realm", socket_path = "/tmp/attention-departed.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local remote = window_double({
    window_id = 816,
    tabs = { { { id = 9928, domain = "departed-realm" } } }, focused = false,
  })
  local local_only = window_double({
    window_id = 816,
    tabs = { { { id = 9929, domain = "local" } } }, focused = false,
  })
  local options = {
    call_after = function(delay, callback)
      scheduled[#scheduled + 1] = { delay = delay, callback = callback }
    end,
  }
  reloaded.poll(remote, options)
  reloaded.poll(remote, options)
  assert(#spawned == 1 and #scheduled == 1, "the unpublished realm must start one schedule")
  reloaded.poll(local_only, options)
  scheduled[1].callback()
  assert(#spawned == 1 and #scheduled == 1,
    "leaving the realm must invalidate its pending retry callback")
  wezterm.background_child_process = original_background
end)

test("closing an unpublished window cancels its stale publication retry", function()
  local spawned = {}
  local scheduled = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return true
  end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "closed-realm", socket_path = "/tmp/attention-closed.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local closing = window_double({
    window_id = 817,
    tabs = { { { id = 9930, domain = "closed-realm" } } }, focused = false,
  })
  local remaining = window_double({
    window_id = 818,
    tabs = { { { id = 9931, domain = "closed-realm", published = 42 } } }, focused = false,
  })
  local live_windows = { closing, remaining }
  local options = {
    gui_windows = function() return live_windows end,
    call_after = function(delay, callback)
      scheduled[#scheduled + 1] = { delay = delay, callback = callback }
    end,
  }
  reloaded.poll(closing, options)
  reloaded.poll(remaining, options)
  reloaded.poll(closing, options)
  assert(#spawned == 1 and #scheduled == 1, "the unpublished window must start one schedule")
  live_windows = { remaining }
  reloaded.poll(remaining, options)
  scheduled[1].callback()
  assert(#spawned == 1 and #scheduled == 1,
    "the current GUI-window inventory must invalidate the closed window's retry")
  wezterm.background_child_process = original_background
end)

test("a failed publish logs once and keeps its retry schedule", function()
  local attempted = 0
  local scheduled = {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function()
    attempted = attempted + 1
    return false
  end
  drain_errors()
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "failure-test", socket_path = "/tmp/attention-failure.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false })
  local call_after = function(delay, callback)
    scheduled[#scheduled + 1] = { delay = delay, callback = callback }
  end
  local window = window_double({
    tabs = { { { id = 9931, domain = "failure-test" } } }, focused = false,
  })
  reloaded.poll(window, { call_after = call_after })
  reloaded.poll(window, { call_after = call_after })
  assert(attempted == 1 and scheduled[1].delay == 2,
    "a failed first spawn must still arm the retry")
  scheduled[1].callback()
  assert(attempted == 2 and scheduled[2].delay == 5,
    "a failed retry must keep the schedule")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("failed to start mux identity publication", 1, true),
    "repeated spawn failure must log once")
  wezterm.background_child_process = original_background
end)

test("get_attention_view returns cached provider facts without exposing the cache table", function()
  assert(os.execute("rm -rf " .. shell_quote(test_dir .. "/v2")) == 0)
  materialize_state_case(protocol_fixture.state_case)
  local wire = protocol_fixture.wire_sample
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  local pane = mux_pane(9941, { domain = "unix", attention = wire })
  reloaded.poll(window_double({ tabs = { { {
    id = 9941, domain = "unix", attention = wire,
  } } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })
  local view = assert(reloaded.get_attention_view(pane), "cached view must be available")
  assert(view.provider == "claude" and view.binding_id == protocol_fixture.record_samples.binding.binding_id,
    "the full view must retain provider and binding identity")
  assert(view.binding_phase == "ended" and view.type == "notify"
      and view.subagents == 1 and view.review == true
      and view.reader_confidence == "confirmed",
    "the accessor must return the cached full-pane facts")
  view.provider = "mutated"
  assert(reloaded.get_attention_view(pane).provider == "claude",
    "the accessor must return a copy, not the cache table")
end)

test("lifecycle facts reach the cached reader without changing the badge", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local file = assert(io.open(repo_root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local fixture = decode_json(file:read("*a"))
  file:close()
  local snapshot = fixture.cases[2].value
  snapshot.address, snapshot.launch_id = samples.claim.address, samples.claim.launch_id
  snapshot.binding_id, snapshot.provider = samples.binding.binding_id, samples.binding.provider
  snapshot.pools.requests.observations = fixture.cases[4].value.pools.requests.observations
  local binding_dir = test_dir .. "/v2/realms/" .. snapshot.address.realm_id
    .. "/incarnations/" .. snapshot.address.incarnation_id
    .. "/panes/42/launches/" .. snapshot.launch_id .. "/bindings/" .. snapshot.binding_id
  write_json_path(binding_dir .. "/lifecycle.json", snapshot)
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  local wire = protocol_fixture.wire_sample
  local window = window_double({ tabs = { { { id = 9961, domain = "unix", attention = wire } } }, focused = false })
  local pane = mux_pane(9961, { domain = "unix", attention = wire })
  reloaded.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  local view = assert(reloaded.get_attention_view(pane))
  assert(view.type == "notify" and view.lifecycle.availability == "available")
  assert(#view.lifecycle.observations == 2)
  view.lifecycle.observations[1].actor.kind = "mutated"
  assert(reloaded.get_attention_view(pane).lifecycle.observations[1].actor.kind == "lead")
  local ack = decode_json(encode_json(samples.acknowledgement))
  ack.activity_event_id = samples.activity.event_id
  write_json_path(binding_dir .. "/ack.json", ack)
  local stopped = decode_json(encode_json(samples.subagent_presence))
  stopped.status = "stopped"
  write_json_path(binding_dir .. "/agents/" .. stopped.agent_key .. ".json", stopped)
  local original_open = io.open
  io.open = function(path, mode)
    if path == binding_dir .. "/lifecycle.json" then return nil, "Permission denied" end
    return original_open(path, mode)
  end
  local ok, failure = pcall(reloaded.poll, window, { now_unix_ns = "00000000000000000000", call_after = function() end })
  io.open = original_open
  assert(ok, failure)
  local cached = assert(reloaded.get_attention_view(pane))
  assert(cached.lifecycle.availability == "cached" and #cached.lifecycle.observations == 2)
  assert(cached.activity_type == nil and cached.subagents == 0, "cached facts cannot undo fresh ack or child stop")
  local skew = false
  for _, problem in ipairs(cached.lifecycle.diagnostics) do if problem.code == "clock_skew" then skew = true end end
  assert(skew, "cached facts retain written UTC and report negative age")
  write_json_path(binding_dir .. "/ack.json", samples.acknowledgement)
  local raw = assert(io.open(binding_dir .. "/lifecycle.json", "w"))
  raw:write('{"schema":3}'); raw:close()
  reloaded.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  local future = assert(reloaded.get_attention_view(pane))
  assert(future.type == "notify" and future.lifecycle.availability == "unsupported")
  assert(#future.lifecycle.observations == 0, "successful future read must not recover cached facts")
  write_json_path(binding_dir .. "/lifecycle.json", snapshot)
  reloaded.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  assert(#reloaded.get_attention_view(pane).lifecycle.observations == 2)
  local next_id = string.rep("e", 64)
  local launch_dir = assert(binding_dir:match("^(.*)/bindings/[^/]+$"))
  local next_binding = decode_json(encode_json(samples.binding)); next_binding.binding_id = next_id
  local next_pointer = decode_json(encode_json(samples.current_binding)); next_pointer.binding_id = next_id
  write_json_path(launch_dir .. "/bindings/" .. next_id .. "/binding.json", next_binding)
  write_json_path(launch_dir .. "/current-binding.json", next_pointer)
  io.open = function(path, mode)
    if path == launch_dir .. "/bindings/" .. next_id .. "/lifecycle.json" then return nil, "Permission denied" end
    return original_open(path, mode)
  end
  ok, failure = pcall(reloaded.poll, window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  io.open = original_open
  assert(ok, failure)
  local replaced = assert(reloaded.get_attention_view(pane))
  assert(replaced.binding_id == next_id and replaced.lifecycle.availability == "unavailable")
  assert(#replaced.lifecycle.observations == 0, "new binding cannot recover old binding facts")
  os.remove(binding_dir .. "/lifecycle.json")
end)

test("question publication, tool return, and badge dismissal stay independent", function()
  local wire = materialize_v2_fixture(9971)
  local samples = protocol_fixture.record_samples
  local binding = decode_json(encode_json(samples.binding))
  binding.address, binding.provider = wire.address, "codex"
  local directory = test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/" .. wire.address.incarnation_id
    .. "/panes/9971/launches/" .. wire.launch_id .. "/bindings/" .. binding.binding_id
  write_json_path(directory .. "/binding.json", binding)
  os.remove(directory .. "/end.json")
  for _, entry in ipairs(protocol_fixture.state_case.files) do
    if entry.path:find("/agents/", 1, true) then
      local presence = decode_json(encode_json(samples[entry.sample]))
      presence.address, presence.provider = wire.address, "codex"
      write_json_path(test_dir .. "/" .. entry.path:gsub("/panes/42/", "/panes/9971/", 1), presence)
    end
  end
  local file = assert(io.open(repo_root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local fixture = decode_json(file:read("*a")); file:close()
  local snapshot = fixture.cases[18].value -- nonblocking tool result, without Pre
  snapshot.address, snapshot.launch_id, snapshot.binding_id = wire.address, wire.launch_id, binding.binding_id
  snapshot.pools.general = fixture.cases[1].value.pools.general
  local post = snapshot.pools.requests.observations[1]
  post.correlation = { tool_call_id = "question-q1", turn_id = "turn-1" }
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  local window = window_double({ tabs = { { { id = 9971, domain = "unix", attention = wire } } }, focused = false })
  local pane = mux_pane(9971, { domain = "unix", attention = wire })
  local function read(focused)
    write_json_path(directory .. "/lifecycle.json", snapshot)
    local target = focused and window_double({ tabs = { { { id = 9971, domain = "unix", attention = wire } } }, focused = true, active_pane_id = { id = 9971, domain = "unix", attention = wire } }) or window
    reloaded.poll(target, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
    return assert(reloaded.get_attention_view(pane))
  end
  local view = read(false)
  local consumer = dofile(repo_root .. "/tests/fixtures/lifecycle/consumer.lua").new()
  local other_consumer = dofile(repo_root .. "/tests/fixtures/lifecycle/consumer.lua").new()
  assert(consumer.appearance(view) == "follow_up" and other_consumer.appearance(view) == "follow_up")
  consumer.dismiss()
  assert(consumer.appearance(reloaded.get_attention_view(pane)) == "base")
  assert(other_consumer.appearance(reloaded.get_attention_view(pane)) == "follow_up")
  assert(view.lifecycle.requests[1].question_mode == "nonblocking")
  assert(#view.lifecycle.requests[1].request_observation_ids == 0)
  assert(view.lifecycle.requests[1].publication_observation_ids[1] == post.observation_id)
  assert(#view.lifecycle.requests[1].relations == 0 and #view.lifecycle.requests[1].result_observation_ids == 0)
  local pre = fixture.cases[17].value.pools.requests.observations[1]
  pre.correlation = post.correlation
  pre.observed_mono_ns = "00000000000000000600"
  snapshot.pools.requests.observations[2] = pre
  view = read(false)
  assert(#view.lifecycle.requests == 1 and #view.lifecycle.requests[1].request_observation_ids == 1)
  assert(view.lifecycle.requests[1].publication_observation_ids[1] == post.observation_id, "late Pre cannot mint a publication")
  local dismissed = read(true)
  assert(consumer.appearance(dismissed) == "base", "badge focus and a late Pre cannot undo consumer dismissal")
  assert(dismissed.lifecycle.badge_acknowledgement.activity_event_id == samples.activity.event_id)
  assert(#dismissed.lifecycle.requests[1].publication_observation_ids == 1, "focus is not an answer")
  assert(dismissed.address.pane_id == "9971" and dismissed.launch_id == wire.launch_id and dismissed.binding_health == "valid")
  dismissed.lifecycle.requests[1].publication_observation_ids[1] = "mutated"
  dismissed.address.pane_id = "changed"
  local again = reloaded.get_attention_view(pane)
  assert(again.address.pane_id == "9971" and again.lifecycle.requests[1].publication_observation_ids[1] == post.observation_id)
  post.tool_name, post.question_mode, pre.tool_name, pre.question_mode = "request_user_input", "blocking", "request_user_input", "blocking"
  view = read(false)
  assert(#view.lifecycle.requests[1].publication_observation_ids == 0)
  assert(view.lifecycle.requests[1].relations[1].kind == "tool_result_observed", "blocking return is separate from async publication")
  post.correlation = nil
  view = read(false)
  assert(#view.lifecycle.requests == 2, "ID-less return cannot join by name")
  pre.correlation = { tool_call_id = post.observation_id }
  view = read(false)
  assert(#view.lifecycle.requests == 2, "local observation UUID is not a native tool ID")
  post.correlation = pre.correlation
  post.actor = { kind = "child", agent_id = "child-a", agent_key = internal.sha256("child-a") }
  pre.actor = { kind = "child", agent_id = "child-b", agent_key = internal.sha256("child-b") }
  view = read(false)
  assert(#view.lifecycle.requests == 2 and #view.lifecycle.requests[1].relations == 0 and #view.lifecycle.requests[2].relations == 0,
    "sibling children cannot correlate equal native tool IDs")
  assert(view._records == nil and view.cache_key == nil and view.next_wakeup_unix_ns == nil)
end)

test("consumer dismissal is scoped to displayed publications and never implies an answer", function()
  local module = dofile(repo_root .. "/tests/fixtures/lifecycle/consumer.lua")
  local view = {
    address = { realm_id = string.rep("a", 64), incarnation_id = string.rep("b", 64), pane_id = "42" },
    launch_id = "00000000-0000-4000-8000-000000000001", binding_id = string.rep("c", 64),
    binding_phase = "active", binding_health = "valid", reader_confidence = "confirmed", pane_presence = "present", activity_type = "thinking",
    lifecycle = { availability = "available", retention_floors = {}, requests = { { kind = "question", question_mode = "nonblocking", publication_observation_ids = { "publication-1" } } } },
  }
  local first, second = module.new(), module.new()
  assert(first.appearance(view) == "follow_up" and second.appearance(view) == "follow_up")
  view.lifecycle.requests[1].publication_observation_ids[2] = "publication-2"
  first.dismiss() -- rendered before publication-2 arrived
  assert(first.appearance(view) == "follow_up", "unseen Q2 must not be dismissed with Q1")
  first.dismiss()
  view.activity_type = "stop"
  view.lifecycle.badge_acknowledgement = { activity_event_id = "badge-1" }
  view.lifecycle.snapshot_id = "unrelated-snapshot-rewrite"
  assert(first.appearance(view) == "base" and second.appearance(view) == "follow_up")
  assert(view.lifecycle.requests[1].publication_observation_ids[1] == "publication-1")
  view.lifecycle.requests[1].publication_observation_ids = { "publication-2" }
  view.lifecycle.retention_floors.requests = "00000000000000000100"
  assert(first.appearance(view) == "unknown", "eviction is not resolution")
  view.lifecycle.availability = "cached"
  assert(first.appearance(view) == "unknown")
  view.binding_phase = "ended"
  assert(first.appearance(view) == "unknown", "ended binding loses its tint")
  view.binding_phase, view.binding_id = "active", string.rep("d", 64)
  view.lifecycle.availability, view.lifecycle.retention_floors = "available", {}
  assert(first.appearance(view) == "follow_up", "new binding does not inherit dismissal")
end)

test("twenty full lifecycle panes keep polling and getter work bounded", function()
  local file = assert(io.open(repo_root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local fixture = decode_json(file:read("*a")); file:close()
  local entries, paths = {}, {}
  for index = 1, 20 do
    local id = 11000 + index
    local wire = materialize_v2_fixture(id)
    entries[#entries + 1] = { id = id, domain = "unix", attention = wire }
    for _, entry in ipairs(protocol_fixture.state_case.files) do
      paths[#paths + 1] = test_dir .. "/" .. entry.path:gsub("/panes/42/", "/panes/" .. id .. "/", 1)
    end
    local snapshot = decode_json(encode_json(fixture.cases[2].value))
    snapshot.address, snapshot.launch_id = wire.address, wire.launch_id
    snapshot.binding_id, snapshot.provider = protocol_fixture.record_samples.binding.binding_id, "claude"
    snapshot.pools.requests.observations, snapshot.pools.general.observations = {}, {}
    for member = 1, 64 do
      for _, pool in ipairs({ "requests", "general" }) do
        local item = decode_json(encode_json(fixture.cases[2].value.pools.general.observations[1]))
        item.observation_id = string.format("00000000-0000-4000-8000-%012d", member + (pool == "requests" and 1000 or 2000))
        item.observed_mono_ns = string.format("%020d", member)
        item.correlation = { tool_call_id = pool .. member }
        if pool == "requests" then item.tool_name, item.tool_class, item.question_mode = "AskUserQuestion", "question", "blocking" end
        snapshot.pools[pool].observations[member] = item
      end
    end
    local directory = test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/" .. wire.address.incarnation_id
      .. "/panes/" .. id .. "/launches/" .. wire.launch_id .. "/bindings/" .. snapshot.binding_id
    write_json_path(directory .. "/lifecycle.json", snapshot)
  end
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  local window = window_double({ tabs = { entries }, focused = false })
  local old_open, old_popen = io.open, io.popen
  local reads, writes, globs = 0, 0, 0
  io.open = function(path, mode)
    if mode and mode:find("w", 1, true) then writes = writes + 1 end
    if path:match("/lifecycle%.json$") then reads = reads + 1 end
    return old_open(path, mode)
  end
  io.popen = function() error("lifecycle polling/getter cannot launch a subprocess") end
  local started = os.clock()
  local ok, failure = pcall(function()
    for _ = 1, 2 do
      instance.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end,
        glob = function(pattern)
          globs = globs + 1
          local directory = assert(pattern:match("^(.*)/%*%.json$"))
          assert(directory:match("/reviews$") or directory:match("/agents$"), "no historical directory walk")
          local matches = {}
          for _, path in ipairs(paths) do if dirname(path) == directory then matches[#matches + 1] = path end end
          return matches
        end,
      })
    end
    local before_getters = reads
    for _, entry in ipairs(entries) do
      local view = assert(instance.get_attention_view(mux_pane(entry.id, entry)))
      assert(view.lifecycle.availability == "available" and #view.lifecycle.observations == 128)
    end
    assert(reads == before_getters, "getters cannot read files")
  end)
  local cpu_ms = (os.clock() - started) * 1000
  io.open, io.popen = old_open, old_popen
  assert(ok, failure)
  assert(reads == 40 and globs == 80 and writes == 0, "exactly one sidecar read per pane/poll and no writes")
  io.write(string.format("lifecycle workload: 20 panes, 2 polls, 128 observations each; CPU %.2f ms; 40 snapshot reads; 0 writes\n", cpu_ms))
end)

test("lifecycle snapshot grammar agrees with the shared fixture", function()
  local api = dofile(repo_root .. "/plugin/protocol.lua")({ wezterm = wezterm, protocol_path = repo_root .. "/protocol/v2.json" })
  local file = assert(io.open(repo_root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local fixture = assert(api.decode_json(file:read("*a")))
  file:close()
  for _, case in ipairs(fixture.cases) do
    local parsed, problem = api.parse_v2_record(case.value, "lifecycle_snapshot")
    assert((parsed and "valid" or problem.code) == case.expected, case.id)
  end
  for _, case in ipairs(fixture.raw_cases) do
    local parsed, problem = api.parse_v2_record_json(case.raw, "lifecycle_snapshot")
    assert((parsed and "valid" or problem.code) == case.expected, case.id)
  end
  local original_open = io.open
  io.open = function(path, mode)
    if path ~= "/synthetic/lifecycle.json" then return original_open(path, mode) end
    return { read = function(_, count)
      assert(count == 262145, "lifecycle read must stop at bound plus one byte")
      return string.rep(" ", count)
    end, close = function() return true end }
  end
  local ok, result, problem, status = pcall(api.read_record_file, "/synthetic/lifecycle.json", "lifecycle_snapshot")
  io.open = original_open
  assert(ok and not result and problem.code == "record_invalid" and status == "invalid")
  local parsed = api.parse_v2_record_json(string.rep("[", 9) .. string.rep("]", 9), "lifecycle_snapshot")
  assert(not parsed, "deep lifecycle containers must reject")
end)

test("a missing protocol module logs once and keeps the v1 reader available", function()
  local module_path = repo_root .. "/plugin/protocol.lua"
  local hidden_path = module_path .. ".missing"
  assert(os.rename(module_path, hidden_path))
  drain_errors()
  local ok, fallback = pcall(dofile, repo_root .. "/plugin/init.lua")
  assert(os.rename(hidden_path, module_path))
  assert(ok and fallback, "the plugin must load without its v2 protocol module")
  write_marker("9951", "stop", "missing-module-v1")
  local atype = fallback.get_attention("9951", { dir = test_dir, now_ms = 1000 })
  assert(atype == "stop", "the missing v2 module must not disable v1 rendering")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("protocol", 1, true),
    "the missing module must produce one named log line")
end)

os.execute("rm -rf " .. shell_quote(test_dir))

io.write(string.format("%d passed, %d failed\n", passed, failed))
if failed > 0 then os.exit(1) end
