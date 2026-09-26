local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = source:match("^(.*)/tests/lua/auto_clear_spec.lua$") or "."

local function shell_quote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

local test_dir = os.tmpname()
os.remove(test_dir)
assert(os.execute("mkdir -p " .. shell_quote(test_dir)) == 0)

local logged_errors = {}
-- Warnings are captured apart from errors: an unexpected error fails a test,
-- a warning is only what a test chooses to assert.
local logged_warnings = {}

local function drain_warnings()
  local drained = {}
  for i, message in ipairs(logged_warnings) do
    drained[i] = message
    logged_warnings[i] = nil
  end
  return drained
end

local function drain_errors()
  local drained = {}
  for i, message in ipairs(logged_errors) do
    drained[i] = message
    logged_errors[i] = nil
  end
  return drained
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
          local function hex_at(at)
            local hex = content:sub(at, at + 3)
            assert(hex:match("^[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]$"),
              "invalid JSON unicode escape")
            return tonumber(hex, 16)
          end
          local code = hex_at(index + 2)
          index = index + 6
          if code >= 0xD800 and code <= 0xDBFF then
            assert(content:sub(index, index + 1) == "\\u", "unpaired JSON surrogate")
            local low = hex_at(index + 2)
            assert(low >= 0xDC00 and low <= 0xDFFF, "unpaired JSON surrogate")
            code = 0x10000 + (code - 0xD800) * 0x400 + (low - 0xDC00)
            index = index + 6
          end
          assert(code < 0xD800 or code > 0xDFFF, "unpaired JSON surrogate")
          -- UTF-8, as serde_json and WezTerm's decoder produce it.
          if code < 0x80 then
            parts[#parts + 1] = string.char(code)
          elseif code < 0x800 then
            parts[#parts + 1] = string.char(0xC0 + math.floor(code / 0x40), 0x80 + code % 0x40)
          elseif code < 0x10000 then
            parts[#parts + 1] = string.char(0xE0 + math.floor(code / 0x1000),
              0x80 + math.floor(code / 0x40) % 0x40, 0x80 + code % 0x40)
          else
            parts[#parts + 1] = string.char(0xF0 + math.floor(code / 0x40000),
              0x80 + math.floor(code / 0x1000) % 0x40,
              0x80 + math.floor(code / 0x40) % 0x40, 0x80 + code % 0x40)
          end
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
-- The mux windows this test has built, which the mux double lists: the plugin
-- asks the mux which windows still exist. Cleared per test, so one test's
-- windows cannot vouch for another's.
-- Keyed by window id, because a test builds a fresh double for each poll and a
-- mux window has one current content, not one per time it was looked at.
local mux_windows_by_id = {}
-- Defined with the fixtures below; the mux double hands out its panes.
local mux_pane

local wezterm = {
  home_dir = test_dir,
  -- WezTerm keeps this one table across config reloads, while every module
  -- the plugin loads starts afresh; a reload here is a second dofile.
  GLOBAL = {},
  mux = {
    all_windows = function()
      local all = {}
      for _, mux_window in pairs(mux_windows_by_id) do all[#all + 1] = mux_window end
      return all
    end,
    -- A pane in one of this test's mux windows, else a local pane: a tab the
    -- GUI draws always has its panes in the mux, and most tests draw local ones.
    get_pane = function(pane_id)
      for _, mux_window in pairs(mux_windows_by_id) do
        for _, mux_tab in ipairs(mux_window.tabs()) do
          local ok, panes = pcall(mux_tab.panes, mux_tab)
          for _, candidate in ipairs(ok and panes or {}) do
            if candidate.pane_id() == pane_id then return candidate end
          end
        end
      end
      return mux_pane(pane_id)
    end,
  },
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
    local directory, extension = pattern:match("^(.*)/%*%.(%w+)$")
    if not directory then return {} end
    local pipe = io.popen("find " .. shell_quote(directory)
      .. " -maxdepth 1 -type f -name '*." .. extension .. "' -print 2>/dev/null")
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
  log_warn = function(message)
    table.insert(logged_warnings, message)
  end,
  on = function(event, callback)
    handlers[event] = handlers[event] or {}
    table.insert(handlers[event], callback)
  end,
}

package.preload.wezterm = function() return wezterm end

--- An integration root with the writer installed. Publication and tab source
--- identity run only once the writer is there, so the tests about them name
--- this root rather than depend on whether this checkout has been built.
local writer_root = test_dir .. "/writer-root"
assert(os.execute("mkdir -p " .. shell_quote(writer_root .. "/libexec") .. " "
  .. shell_quote(writer_root .. "/bin")) == 0)
assert(io.open(writer_root .. "/libexec/attention-rs", "w")):close()

local attention = dofile(repo_root .. "/plugin/init.lua")
attention.apply_to_config({}, {
  auto_poll = false,
  dir = test_dir,
  review_key = false,
  integration_root = writer_root,
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
-- The fixture interpreter lives in tests/, not in the shipped plugin. It drives
-- the production parsers, which still come from internal.
local fixtures = dofile(repo_root .. "/tests/lua/support/protocol_fixtures.lua")(internal)

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
-- The plugin encodes a tab order's source with WezTerm's own encoder.
wezterm.json_encode = encode_json

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

local function copy_json(value)
  return decode_json(encode_json(value))
end

--- The time every poll below reads its records at, unless a test says. It is
--- also WezTerm's clock, for a poll the plugin starts itself.
local fixture_now = protocol_fixture.state_case.now_unix_ns
wezterm.time = { now = function()
  return { format_utc = function() return (fixture_now:gsub("^0+", "")) end }
end }

--- The realm the panes below live in, apart from the fixture's own, so a
--- test that seeds the fixture's pane 42 and one that seeds a pane here
--- never share a directory.
local seeded_realm = string.rep("e", 64)

--- The identity each pane id below publishes once a test has given it
--- records. A window double's bare pane id then stands for a mux-client pane
--- publishing it, whose GUI-local number is the pane id.
local seeded_wires = {}

local function seeded_wire(pane_id)
  local key = tostring(pane_id)
  local wire = seeded_wires[key]
  if wire then return wire end
  wire = copy_json(protocol_fixture.wire_sample)
  wire.address.realm_id = seeded_realm
  wire.address.pane_id = key
  seeded_wires[key] = wire
  return wire
end

local function seeded_pane_root(pane_id)
  local wire = seeded_wire(pane_id)
  return test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/"
    .. wire.address.incarnation_id .. "/panes/" .. wire.address.pane_id
end

--- Where the seeded pane's activity and its acknowledgement live.
local function seeded_records_root(pane_id)
  local wire = seeded_wire(pane_id)
  return seeded_pane_root(pane_id) .. "/launches/" .. wire.launch_id .. "/bindings/"
    .. protocol_fixture.record_samples.binding.binding_id
end

--- The key the plugin caches a seeded pane's view under.
local function seeded_key(pane_id)
  return internal.address_cache_key(seeded_wire(pane_id).address)
end

local function user_review_path(pane_id)
  return seeded_pane_root(pane_id) .. "/reviews/" .. internal.sha256("user") .. ".json"
end

--- Records of the fixture's kind `sample`, moved to the seeded pane.
local function seeded_record(pane_id, sample)
  local wire = seeded_wire(pane_id)
  local record = copy_json(protocol_fixture.record_samples[sample])
  if record.realm_id then record.realm_id = wire.address.realm_id end
  if record.address then record.address = copy_json(wire.address) end
  return record
end

local event_counter = 0
local function next_event_id()
  event_counter = event_counter + 1
  return string.format("00000000-0000-4000-a000-%012d", event_counter)
end

--- A claimed and bound pane with nothing to show: whatever an earlier test
--- left for the same pane id is gone.
local function seed_pane(pane_id)
  local wire = seeded_wire(pane_id)
  assert(os.execute("rm -rf " .. shell_quote(seeded_pane_root(pane_id))) == 0)
  local realm_root = test_dir .. "/v2/realms/" .. wire.address.realm_id
  write_json_path(realm_root .. "/realm.json", seeded_record(pane_id, "realm"))
  write_json_path(realm_root .. "/incarnations/" .. wire.address.incarnation_id
    .. "/incarnation.json", seeded_record(pane_id, "incarnation"))
  local pane_root = seeded_pane_root(pane_id)
  write_json_path(pane_root .. "/claim.json", seeded_record(pane_id, "claim"))
  write_json_path(pane_root .. "/launches/" .. wire.launch_id .. "/current-binding.json",
    seeded_record(pane_id, "current_binding"))
  write_json_path(seeded_records_root(pane_id) .. "/binding.json", seeded_record(pane_id, "binding"))
  return wire
end

--- Publish `activity_type` for the pane, as its agent's hook does, and
--- return the activity's event id. A pane with no records yet is seeded
--- first. `review` is the user's flag, which the review key sets.
local function write_activity(pane_id, activity_type, frame)
  if not path_exists(seeded_pane_root(pane_id) .. "/claim.json") then seed_pane(pane_id) end
  if activity_type == "review" then
    local review = seeded_record(pane_id, "review")
    review.event_id = next_event_id()
    write_json_path(user_review_path(pane_id), review)
    return review.event_id
  end
  local activity = seeded_record(pane_id, "activity")
  activity.type = activity_type
  activity.label = nil
  activity.frame = frame
  activity.event_id = next_event_id()
  activity.observed_mono_ns = string.format("%020d", 3000000000 + event_counter)
  write_json_path(seeded_records_root(pane_id) .. "/activity.json", activity)
  return activity.event_id
end

local function activity_exists(pane_id)
  return path_exists(seeded_records_root(pane_id) .. "/activity.json")
end

local function clear_activity(pane_id)
  os.remove(seeded_records_root(pane_id) .. "/activity.json")
end

--- Record that the user saw the pane's activity `event_id`, as the attention
--- command does when the plugin asks it to.
local function write_acknowledgement(pane_id, event_id)
  local ack = seeded_record(pane_id, "acknowledgement")
  ack.activity_event_id = event_id
  ack.event_id = next_event_id()
  write_json_path(seeded_records_root(pane_id) .. "/ack.json", ack)
end

local function acknowledgement_exists(pane_id)
  return path_exists(seeded_records_root(pane_id) .. "/ack.json")
end

--- The command lines the plugin ran through wezterm.run_child_process while
--- `callback` ran. `answer(argv)` gives the child's (success, stdout); by
--- default an "applied" answer, which is all a test that only watches the
--- command line needs.
local function applied_answer(argv)
  local action = table.concat(argv, " "):match(" plugin ([%w%-]+)") or "unknown"
  return true, '{"schema":1,"command":"plugin ' .. action .. '","status":"ok",'
    .. '"complete":true,"result":{"disposition":"applied","diagnostic":null,'
    .. '"event_id":"00000000-0000-4000-a000-999999999999"},"diagnostics":[]}'
end

local function with_plugin_command(answer, callback)
  local spawned = {}
  local previous = wezterm.run_child_process
  wezterm.run_child_process = function(argv)
    spawned[#spawned + 1] = argv
    return (answer or applied_answer)(argv)
  end
  local ok, failure = pcall(callback)
  wezterm.run_child_process = previous
  assert(ok, failure)
  return spawned
end

--- The `attention plugin` arguments of a spawned command line, after the
--- executable, as one string.
local function plugin_arguments(argv)
  for index, value in ipairs(argv) do
    if value:sub(-#"/bin/attention") == "/bin/attention" then
      return table.concat(argv, " ", index + 1)
    end
  end
  return nil
end

--- What the attention command does for `plugin set-review` and
--- `plugin clear-review` on a seeded pane, answered as it answers.
local function reviewing_answer(argv)
  local arguments = assert(plugin_arguments(argv), "not an attention command line")
  local action = assert(arguments:match("^plugin (%S+)"))
  local function flag(name) return assert(arguments:match("%-%-" .. name .. " (%S+)"), name) end
  local address = {
    realm_id = flag("realm%-id"), incarnation_id = flag("incarnation%-id"), pane_id = flag("pane%-id"),
  }
  local path = test_dir .. "/v2/realms/" .. address.realm_id .. "/incarnations/"
    .. address.incarnation_id .. "/panes/" .. address.pane_id .. "/reviews/"
    .. internal.sha256("user") .. ".json"
  local disposition = "applied"
  if action == "set-review" then
    local review = copy_json(protocol_fixture.record_samples.review)
    review.address, review.event_id = address, next_event_id()
    write_json_path(path, review)
  elseif path_exists(path) then
    os.remove(path)
  else
    disposition = "skipped"
  end
  return true, '{"schema":1,"command":"plugin ' .. action .. '","status":"ok","complete":true,'
    .. '"result":{"disposition":"' .. disposition .. '","diagnostic":null,"event_id":null},'
    .. '"diagnostics":[]}'
end


--- A mux pane double.
---
--- `spec.domain` is the pane's domain name ("local" unless a test says
--- otherwise) and `spec.published` is the value the pane has published as its
--- WEZTERM_PANE user var. A plain number therefore describes the ordinary
--- case: a local pane whose local id is also its marker id.
mux_pane = function(pane_id, spec)
  spec = spec or {}
  -- A pane whose handle answers but whose mux resolution does not: pane_id is
  -- held by the handle, while the other two go through the mux and fail together.
  if spec.unresolvable then
    return {
      pane_id = function() return pane_id end,
      get_domain_name = function() error("pane " .. pane_id .. " not found in mux") end,
      get_user_vars = function() error("pane " .. pane_id .. " not found in mux") end,
      get_title = function() error("pane " .. pane_id .. " not found in mux") end,
    }
  end
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
--- { id, domain, published, attention }. A bare id a test has seeded records
--- for stands for the mux-client pane that publishes them.
local function pane_from_entry(entry)
  local seeded = type(entry) ~= "table" and seeded_wires[tostring(entry)]
  if seeded then return mux_pane(entry, { domain = "unix", attention = seeded }) end
  if type(entry) == "table" then
    return mux_pane(entry.id, {
      unresolvable = entry.unresolvable,
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
  -- A tab is a list of pane entries, or { tab_id = N, panes = {...} } when the
  -- test needs the id to stay the same across polls. { tab_id = N, gone = true }
  -- is a tab WezTerm still lists and the mux has already dropped: tabs() returns
  -- it, tab_id answers, and panes() raises.
  for index, entry in ipairs(spec.tabs or {}) do
    local tab_id, pane_ids, gone = index, entry, false
    if type(entry) == "table" and entry.tab_id then
      tab_id, pane_ids, gone = entry.tab_id, entry.panes or {}, entry.gone or false
    end
    local panes = {}
    if not gone then
      for _, pane_entry in ipairs(pane_ids) do
        table.insert(panes, pane_from_entry(pane_entry))
      end
    end
    table.insert(mux_tabs, {
      tab_id = function() return tab_id end,
      panes = function()
        if gone then error("tab id " .. tab_id .. " not found in mux") end
        return panes
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

  local mux_window = {
    tabs = function() return mux_tabs end,
    window_id = function() return assigned_window_id end,
  }
  mux_windows_by_id[assigned_window_id] = mux_window

  function w.mux_window()
    return mux_window
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
  attention.poll(window_double({ tabs = { pane_ids }, focused = false }),
    { now_unix_ns = fixture_now })
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
  local opts = { now_unix_ns = fixture_now }
  for key, value in pairs(spec.opts or {}) do opts[key] = value end
  attention.poll(w, opts)
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
  drain_warnings()
  for key in pairs(mux_windows_by_id) do mux_windows_by_id[key] = nil end
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

-- ── Visible-attention projection ────────────────────────────────────────────

test("projection returns the highest-priority cached pane and ignores uncached ones", function()
  write_activity(711, "thinking")
  write_activity(712, "notify")
  poll({ 711, 712 })

  local visible = internal.resolve_visible_attention({ seeded_key(711), seeded_key(712), "799" })
  assert(visible.type == "notify", "notify outranks thinking, got " .. tostring(visible.type))
  assert(visible.indicator == "! ", "indicator should be the notify glyph")
  assert(visible.color == "#240f16", "color should be the notify tint")

  local empty = internal.resolve_visible_attention({ "799" })
  assert(empty.type == nil and empty.indicator == "" and empty.color == nil,
    "a pane with no cache entry should project no attention")
end)

test("generated thinking frames are stable inside one wall-clock bucket", function()
  write_activity(731, "thinking")
  local w = window_double({ tabs = { { 731 } }, focused = false })

  attention.poll(w, { now_ms = 1000, now_unix_ns = fixture_now })
  local _, f0 = attention.get_attention(731)
  attention.poll(w, { now_ms = 1500, now_unix_ns = fixture_now })
  local _, f1 = attention.get_attention(731)
  attention.poll(w, { now_ms = 1999, now_unix_ns = fixture_now })
  local _, f2 = attention.get_attention(731)
  assert(f0 == 1 and f1 == 1 and f2 == 1,
    "one bucket should keep one frame but got "
      .. tostring(f0) .. "," .. tostring(f1) .. "," .. tostring(f2))

  attention.poll(w, { now_ms = 2000, now_unix_ns = fixture_now })
  local _, next_frame = attention.get_attention(731)
  assert(next_frame == 2, "crossing the bucket should advance to frame 2, got " .. tostring(next_frame))
end)

test("time-derived frames preserve the public get_attention return shape", function()
  write_activity(732, "thinking")
  local w = window_double({ tabs = { { 732 } }, focused = false })

  attention.poll(w, { now_ms = 3000, now_unix_ns = fixture_now })
  local state, frame = attention.get_attention(732)

  assert(state == "thinking", "the first return remains the marker type")
  assert(frame == 3, "the second return remains the derived frame, got " .. tostring(frame))
end)

-- ── Read-only rendering ─────────────────────────────────────────────────────

test("neither renderer clears a marker, even on the active tab", function()
  write_activity(101, "stop")
  write_activity(102, "notify")
  poll({ 101, 102 })

  format_tab_title(tab(101, 102, true))
  attention.wrap_title_formatter(function() return "custom title" end)(tab(101, 102, true))

  assert(activity_exists(101), "the active pane's marker must survive rendering")
  assert(activity_exists(102), "the sibling pane's marker must survive rendering")
  assert(attention.get_attention(101) == "stop", "rendering must not touch the cache")
  assert(attention.get_attention(102) == "notify", "rendering must not touch the cache")
end)

test("the tab renders the highest-priority pane, sibling included", function()
  write_activity(111, "review")
  write_activity(112, "stop")
  poll({ 111, 112 })

  local rendered = format_tab_title(tab(111, 112, true))

  assert(type(rendered) == "table", "an attention tab should carry a tint")
  assert(rendered[1].Background.Color == "#12271c", "tint should be the stop color")
  assert(rendered[2].Text:find("✓ ", 1, true), "stop outranks review and should be shown")
end)

test("both renderers project the same attention for the same tab", function()
  write_activity(741, "review")
  write_activity(742, "stop")
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

-- ── Publishing the drawn tab order ─────────────────────────────────────────

--- WezTerm's format-tab-title argument is TabInformation userdata, not a table.
--- `newproxy` is the luajit stand-in: `type()` is `"userdata"` and field reads
--- go through `__index`.
local function as_userdata(fields)
  assert(newproxy, "the harness needs newproxy to stand in for TabInformation")
  local proxy = newproxy(true)
  getmetatable(proxy).__index = fields
  return proxy
end

--- A GUI tab double the renderer can publish from: it carries the window and
--- tab ids WezTerm supplies on TabInformation alongside the drawn index.
local function gui_tab(spec)
  local panes = {}
  for index, pane_id in ipairs(spec.panes) do panes[index] = gui_pane(pane_id) end
  local tab = as_userdata({
    tab_id = spec.tab_id,
    window_id = spec.window_id,
    tab_index = spec.tab_index,
    is_active = spec.is_active == true,
    active_pane = panes[1],
    panes = panes,
  })
  assert(type(tab) == "userdata",
    "the double must be userdata, the way WezTerm's TabInformation is")
  return tab
end

local function tab_publication_path(window_id)
  return test_dir .. "/tabs/" .. window_id .. ".json"
end

local function read_tab_publication(window_id)
  local content = read_path(tab_publication_path(window_id))
  if not content then return nil end
  return decode_json(content)
end

--- The text a format-tab-title return carries, tinted or not, so a test can
--- compare what was published against what that same call drew.
local function rendered_text(rendered)
  if type(rendered) == "string" then return rendered end
  for _, item in ipairs(rendered) do
    if type(item) == "table" and type(item.Text) == "string" then return item.Text end
  end
  return nil
end

local function tab_source_response(socket, incarnation)
  return '{"schema":1,"command":"tab-source","status":"ok","complete":true,"result":{'
    .. '"socket_path":' .. encode_json_string(socket) .. ',"realm_id":"' .. internal.sha256(socket)
    .. '","incarnation_id":"' .. string.rep(incarnation or "a", 64) .. '"},"diagnostics":[]}'
end

test("tab source parsing refuses when the plugin manifest is unavailable", function()
  -- A runtime bound to a protocol module whose manifest did not load.
  local function unloaded_manifest_runtime(values)
    values.protocol_api = { now_ms = function() return os.time() * 1000 end }
    values.overlays = { report_error_once = function() end }
    values.reader, values.titles = {}, {}
    return dofile(repo_root .. "/plugin/runtime.lua")().bind(values)
  end
  local degraded = unloaded_manifest_runtime({ M = {} })
  assert(degraded.parse_tab_source_response(tab_source_response("/test/gui.sock")) == nil)
  -- No answer could be read, so a tab order drawn meanwhile is not held for one.
  local writer = unloaded_manifest_runtime({
    M = { _active_integration_root = writer_root, _active_writer_installed = true },
    wezterm = { run_child_process = function() return true, tab_source_response("/test/gui.sock"), "" end },
  })
  writer.acquire_tab_source("/test/gui.sock")
  assert(writer.tab_source_status() == "unavailable")
end)

test("tab source acquisition is single flight and rejects stale completion", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  local calls = 0
  wezterm.run_child_process = function(args)
    calls = calls + 1
    internal.acquire_tab_source("/test/gui.sock")
    assert(calls == 1, "another window must not start overlapping acquisition")
    return true, tab_source_response(args[4]), ""
  end
  internal.acquire_tab_source("/test/gui.sock")
  assert(calls == 1 and internal.tab_source().socket_path == "/test/gui.sock")
  internal.acquire_tab_source("/test/gui.sock")
  assert(calls == 1, "successful identity is reused")
  internal.reset_tab_source()
  wezterm.run_child_process = function(args)
    internal.reset_tab_source()
    return true, tab_source_response(args[4]), ""
  end
  internal.acquire_tab_source("/test/gui.sock")
  assert(internal.tab_source() == nil, "a late completion cannot revive a previous generation")
  wezterm.run_child_process = previous
  internal.reset_tab_source()
end)

test("a failed source acquisition backs off before it runs again", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  local calls = 0
  wezterm.run_child_process = function() calls = calls + 1; return false, "", "unused failure text" end
  internal.acquire_tab_source("/test/failed.sock")
  internal.acquire_tab_source("/test/failed.sock")
  assert(calls == 1 and internal.tab_source() == nil, "retry waits for its backoff")
  assert(not internal.parse_tab_source_response(tab_source_response("/test/gui.sock"):gsub('"complete":true', '"complete":false')))
  assert(not internal.parse_tab_source_response(tab_source_response("/test/gui.sock"):gsub('"realm_id":"[a-f0-9]+"', '"realm_id":"' .. string.rep("0",64) .. '"')))
  assert(not internal.parse_tab_source_response('{}'))
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  drain_errors()
end)

--- Make this GUI's own socket visible to the plugin while `callback` runs, as
--- WezTerm's does in the GUI process, so a source can be pending.
local function with_gui_socket(socket, callback)
  local real_getenv = os.getenv
  os.getenv = function(name)
    if name == "WEZTERM_UNIX_SOCKET" then return socket end
    return real_getenv(name)
  end
  local ok, failure = pcall(callback)
  os.getenv = real_getenv
  assert(ok, failure)
end

test("a window drawn before its source is answered is published once, under the source", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  wezterm.run_child_process = function(args) return true, tab_source_response(args[4]), "" end
  local tab = gui_tab({window_id=9793,tab_id=9794,tab_index=0,panes={9795}})
  local file = test_dir .. "/tabs/" .. string.rep("a",64) .. "-9793.json"
  local before_draw = math.floor(os.time() * 1000)
  with_gui_socket("/test/gui.sock", function()
    format_tab_title(tab, {tab})
    assert(not path_exists(tab_publication_path(9793)) and not path_exists(file),
      "nothing is written while the source is still to come")
    internal.acquire_tab_source("/test/gui.sock")
  end)
  local publication = decode_json(assert(read_path(file), "the held draw is published"))
  assert(publication.schema == 2 and publication.source.socket_path == "/test/gui.sock")
  assert(publication.published_at_ms >= before_draw and publication.published_at_ms <= os.time() * 1000,
    "the file says when the bar drew it")
  assert(not path_exists(tab_publication_path(9793)), "the unsourced name was never written")
  local foreign = test_dir .. "/tabs/" .. string.rep("b",64) .. "-9793.json"
  local out = assert(io.open(foreign,"w")); out:write("foreign"); out:close()
  local polling = window_double({window_id=9799,tabs={},focused=false})
  attention.poll(polling, {gui_windows={polling}})
  assert(not path_exists(file), "owned source-specific file is withdrawn")
  assert(read_path(foreign) == "foreign", "another source's equal window id is untouched")
  os.remove(foreign)
  wezterm.run_child_process = previous
  internal.reset_tab_source()
end)

test("a window drawn while a failed source run waits for its retry is published under the source", function()
  local previous, real_time = wezterm.run_child_process, os.time
  internal.reset_tab_source()
  local calls = 0
  wezterm.run_child_process = function(args)
    calls = calls + 1
    if calls == 1 then return false, "", "unused failure text" end
    return true, tab_source_response(args[4]), ""
  end
  local tab = gui_tab({window_id=9837,tab_id=9838,tab_index=0,panes={9839}})
  local sourced = test_dir .. "/tabs/" .. string.rep("a",64) .. "-9837.json"
  local ok, failure = pcall(with_gui_socket, "/test/retried.sock", function()
    format_tab_title(tab, {tab})
    internal.acquire_tab_source("/test/retried.sock")
    format_tab_title(tab, {tab})
    assert(not path_exists(tab_publication_path(9837)), "held while the retry waits")
    os.time = function() return real_time() + 60 end
    internal.acquire_tab_source("/test/retried.sock")
  end)
  os.time = real_time
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  assert(ok, failure)
  assert(calls == 2, "the retry ran, got " .. calls)
  local publication = decode_json(assert(read_path(sourced), "the held draw is published under the source"))
  assert(publication.schema == 2 and publication.source.socket_path == "/test/retried.sock")
  assert(not path_exists(tab_publication_path(9837)), "the unsourced name was never written")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("wait for a retry", 1, true),
    "the failure is logged once, got " .. #errors)
  os.remove(sourced)
end)

test("a held tab order is published without a source once every retry has failed", function()
  local previous, real_time = wezterm.run_child_process, os.time
  internal.reset_tab_source()
  local answer = false
  local calls = 0
  wezterm.run_child_process = function(args)
    calls = calls + 1
    if answer then return true, tab_source_response(args[4]), "" end
    return false, "", "unused failure text"
  end
  local tab = gui_tab({window_id=9843,tab_id=9844,tab_index=0,panes={9845}})
  local later = gui_tab({window_id=9846,tab_id=9847,tab_index=0,panes={9848}})
  local sourced_later = test_dir .. "/tabs/" .. string.rep("a",64) .. "-9846.json"
  local ok, failure = pcall(with_gui_socket, "/test/never-answers.sock", function()
    format_tab_title(tab, {tab})
    -- The first run, then one retry after each of the 2, 5, 10 and 30 s waits.
    for run = 1, 5 do
      assert(not path_exists(tab_publication_path(9843)), "held before run " .. run)
      os.time = function() return real_time() + run * 60 end
      internal.acquire_tab_source("/test/never-answers.sock")
    end
    assert(calls == 5, "every retry ran, got " .. calls)
    local publication = assert(read_tab_publication(9843), "the held draw is published once no retry is left")
    assert(publication.schema == 1 and publication.source == nil)
    -- A later retry may still answer; a window drawn after it is published under the source.
    answer = true
    os.time = function() return real_time() + 600 end
    internal.acquire_tab_source("/test/never-answers.sock")
    assert(internal.tab_source(), "a later retry still answers")
    format_tab_title(later, {later})
  end)
  os.time = real_time
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  assert(ok, failure)
  assert(path_exists(sourced_later), "a window first drawn after the answer is published under the source")
  local errors = drain_errors()
  assert(#errors == 2 and errors[2]:find("without source identity", 1, true),
    "the unsourced publication is logged once, got " .. #errors)
  os.remove(tab_publication_path(9843))
  os.remove(sourced_later)
end)

test("a held tab order is published without a source once no answer can come", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  wezterm.run_child_process = function() error("the tab source is not asked for in this case") end
  local tab = gui_tab({window_id=9840,tab_id=9841,tab_index=0,panes={9842}})
  local ok, failure = pcall(with_gui_socket, "/test/unanswered.sock", function()
    format_tab_title(tab, {tab})
    assert(not path_exists(tab_publication_path(9840)), "held while the answer is to come")
    -- Nothing can run the attention command any more, so no answer can come.
    wezterm.run_child_process = nil
    internal.acquire_tab_source("/test/unanswered.sock")
  end)
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  assert(ok, failure)
  local publication = assert(read_tab_publication(9840), "the held draw is published")
  assert(publication.schema == 1 and publication.source == nil)
  os.remove(tab_publication_path(9840))
end)

test("a legacy tab order this process never wrote survives its window's sourced one", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  local foreign = tab_publication_path(9796)
  local out = assert(io.open(foreign, "w")); out:write("from an exited GUI"); out:close()
  wezterm.run_child_process = function(args) return true, tab_source_response(args[4]), "" end
  internal.acquire_tab_source("/test/gui.sock")
  local drawn = gui_tab({ window_id = 9796, tab_id = 9797, tab_index = 0, panes = { 9798 } })
  format_tab_title(drawn, { drawn })
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  assert(path_exists(test_dir .. "/tabs/" .. string.rep("a", 64) .. "-9796.json"))
  assert(read_path(foreign) == "from an exited GUI", "a file this process did not write is sweep's")
  os.remove(foreign)
end)

test("a window first published without a source keeps that one file after the source arrives", function()
  local previous = wezterm.run_child_process
  internal.reset_tab_source()
  local drawn = gui_tab({ window_id = 9830, tab_id = 9831, tab_index = 0, panes = { 9832 } })
  format_tab_title(drawn, { drawn })
  local legacy = tab_publication_path(9830)
  assert(path_exists(legacy), "with no source to wait for, the first draw publishes unsourced")
  wezterm.run_child_process = function(args) return true, tab_source_response(args[4]), "" end
  internal.acquire_tab_source("/test/gui.sock")
  write_activity(9832, "stop")
  poll({ 9832 })
  format_tab_title(drawn, { drawn })
  local later = gui_tab({ window_id = 9829, tab_id = 9828, tab_index = 0, panes = { 9827 } })
  format_tab_title(later, { later })
  wezterm.run_child_process = previous
  internal.reset_tab_source()
  local sourced = test_dir .. "/tabs/" .. string.rep("a", 64) .. "-9830.json"
  assert(not path_exists(sourced), "the window is not given a second file")
  local publication = assert(read_tab_publication(9830))
  assert(publication.schema == 1 and publication.tabs[1].text:find("✓ ", 1, true),
    "the changed bar is written to the window's one file")
  assert(path_exists(test_dir .. "/tabs/" .. string.rep("a", 64) .. "-9829.json"),
    "a window first drawn after the answer is published under the source")
  os.remove(legacy)
  os.remove(test_dir .. "/tabs/" .. string.rep("a", 64) .. "-9829.json")
  os.remove(test_dir .. "/9832")
end)

test("a config reload moves a window to its sourced tab order and removes its own unsourced one", function()
  local previous = wezterm.run_child_process
  -- Before the writer is installed the plugin has no source to wait for, so
  -- its first draws publish under the unsourced name every GUI shares.
  local bare_root = test_dir .. "/root-before-install"
  assert(os.execute("mkdir -p " .. shell_quote(bare_root)) == 0)
  local before = dofile(repo_root .. "/plugin/init.lua")
  before.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = bare_root })
  local draw_before = handlers["format-tab-title"][#handlers["format-tab-title"]]
  drain_warnings()
  local own = gui_tab({ window_id = 9870, tab_id = 9871, tab_index = 0, panes = { 9872 } })
  local rewritten = gui_tab({ window_id = 9873, tab_id = 9874, tab_index = 0, panes = { 9875 } })
  draw_before(own, { own })
  draw_before(rewritten, { rewritten })
  assert(path_exists(tab_publication_path(9870)) and path_exists(tab_publication_path(9873)),
    "precondition: both windows are published unsourced")
  -- Another GUI's window with the same id takes the shared name meanwhile.
  local out = assert(io.open(tab_publication_path(9873), "w"))
  out:write("another GUI's window 9873"); out:close()

  wezterm.run_child_process = function(args) return true, tab_source_response(args[4]), "" end
  local ok, failure = pcall(function()
    local after = dofile(repo_root .. "/plugin/init.lua")
    after.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
      integration_root = writer_root })
    local draw_after = handlers["format-tab-title"][#handlers["format-tab-title"]]
    after._internal.acquire_tab_source("/test/gui.sock")
    draw_after(own, { own })
    draw_after(rewritten, { rewritten })
  end)
  wezterm.run_child_process = previous
  assert(ok, failure)
  local sourced = function(window_id)
    return test_dir .. "/tabs/" .. string.rep("a", 64) .. "-" .. window_id .. ".json"
  end
  assert(path_exists(sourced(9870)) and path_exists(sourced(9873)),
    "after the reload both windows are published under the source")
  assert(not path_exists(tab_publication_path(9870)),
    "the unsourced file this GUI wrote before the reload is removed")
  assert(read_path(tab_publication_path(9873)) == "another GUI's window 9873",
    "a file another GUI rewrote is not this GUI's to remove")
  for _, path in ipairs({ sourced(9870), sourced(9873), tab_publication_path(9873) }) do
    os.remove(path)
  end
end)

--- Every file under tabs/ that describes `window_id` with `pane_id` as its
--- first tab's first marker id: what `attention tabs` would list for that
--- window of the GUI that drew it.
local function tab_orders_naming(window_id, pane_id)
  local found = {}
  for _, path in ipairs(wezterm.glob(test_dir .. "/tabs/*.json")) do
    local ok, publication = pcall(decode_json, read_path(path) or "")
    if ok and type(publication) == "table" and publication.window_id == window_id
        and publication.tabs[1] and publication.tabs[1].marker_ids[1] == tostring(pane_id) then
      found[#found + 1] = path
    end
  end
  return found
end

test("two GUIs drawing the same window id never remove each other's tab order", function()
  local previous_run, real_getenv, real_remove = wezterm.run_child_process, os.getenv, os.remove
  -- A second GUI process with no source identity, so its windows publish
  -- under the unsourced name every GUI shares: window ids restart in each.
  local other_root = test_dir .. "/root-without-writer"
  assert(os.execute("mkdir -p " .. shell_quote(other_root)) == 0)
  local other = dofile(repo_root .. "/plugin/init.lua")
  local other_handler = #handlers["format-tab-title"] + 1
  other.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = other_root })
  local other_format = assert(handlers["format-tab-title"][other_handler])
  drain_warnings()
  -- A pane is published under the key a poll found it by.
  poll({ 9862 })
  other.poll(window_double({ tabs = { { 9863 } }, focused = false }))

  internal.reset_tab_source()
  os.getenv = function(name)
    if name == "WEZTERM_UNIX_SOCKET" then return "/test/gui.sock" end
    return real_getenv(name)
  end
  wezterm.run_child_process = function(args) return true, tab_source_response(args[4]), "" end
  local mine = gui_tab({ window_id = 9860, tab_id = 9861, tab_index = 0, panes = { 9862 } })
  local theirs = gui_tab({ window_id = 9860, tab_id = 9861, tab_index = 0, panes = { 9863 } })
  local shared = tab_publication_path(9860)
  local sourced = test_dir .. "/tabs/" .. string.rep("a", 64) .. "-9860.json"
  -- The other GUI renames its draw into the shared name at the worst moment:
  -- after this process has read the file and before it removes it.
  local other_drew = false
  os.remove = function(target)
    if target == shared and not other_drew then
      other_drew = true
      other_format(theirs, { theirs })
    end
    return real_remove(target)
  end
  local ok, failure = pcall(function()
    format_tab_title(mine, { mine })
    internal.acquire_tab_source("/test/gui.sock")
    format_tab_title(mine, { mine })
    if not other_drew then
      other_drew = true
      other_format(theirs, { theirs })
    end
  end)
  os.remove, os.getenv, wezterm.run_child_process = real_remove, real_getenv, previous_run
  internal.reset_tab_source()
  assert(ok, failure)
  local their_order = read_path(shared)
  assert(their_order and decode_json(their_order).tabs[1].marker_ids[1] == "9863",
    "the other GUI's tab order must survive this one's draws")
  assert(path_exists(sourced), "this GUI's window is published under its source")
  local listed = tab_orders_naming(9860, 9862)
  assert(#listed == 1 and listed[1] == sourced,
    "this GUI's window must be listed once, got " .. #listed)
  os.remove(shared)
  os.remove(sourced)
end)

test("a closed window's tab order another process rewrote is not withdrawn", function()
  internal.reset_tab_source()
  local drawn = gui_tab({ window_id = 9833, tab_id = 9834, tab_index = 0, panes = { 9835 } })
  format_tab_title(drawn, { drawn })
  local legacy = tab_publication_path(9833)
  assert(path_exists(legacy), "the draw publishes")
  local out = assert(io.open(legacy, "w")); out:write("another GUI's window 9833"); out:close()
  local polling = window_double({ window_id = 9836, tabs = {}, focused = false })
  attention.poll(polling, { gui_windows = { polling } })
  assert(read_path(legacy) == "another GUI's window 9833",
    "a file whose bytes are not the ones this process wrote is not this process's to remove")
  os.remove(legacy)
end)

test("a window publishes its drawn order once every one of its tabs is drawn", function()
  write_activity(9820, "stop")
  -- 9821 has no records: a pane no launch has claimed is published by its id.
  poll({ 9820, 9821 })

  local first = gui_tab({ window_id = 9800, tab_id = 9810, tab_index = 0, panes = { 9820 } })
  local second = gui_tab({ window_id = 9800, tab_id = 9811, tab_index = 1, panes = { 9821 } })
  local bar = { first, second }

  local first_drawn = format_tab_title(first, bar)
  assert(not path_exists(tab_publication_path(9800)),
    "a window with a tab still undrawn publishes nothing")

  local second_drawn = format_tab_title(second, bar)
  local published = assert(read_tab_publication(9800), "the drawn window should be published")

  assert(published.schema == 1, "the file carries its own schema")
  assert(published.window_id == 9800, "the file names the window it describes")
  assert(type(published.published_at_ms) == "number",
    "the file says when it was written")
  assert(#published.tabs == 2, "both tabs should be there, got " .. tostring(#published.tabs))
  assert(published.tabs[1].number == 1 and published.tabs[2].number == 2,
    "the numbers are the ones the bar draws")
  assert(published.tabs[1].text == rendered_text(first_drawn)
      and published.tabs[2].text == rendered_text(second_drawn),
    "the text is what those calls drew")
  assert(published.tabs[1].marker_ids[1] == seeded_key(9820)
      and published.tabs[2].marker_ids[1] == "9821",
    "each tab carries the keys its panes are cached under")

  -- Byte for byte, because the reader on the other side wants integers and
  -- this is where the plugin's own encoding is decided.
  assert(read_path(tab_publication_path(9800)) == string.format(
    '{"published_at_ms":%d,"schema":1,"tabs":['
      .. '{"marker_ids":["' .. seeded_key(9820) .. '"],"number":1,"text":%s},'
      .. '{"marker_ids":["9821"],"number":2,"text":%s}],"window_id":9800}\n',
    published.published_at_ms,
    encode_json_string(published.tabs[1].text),
    encode_json_string(published.tabs[2].text)),
    "the published bytes are sorted keys and unquoted integers")
end)

test("a redraw that draws the same thing writes no file", function()
  write_activity(9822, "stop")
  poll({ 9822 })

  local first = gui_tab({ window_id = 9801, tab_id = 9812, tab_index = 0, panes = { 9822 } })
  local second = gui_tab({ window_id = 9801, tab_id = 9813, tab_index = 1, panes = { 9823 } })
  local bar = { first, second }
  format_tab_title(first, bar)
  format_tab_title(second, bar)
  assert(path_exists(tab_publication_path(9801)), "the first draw publishes")

  assert(os.remove(tab_publication_path(9801)), "the published file should be removable")
  format_tab_title(first, bar)
  format_tab_title(second, bar)
  assert(not path_exists(tab_publication_path(9801)),
    "an unchanged bar must not write on the GUI thread")

  write_activity(9823, "notify")
  poll({ 9822, 9823 })
  format_tab_title(first, bar)
  local changed_drawn = format_tab_title(second, bar)
  local published = assert(read_tab_publication(9801),
    "a changed bar publishes the whole window again")
  assert(published.tabs[2].text == rendered_text(changed_drawn),
    "the republished text is the changed one")
  assert(published.tabs[2].text:find("! ", 1, true),
    "the changed tab draws the notify glyph")
end)

test("the published ids are the translated ones, not the window's local ids", function()
  write_activity(9840, "stop")
  attention.poll(window_double({
    tabs = { { { id = 9830, published = 9840, domain = "mux" } } }, focused = false }))

  local only = gui_tab({ window_id = 9802, tab_id = 9814, tab_index = 0, panes = { 9830 } })
  format_tab_title(only, { only })

  local published = assert(read_tab_publication(9802), "the window should be published")
  assert(published.tabs[1].marker_ids[1] == "9840",
    "a mux client's tab carries the published pane id, got "
      .. tostring(published.tabs[1].marker_ids[1]))
end)

test("a mux-client pane drawn before any poll shows no other pane's attention", function()
  write_activity(9746, "stop")
  -- A client pane that published 9746 as its marker id, walked by polls long
  -- enough for its title to settle.
  for _ = 1, 2 do
    attention.poll(window_double({ tabs = { { { id = 9747, published = 9746, domain = "unix",
      title = "server-job" } } }, focused = false }))
  end
  -- Another client pane whose GUI-local number is 9746, not polled yet.
  window_double({ window_id = 9748, tabs = { { { id = 9746, domain = "unix" } } }, focused = false })
  local drawn = gui_tab({ window_id = 9748, tab_id = 9749, tab_index = 0, panes = { 9746 } })
  local rendered = format_tab_title(drawn, { drawn })
  assert(not rendered_text(rendered):find("✓", 1, true),
    "the local number names another pane's markers, got " .. rendered_text(rendered))
  assert(not rendered_text(rendered):find("server-job", 1, true),
    "nor the other pane's settled title, got " .. rendered_text(rendered))
  local published = assert(read_tab_publication(9748))
  assert(#published.tabs[1].marker_ids == 0, "and is not published as this tab's")

  os.remove(test_dir .. "/9746")
end)

test("two windows publish their own orders into their own files", function()
  local left = gui_tab({ window_id = 9803, tab_id = 9815, tab_index = 0, panes = { 9824 } })
  local right_first = gui_tab({ window_id = 9804, tab_id = 9816, tab_index = 0, panes = { 9825 } })
  local right_second = gui_tab({ window_id = 9804, tab_id = 9817, tab_index = 1, panes = { 9826 } })
  poll({ 9824, 9825, 9826 })

  format_tab_title(left, { left })
  format_tab_title(right_first, { right_first, right_second })
  assert(not path_exists(tab_publication_path(9804)),
    "one window's complete bar does not complete another's")
  format_tab_title(right_second, { right_first, right_second })

  local one = assert(read_tab_publication(9803), "the one-tab window should be published")
  local two = assert(read_tab_publication(9804), "the two-tab window should be published")
  assert(#one.tabs == 1 and #two.tabs == 2,
    "each file holds only the tabs of its own window")
  assert(one.tabs[1].marker_ids[1] == "9824" and two.tabs[1].marker_ids[1] == "9825",
    "each file holds its own window's panes")
end)

--- Publish one window's order the way the bar does, then poll from another
--- window with the given GUI-window inventory, which is how a closed window is
--- noticed: its own bar never redraws.
local function publish_window(window_id, tab_id, pane_id)
  local tab = gui_tab({ window_id = window_id, tab_id = tab_id, tab_index = 0, panes = { pane_id } })
  format_tab_title(tab, { tab })
  assert(path_exists(tab_publication_path(window_id)), "the window should be published")
end

local function poll_with_inventory(polling_window_id, live_window_ids)
  -- A window that closed is gone from the mux as well as from the GUI.
  for id in pairs(mux_windows_by_id) do mux_windows_by_id[id] = nil end
  local live = {}
  for index, id in ipairs(live_window_ids) do
    live[index] = window_double({ window_id = id, tabs = {}, focused = false })
  end
  local polling = window_double({ window_id = polling_window_id, tabs = { { 9899 } }, focused = false })
  attention.poll(polling, { gui_windows = function() return live end })
end

test("a window that has closed has its tab order withdrawn by a surviving window's poll", function()
  publish_window(9805, 9818, 9827)
  publish_window(9806, 9819, 9828)

  poll_with_inventory(9806, { 9805, 9806 })
  assert(path_exists(tab_publication_path(9805)) and path_exists(tab_publication_path(9806)),
    "a live window keeps its tab order")

  poll_with_inventory(9806, { 9806 })
  assert(not path_exists(tab_publication_path(9805)), "the closed window's file is withdrawn")
  assert(path_exists(tab_publication_path(9806)), "the polling window's own file stays")

  poll_with_inventory(9806, { 9806 })
  assert(not path_exists(tab_publication_path(9805)), "a second poll finds nothing to do")
end)

test("a tab order this process did not write is left for sweep", function()
  local foreign = tab_publication_path(9807)
  local file = assert(io.open(foreign, "w"))
  file:write('{"published_at_ms":1,"schema":1,"tabs":[],"window_id":9807}\n')
  file:close()

  poll_with_inventory(9806, { 9806 })
  assert(path_exists(foreign), "another process's file is not this process's to withdraw")
  os.remove(foreign)
end)

test("an unreadable window inventory withdraws nothing", function()
  publish_window(9808, 9820, 9829)
  local polling = window_double({ window_id = 9806, tabs = { { 9899 } }, focused = false })
  attention.poll(polling, { gui_windows = function() error("inventory unavailable") end })
  assert(path_exists(tab_publication_path(9808)),
    "no inventory means no window is known to be closed")
  poll_with_inventory(9806, { 9806 })
  assert(not path_exists(tab_publication_path(9808)), "the next readable inventory withdraws it")
end)

test("a window id reused after withdrawal publishes again", function()
  publish_window(9809, 9821, 9830)
  poll_with_inventory(9806, { 9806 })
  assert(not path_exists(tab_publication_path(9809)), "withdrawn")

  publish_window(9809, 9821, 9830)
  assert(path_exists(tab_publication_path(9809)),
    "the same composed list must be written again once its file was withdrawn")
  poll_with_inventory(9806, { 9806 })
end)

test("a claimed pane publishes its cache key, not the local pane id", function()
  materialize_state_case(protocol_fixture.state_case)
  attention.poll(window_double({ tabs = { { {
    id = 9850, domain = "unix", attention = protocol_fixture.wire_sample,
  } } }, focused = false }), {
    now_unix_ns = protocol_fixture.state_case.now_unix_ns,
    call_after = function() end,
  })

  local only = gui_tab({ window_id = 9805, tab_id = 9818, tab_index = 0, panes = { 9850 } })
  format_tab_title(only, { only })

  local published = assert(read_tab_publication(9805), "the window should be published")
  local key = internal.address_cache_key(protocol_fixture.wire_sample.address)
  assert(published.tabs[1].marker_ids[1] == key,
    "a claimed pane publishes the cache key the plugin indexes it by, got "
      .. tostring(published.tabs[1].marker_ids[1]))
end)

-- ── Focus-aware acknowledgement ─────────────────────────────────────────────

--- What the attention command answers to `plugin acknowledge` for a seeded
--- pane: it writes the acknowledgement only while the event named is still
--- the pane's activity, as the command does under its locks.
local function acknowledging_answer(argv)
  local arguments = assert(plugin_arguments(argv), "not an attention command line")
  local function flag(name) return assert(arguments:match("%-%-" .. name .. " (%S+)"), name) end
  local address = {
    realm_id = flag("realm%-id"), incarnation_id = flag("incarnation%-id"), pane_id = flag("pane%-id"),
  }
  local launch_id, event_id = flag("launch%-id"), flag("activity%-event%-id")
  local launch_root = test_dir .. "/v2/realms/" .. address.realm_id .. "/incarnations/"
    .. address.incarnation_id .. "/panes/" .. address.pane_id .. "/launches/" .. launch_id
  local pointer = read_path(launch_root .. "/current-binding.json")
  local records_root = launch_root .. "/bindings/" .. decode_json(pointer).binding_id
  local current = read_path(records_root .. "/activity.json")
  local disposition = "ignored"
  if current and decode_json(current).event_id == event_id then
    local ack = copy_json(protocol_fixture.record_samples.acknowledgement)
    ack.address, ack.launch_id = address, launch_id
    ack.target = decode_json(current).target
    ack.activity_event_id, ack.event_id = event_id, next_event_id()
    write_json_path(records_root .. "/ack.json", ack)
    disposition = "applied"
  end
  return true, '{"schema":1,"command":"plugin acknowledge","status":"ok","complete":true,'
    .. '"result":{"disposition":"' .. disposition .. '","diagnostic":null,"event_id":null},'
    .. '"diagnostics":[]}'
end

test("a focused poll acknowledges only the active pane", function()
  local event_id = write_activity(801, "notify")
  write_activity(802, "stop")

  local w
  local spawned = with_plugin_command(acknowledging_answer, function()
    w = poll_focused({ tabs = { { 801, 802 } }, active_pane_id = 801 })
  end)

  assert(#spawned == 1, "one acknowledgement runs, got " .. #spawned)
  local wire = seeded_wire(801)
  assert(spawned[1][1] == "env" and spawned[1][2] == "WEZTERM_ATTENTION_DIR=" .. test_dir,
    "the state directory is handed to the command")
  assert(plugin_arguments(spawned[1]) == table.concat({
    "plugin", "acknowledge",
    "--realm-id", wire.address.realm_id,
    "--incarnation-id", wire.address.incarnation_id,
    "--pane-id", "801",
    "--launch-id", wire.launch_id,
    "--activity-event-id", event_id,
  }, " "), "the command names the pane, its launch and the event shown: "
    .. tostring(plugin_arguments(spawned[1])))
  assert(activity_exists(801), "acknowledgement must never remove the activity")
  assert(attention.get_attention(801) == nil, "the acknowledged activity is gone from the tab at once")
  assert(attention.get_attention(802) == "stop", "the sibling cache entry should remain")
  assert(#w.status_writes == 0 and #w.title_writes == 0,
    "the plugin must not write status or title text")
end)

test("an unfocused poll acknowledges nothing and performs no action", function()
  write_activity(811, "notify")
  write_activity(812, "stop")

  local w = window_double({ tabs = { { 811, 812 } }, focused = false, active_pane_id = 811 })
  local spawned = with_plugin_command(acknowledging_answer, function()
    attention.poll(w, { now_unix_ns = fixture_now })
  end)

  assert(#spawned == 0, "a background window must not acknowledge its active pane")
  assert(attention.get_attention(811) == "notify", "the cache should still be filled")
  assert(#w.actions == 0, "an unfocused window must never be sent a key action")
end)

test("visiting the sibling pane acknowledges its activity", function()
  write_activity(401, "stop")
  local sibling_event = write_activity(402, "notify")

  with_plugin_command(acknowledging_answer, function()
    poll_focused({ tabs = { { 401, 402 } }, active_pane_id = 401 })
  end)
  assert(not acknowledgement_exists(402), "the unvisited sibling is not acknowledged")

  local spawned = with_plugin_command(acknowledging_answer, function()
    poll_focused({ tabs = { { 401, 402 } }, active_pane_id = 402 })
  end)
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find("--activity-event-id " .. sibling_event, 1, true),
    "the visited sibling's activity is acknowledged")
  assert(attention.get_attention(402) == nil, "the acknowledged sibling should disappear from effective cache")
end)

test("a new activity is shown again after an older one was acknowledged", function()
  write_activity(931, "notify")
  with_plugin_command(acknowledging_answer, function()
    poll_focused({ tabs = { { 930, 931 } }, active_pane_id = 931 })
  end)
  assert(attention.get_attention(931) == nil, "the first activity should be acknowledged")

  write_activity(931, "notify")
  poll({ 930, 931 })

  assert(attention.get_attention(931) == "notify", "the second activity must be visible")
end)

test("an acknowledgement survives a plugin reload", function()
  write_activity(947, "notify")
  with_plugin_command(acknowledging_answer, function()
    poll_focused({ tabs = { { 940, 947 } }, active_pane_id = 947 })
  end)
  assert(acknowledgement_exists(947), "precondition: acknowledged")

  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false })
  reloaded.poll(window_double({ tabs = { { 940, 947 } }, focused = false }),
    { now_unix_ns = fixture_now })

  assert(activity_exists(947), "reload must leave the activity untouched")
  assert(reloaded.get_attention(947) == nil, "reload should honor the durable acknowledgement")
end)

--- The answer of a `plugin acknowledge` that failed with diagnostic `code`.
local function acknowledgement_failed(code, message)
  return false, '{"schema":1,"command":"plugin acknowledge","status":"unavailable",'
    .. '"complete":false,"result":{},"diagnostics":[{"code":"' .. code .. '",'
    .. '"message":"' .. message .. '","context":{},"help":""}]}'
end

test("a refused acknowledgement leaves the activity visible, says why once, and is not retried", function()
  write_activity(943, "notify")
  local function refused()
    return acknowledgement_failed("claim_stale",
      "the pane's claim does not name the launch the pane published")
  end
  local real_time = os.time
  local spawned
  local ok, failure = pcall(function()
    spawned = with_plugin_command(refused, function()
      poll_focused({ tabs = { { 943 } }, active_pane_id = 943 })
      poll_focused({ tabs = { { 943 } }, active_pane_id = 943 })
      -- Past every backoff wait: a refusal stands for this event.
      os.time = function() return real_time() + 600 end
      poll_focused({ tabs = { { 943 } }, active_pane_id = 943 })
    end)
  end)
  os.time = real_time
  assert(ok, failure)

  assert(#spawned == 1, "the same activity is not tried again on every poll, got " .. #spawned)
  assert(attention.get_attention(943) == "notify", "the activity must remain visible")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("claim_stale", 1, true),
    "the failure is reported once with the command's reason, got " .. tostring(errors[1]))

  -- A newer activity is a new thing to acknowledge.
  write_activity(943, "stop")
  spawned = with_plugin_command(acknowledging_answer, function()
    poll_focused({ tabs = { { 943 } }, active_pane_id = 943 })
  end)
  assert(#spawned == 1 and attention.get_attention(943) == nil, "the newer activity is acknowledged")
end)

test("an acknowledgement that failed in a way that can pass is tried again after a wait", function()
  local failures = {
    [9431] = function() return acknowledgement_failed("probe_unavailable", "state lock timed out") end,
    [9432] = function() error("spawn failed") end,
    [9433] = function() return false, "" end,
  }
  local real_time = os.time
  for pane_id, fail in pairs(failures) do
    write_activity(pane_id, "notify")
    local runs = 0
    local function failing_once(argv)
      runs = runs + 1
      if runs == 1 then return fail(argv) end
      return acknowledging_answer(argv)
    end
    local ok, failure = pcall(function()
      with_plugin_command(failing_once, function()
        poll_focused({ tabs = { { pane_id } }, active_pane_id = pane_id })
        poll_focused({ tabs = { { pane_id } }, active_pane_id = pane_id })
        assert(runs == 1, "pane " .. pane_id .. ": the retry waits for its backoff, ran " .. runs)
        os.time = function() return real_time() + 60 end
        poll_focused({ tabs = { { pane_id } }, active_pane_id = pane_id })
      end)
    end)
    os.time = real_time
    assert(ok, failure)
    assert(runs == 2, "pane " .. pane_id .. ": the failed run is tried once more, ran " .. runs)
    assert(attention.get_attention(pane_id) == nil, "pane " .. pane_id .. ": the retry acknowledged it")
    local errors = drain_errors()
    assert(#errors == 1, "pane " .. pane_id .. ": the failure is reported once, got " .. #errors)
  end
end)

-- The reader and the command can disagree about which activity is shown. An
-- answer is an answer: asking again on every poll would change nothing.
test("an acknowledgement the command answered is not asked again for that event", function()
  write_activity(9434, "notify")
  local function ignoring()
    return true, '{"schema":1,"command":"plugin acknowledge","status":"ok","complete":true,'
      .. '"result":{"disposition":"ignored","diagnostic":null,"event_id":null},"diagnostics":[]}'
  end
  local spawned = with_plugin_command(ignoring, function()
    for _ = 1, 3 do poll_focused({ tabs = { { 9434 } }, active_pane_id = 9434 }) end
  end)
  assert(#spawned == 1, "one run for the event, got " .. #spawned)
  assert(attention.get_attention(9434) == "notify", "an ignored acknowledgement hides nothing")
end)

test("a command that answers nothing is said to predate the plugin", function()
  write_activity(9435, "notify")
  with_plugin_command(function() return false, "" end, function()
    poll_focused({ tabs = { { 9435 } }, active_pane_id = 9435 })
  end)
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("may predate this plugin", 1, true)
      and errors[1]:find("scripts/install-cli.sh", 1, true)
      and errors[1]:find(writer_root, 1, true),
    "the log names the likely cause and the way out, got " .. tostring(errors[1]))
end)

test("a pane that vanishes between polls leaves the cache and keeps its records", function()
  seed_pane(940)
  write_activity(945, "notify")
  local window_id = 9450
  attention.poll(window_double({
    tabs = { { 940, 945 } }, focused = false, window_id = window_id,
  }), { now_unix_ns = fixture_now })
  assert(attention.get_attention(945) == "notify", "precondition: shown")

  -- 945 is gone; 940 is still here, so its domain is still represented and the
  -- disappearance reads as a closed pane rather than a detached domain.
  attention.poll(window_double({
    tabs = { { 940 } }, focused = false, window_id = window_id,
  }), { now_unix_ns = fixture_now })

  assert(activity_exists(945), "the records are the writer's, and stay")
  assert(attention.get_attention(945) == nil, "a closed pane should leave the cache")
end)

test("a stale update-status pane is never acknowledgement authority", function()
  write_activity(951, "notify")
  local w = window_double({ tabs = { { 951, 952 } }, focused = true, active_pane_id = 952 })

  local spawned = with_plugin_command(acknowledging_answer, function()
    attention.poll(w, { active_pane = pane_from_entry(951), now_unix_ns = fixture_now })
  end)

  assert(#spawned == 0, "the stale event pane must not be acknowledged")
  assert(attention.get_attention(951) == "notify", "the unseen notification must remain visible")
end)

test("an activity replaced before its acknowledgement is not acknowledged unseen", function()
  for _, replacement in ipairs({ "thinking", "notify" }) do
    local shown = write_activity(501, "stop")
    write_activity(502, "notify")
    local rewrote = false
    local spawned = with_plugin_command(acknowledging_answer, function()
      poll_focused({
        tabs = { { 501, 502 } },
        active_pane_id = 501,
        on_focus_check = function()
          if rewrote then return end
          rewrote = true
          -- The poll has already read the stop the user is looking at.
          write_activity(501, replacement)
        end,
      })
    end)

    assert(#spawned == 1 and plugin_arguments(spawned[1]):find("--activity-event-id " .. shown, 1, true),
      "what is acknowledged is the activity this poll showed")
    assert(not acknowledgement_exists(501), "the replacement must not be acknowledged unseen")
    assert(attention.get_attention(501) == replacement, "the tab takes the replacement at once")
  end
end)

test("a focused window with no active pane acknowledges nothing", function()
  write_activity(821, "notify")

  local w
  local spawned = with_plugin_command(acknowledging_answer, function()
    w = poll_focused({ tabs = { { 821 } }, active_pane_id = nil })
  end)

  assert(#spawned == 0, "with no active pane there is nothing to acknowledge")
  assert(attention.get_attention(821) == "notify", "the cache should still be filled")
  assert(#w.actions == 0, "there is no pane to perform an action through")
end)

-- ── Focus-safe redraw ───────────────────────────────────────────────────────

test("a focused visible change requests exactly one redraw through the active pane", function()
  write_activity(831, "thinking")

  local w = poll_focused({ tabs = { { 830, 831 } }, active_pane_id = 830 })

  assert(#w.actions == 1, "one visible change should cost one action, got " .. #w.actions)
  assert(w.actions[1].action.ActivateTabRelative == 0,
    "the action must re-activate the current tab, changing no selection")
  assert(w.actions[1].pane_id == 830, "the action must run through the active pane")
  assert(#w.status_writes == 0 and #w.title_writes == 0,
    "redrawing must not write status or title text")
end)

test("an unchanged tab bar requests no redraw", function()
  write_activity(841, "thinking")
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
  write_activity(851, "notify")
  write_activity(852, "notify")
  poll({ 850, 851, 852 })

  -- 852 falls from notify to stop; 851 still shows notify, so the tab does not
  -- change. 850 is the active pane and carries no marker, so nothing is
  -- acknowledged either.
  write_activity(852, "stop")
  local w = poll_focused({ tabs = { { 850, 851, 852 } }, active_pane_id = 850 })

  assert(#w.actions == 1, "a cache change should request one redraw, got " .. #w.actions)
  assert(attention.get_attention(852) == "stop", "the cache should still have followed the marker")
end)

test("a marker disappearing requests a redraw", function()
  write_activity(861, "notify")
  poll({ 860, 861 })

  clear_activity(861)
  local w = poll_focused({ tabs = { { 860, 861 } }, active_pane_id = 860 })

  assert(attention.get_attention(861) == nil, "the cache should drop the removed marker")
  assert(#w.actions == 1, "attention vanishing is a visible change, got " .. #w.actions)
end)

test("animation redraws once per wall-clock bucket, not once per poll", function()
  write_activity(871, "thinking")

  local w = window_double({ tabs = { { 870, 871 } }, focused = true, active_pane_id = 870 })
  attention.poll(w, { now_ms = 1000, now_unix_ns = fixture_now })
  assert(#w.actions == 1, "the marker appearing is the first visible change")

  attention.poll(w, { now_ms = 1000, now_unix_ns = fixture_now })
  attention.poll(w, { now_ms = 1999, now_unix_ns = fixture_now })
  assert(#w.actions == 1, "induced polls in one bucket must not redraw again")

  attention.poll(w, { now_ms = 2000, now_unix_ns = fixture_now })
  assert(#w.actions == 2, "the next bucket is a new indicator, so one new redraw")
  assert(select(2, attention.get_attention(871)) == 2, "the frame should come from the new bucket")
end)

test("a thinking pane animates from the wall clock", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local pane_root = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id .. "/panes/42"
  local activity_path = pane_root .. "/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/activity.json"
  local activity = decode_json(encode_json(samples.activity))
  activity.type = "thinking"
  write_json_path(activity_path, activity)
  -- The fixture's review flag outranks thinking; this is about the spinner.
  assert(os.execute("rm -f " .. shell_quote(pane_root) .. "/reviews/*.json") == 0)
  local window = window_double({ tabs = { { { id = 4261, domain = "unix",
    attention = protocol_fixture.wire_sample } } }, focused = false })
  local key = internal.address_cache_key(protocol_fixture.wire_sample.address)
  local frames = {}
  for second = 1, 2 do
    attention.poll(window, { now_ms = second * 1000, now_unix_ns = protocol_fixture.state_case.now_unix_ns,
      call_after = function() end })
    frames[second] = internal.attention_cache[key].frame
  end
  materialize_state_case(protocol_fixture.state_case)
  assert(frames[1] == 1 and frames[2] == 2,
    "a thinking view must carry the wall-clock frame, got " .. tostring(frames[1]) .. ", " .. tostring(frames[2]))
end)

test("the spinner's frame does not rewrite the published tab order", function()
  write_activity(9870, "thinking")
  local drawn_tab = gui_tab({ window_id = 9871, tab_id = 9872, tab_index = 0, panes = { 9870 } })
  local path = tab_publication_path(9871)
  local real_open, writes = io.open, 0
  io.open = function(target, mode)
    if mode == "w" and target:sub(1, #path) == path then writes = writes + 1 end
    return real_open(target, mode)
  end
  local drawn, published = {}, {}
  local ok, failure = pcall(function()
    for second = 1, 4 do
      attention.poll(window_double({ tabs = { { 9870 } }, focused = false }),
        { now_ms = second * 1000, now_unix_ns = fixture_now })
      drawn[second] = rendered_text(format_tab_title(drawn_tab, { drawn_tab }))
      published[second] = read_tab_publication(9871).tabs[1].text
    end
  end)
  io.open = real_open
  assert(ok, failure)
  assert(drawn[1] ~= drawn[2], "the bar itself still animates")
  assert(published[1] == published[4], "the published text must not follow the frame")
  assert(writes == 1, "four seconds of spinning wrote the tab order " .. writes .. " times")
end)

test("redraw-induced polls terminate inside the current frame bucket", function()
  write_activity(873, "thinking")

  local reentries = 0
  local current_now = 1000
  local w = window_double({
    tabs = { { 870, 873 } },
    focused = true,
    active_pane_id = 870,
    on_action = function(window)
      for _ = 1, 2 do
        reentries = reentries + 1
        attention.poll(window, { now_ms = current_now, now_unix_ns = fixture_now })
      end
    end,
  })

  attention.poll(w, { now_ms = 1000, now_unix_ns = fixture_now })
  assert(reentries == 2, "the redraw action should induce two nested polls in this double")
  assert(w.action_calls == 1, "neither nested poll may request another action")

  current_now = 2000
  attention.poll(w, { now_ms = current_now, now_unix_ns = fixture_now })
  assert(reentries == 4 and w.action_calls == 2,
    "the next bucket should permit exactly one more action")
end)

test("a failed redraw action leaves marker and cache truth intact", function()
  write_activity(881, "notify")

  local w = poll_focused({
    tabs           = { { 880, 881 } },
    active_pane_id = 880,
    action_error   = "wezterm exploded",
  })

  assert(#w.actions == 0, "the failed action should record nothing")
  assert(activity_exists(881), "a failed redraw must not touch the marker")
  assert(attention.get_attention(881) == "notify", "a failed redraw must not touch the cache")

  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("redraw failed", 1, true),
    "the failure should be logged once, got " .. tostring(errors[1]))

  write_activity(881, "stop")
  attention.poll(w, { now_ms = 2000, now_unix_ns = fixture_now })
  assert(w.action_calls == 1, "a failed window should not retry the redraw action")
  assert(#drain_errors() == 0, "a disabled window should not repeat the runtime error")
end)

-- ── Window scoping and composition root ─────────────────────────────────────

test("polling one window never removes another window's cache entries", function()
  write_activity(901, "thinking")
  write_activity(902, "thinking")

  poll_focused({ tabs = { { 901 } }, active_pane_id = 901 })
  assert(attention.get_attention(901) == "thinking", "window A's pane should be cached")

  poll_focused({ tabs = { { 902 } }, active_pane_id = 902 })
  assert(attention.get_attention(901) == "thinking", "window A's entry must survive window B's poll")
  assert(attention.get_attention(902) == "thinking", "window B's pane should be cached")
end)

test("poll resolves current pane even when the event pane is supplied", function()
  write_activity(911, "notify")

  local w = window_double({ tabs = { { 911 } }, focused = true, active_pane_id = 911 })
  local spawned = with_plugin_command(nil, function()
    attention.poll(w, { active_pane = pane_from_entry(911), now_unix_ns = fixture_now })
  end)

  assert(w.active_pane_calls == 1, "acknowledgement authority must be resolved at use time")
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find("--pane-id 911", 1, true),
    "the current active pane should be acknowledged")
end)

test("the registered update-status handler resolves current pane before acknowledgement", function()
  -- A second, independent copy of the plugin: apply_to_config only registers
  -- handlers once per module instance.
  local second = dofile(repo_root .. "/plugin/init.lua")
  local first_new_handler = #(handlers["update-status"] or {}) + 1
  second.apply_to_config({}, { dir = test_dir, review_key = false, integration_root = writer_root })
  write_activity(921, "notify")

  local w = window_double({ tabs = { { 921 } }, focused = true, active_pane_id = 921 })
  local spawned = with_plugin_command(nil, function()
    for index = first_new_handler, #handlers["update-status"] do
      handlers["update-status"][index](w, pane_from_entry(921))
    end
  end)

  assert(w.active_pane_calls == 1, "the handler must not trust its captured event pane")
  local acknowledgements = 0
  for _, argv in ipairs(spawned) do
    if (plugin_arguments(argv) or ""):find("plugin acknowledge", 1, true) then
      acknowledgements = acknowledgements + 1
    end
  end
  assert(acknowledgements == 1, "the current active pane should be acknowledged")
end)

test("the review key redraws after the command it runs succeeds, and only then", function()
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir, integration_root = writer_root })
  local toggle = assert(config.keys and config.keys[1] and config.keys[1].action,
    "review key action was not registered")

  write_activity(971, "notify")
  write_activity(972, "review")
  local masked = window_double({ tabs = { { 971, 972 } }, focused = true, active_pane_id = 972 })
  review.poll(window_double({ tabs = { { 971, 972 } }, focused = false }), { now_unix_ns = fixture_now })
  local spawned = with_plugin_command(reviewing_answer, function()
    toggle(masked, pane_from_entry(972))
  end)
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find("^plugin clear%-review .*%-%-pane%-id 972"),
    "a flagged tab withdraws the user's flag: " .. tostring(plugin_arguments(spawned[1] or {})))
  assert(masked.action_calls == 1, "a successful review removal should redraw once")

  write_activity(974, "notify")
  local unflagged = window_double({ tabs = { { 974 } }, focused = true, active_pane_id = 974 })
  spawned = with_plugin_command(reviewing_answer, function()
    toggle(unflagged, pane_from_entry(974))
  end)
  local wire = seeded_wire(974)
  assert(#spawned == 1 and plugin_arguments(spawned[1]) == table.concat({
    "plugin", "set-review",
    "--realm-id", wire.address.realm_id,
    "--incarnation-id", wire.address.incarnation_id,
    "--pane-id", "974",
    "--launch-id", wire.launch_id,
  }, " "), "an unflagged tab flags the focused pane: " .. tostring(plugin_arguments(spawned[1] or {})))
  assert(unflagged.action_calls == 1, "setting the flag should redraw once")
  assert(review.get_attention(974) == "review" or select(6, review.get_attention(974)) == true,
    "the flag shows at once")

  write_activity(975, "notify")
  local failed = window_double({ tabs = { { 975 } }, focused = true, active_pane_id = 975 })
  spawned = with_plugin_command(function()
    return false, '{"schema":1,"command":"plugin set-review","status":"unavailable",'
      .. '"complete":false,"result":{},"diagnostics":[{"code":"probe_unavailable",'
      .. '"message":"state lock timed out","context":{},"help":""}]}'
  end, function()
    toggle(failed, pane_from_entry(975))
    toggle(failed, pane_from_entry(975))
  end)
  assert(#spawned == 2, "each press runs the command")
  assert(failed.action_calls == 0, "a failed write must not claim a redraw")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("state lock timed out", 1, true),
    "the failure should be logged once with the command's reason, got " .. tostring(errors[1]))
end)

-- ── Which id names the pane ──────────────────────────────────────────

test("a local pane with no user var is named by its own pane id", function()
  local pane = mux_pane(7002)
  assert(attention.pane_marker_id(pane) == "7002",
    "a local pane's id is its pane id even unpublished")
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
    "an unpublished remote pane's local id names some other pane")

  -- Cache pane 7003's activity from the pane that publishes it, in one window.
  write_activity(7003, "notify")
  poll({ 7003 })
  assert(attention.get_attention(7003) == "notify", "precondition: pane 7003 is cached")

  -- Another window holds a remote pane the GUI also numbered 7003. Polling it
  -- must not touch pane 7003's cache entry.
  attention.poll(window_double({
    tabs    = { { { id = 7003, domain = "unix" } } },
    focused = false,
  }))
  assert(attention.get_attention(7003) == "notify", "an unpublished remote pane is not pane 7003")

  -- And its tab renders nothing, rather than the other pane's notify.
  local rendered = format_tab_title(tab(7003, 7003, true))
  assert(type(rendered) == "string",
    "a tab of unresolvable panes must carry no tint")
  assert(not rendered:find("! ", 1, true),
    "a tab of unresolvable panes must not borrow another pane's indicator")
end)

test("a local pane's own id outranks a WEZTERM_PANE that disagrees with it", function()
  -- Printed by something in the pane -- a catted file, a remote prompt -- not
  -- by the pane's own shell, which reads the same id WezTerm numbered it with.
  assert(attention.pane_marker_id(mux_pane(7010, { published = 7011 })) == "7010",
    "a local pane must not answer to another pane's marker")
  assert(attention.pane_marker_id(mux_pane(7012, { published = 7012 })) == "7012")
  assert(attention.pane_marker_id(mux_pane(7013, { domain = "unix", published = 7011 })) == "7011",
    "a client domain's pane still goes by what it published")
end)

test("a pane's published identity is bounded before it is read", function()
  local wire = encode_json(protocol_fixture.wire_sample)
  local padded = wire:sub(1, -2) .. string.rep(" ", 5000) .. "}"
  local _, problem = internal.parse_wire_json(padded)
  assert(problem and problem.code == "record_invalid", "an oversized WEZTERM_ATTENTION was parsed")
  assert(internal.parse_wire_json(wire), "the writer's own value still parses")
  assert(attention.pane_marker_id(mux_pane(7014, { domain = "unix",
    published = string.rep("9", 21) })) == nil, "a pane id wider than any WezTerm id is not one")
end)

test("a v2 identity naming another pane is refused in a local pane", function()
  local wire = decode_json(encode_json(protocol_fixture.wire_sample))
  local foreign = internal.resolve_pane_read(mux_pane(4299, { attention = wire }))
  assert(foreign.kind == "invalid", "a local pane printed pane 42's identity and was believed")
  local own = internal.resolve_pane_read(mux_pane(tonumber(wire.address.pane_id), { attention = wire }))
  assert(own.kind == "claimed", "a local pane's own identity is still read")
end)

test("exec, WSL and serial domains are local, so their panes need no published id", function()
  local config = { serial_ports = { { name = "serial-dev" } } }
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config(config, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root })
  -- Set after apply_to_config, the way a config may.
  config.exec_domains = { { name = "exec-dev" } }
  for _, domain in ipairs({ "exec-dev", "serial-dev", "WSL:Ubuntu" }) do
    assert(instance.pane_marker_id(mux_pane(7020, { domain = domain, published = 7021 })) == "7020",
      domain .. " is a local domain, so its pane id names its markers")
  end
  assert(instance.pane_marker_id(mux_pane(7022, { domain = "SSHMUX:host" })) == nil,
    "a client domain's unpublished pane still has no marker id")

  local listed = dofile(repo_root .. "/plugin/init.lua")
  listed.apply_to_config({ wsl_domains = { { name = "my-wsl" } } }, { auto_poll = false,
    dir = test_dir, review_key = false, renderer = "manual", integration_root = writer_root })
  assert(listed.pane_marker_id(mux_pane(7023, { domain = "my-wsl" })) == "7023")
  assert(listed.pane_marker_id(mux_pane(7024, { domain = "WSL:Ubuntu" })) == nil,
    "an explicit wsl_domains list replaces the default WSL:<distro> domains")
end)

--- Poll an unpublished pane of `domain` twice under `config`, with the given
--- platform and environment, and return the sockets republication was sent to.
local function republished_sockets(config, domain, triple, environment)
  local spawned = {}
  local real_background, real_triple, real_getenv =
    wezterm.background_child_process, wezterm.target_triple, os.getenv
  wezterm.background_child_process = function(argv) spawned[#spawned + 1] = argv; return true end
  wezterm.target_triple = triple
  os.getenv = function(name)
    if environment[name] ~= nil then return environment[name] end
    return real_getenv(name)
  end
  local ok, failure = pcall(function()
    local instance = dofile(repo_root .. "/plugin/init.lua")
    instance.apply_to_config(config.at_load or {}, { auto_poll = false, dir = test_dir,
      review_key = false, renderer = "manual", integration_root = writer_root })
    if config.after_load then config.after_load(config.at_load) end
    local window = window_double({ tabs = { { { id = 9981, domain = domain } } }, focused = false })
    instance.poll(window, { call_after = function() end })
    instance.poll(window, { call_after = function() end })
  end)
  wezterm.background_child_process, wezterm.target_triple, os.getenv =
    real_background, real_triple, real_getenv
  assert(ok, failure)
  local sockets = {}
  for _, argv in ipairs(spawned) do
    for index, value in ipairs(argv) do
      if value == "--socket" then sockets[#sockets + 1] = argv[index + 1] end
    end
  end
  return sockets
end

test("the implicit unix domain republishes through WezTerm's default socket", function()
  local mac = republished_sockets({}, "unix", "aarch64-apple-darwin", {})
  assert(#mac == 1 and mac[1] == test_dir .. "/.local/share/wezterm/sock",
    "macOS keeps the socket under ~/.local/share/wezterm, got " .. tostring(mac[1]))
  local linux = republished_sockets({}, "unix", "x86_64-unknown-linux-gnu",
    { XDG_RUNTIME_DIR = "/run/user/1000/" })
  assert(#linux == 1 and linux[1] == "/run/user/1000/wezterm/sock",
    "Linux uses $XDG_RUNTIME_DIR/wezterm, got " .. tostring(linux[1]))
  local relative = republished_sockets({}, "unix", "x86_64-unknown-linux-gnu",
    { XDG_RUNTIME_DIR = "run/user" })
  assert(#relative == 1 and relative[1] == test_dir .. "/.local/share/wezterm/sock",
    "a relative XDG_RUNTIME_DIR is not a runtime directory, got " .. tostring(relative[1]))
end)

test("a unix domain listed without a socket path, even after apply_to_config, republishes", function()
  local late = republished_sockets({ at_load = {}, after_load = function(config)
    config.unix_domains = { { name = "dev-mux" }, { name = "pinned", socket_path = "/tmp/pinned.sock" } }
  end }, "dev-mux", "aarch64-apple-darwin", {})
  assert(#late == 1 and late[1] == test_dir .. "/.local/share/wezterm/sock",
    "a domain with no socket_path uses the default one, got " .. tostring(late[1]))
  local listed = republished_sockets({ at_load = { unix_domains = { { name = "dev-mux" } } } },
    "unix", "aarch64-apple-darwin", {})
  assert(#listed == 0, "a config that lists its own unix domains has no implicit \"unix\"")
end)

test("a pane with no socket to republish through is named once in the log", function()
  local proxied = republished_sockets({ at_load = { unix_domains = { { name = "remote-mux",
    proxy_command = { "ssh", "host", "wezterm", "cli", "proxy" } } } } },
    "remote-mux", "aarch64-apple-darwin", {})
  assert(#proxied == 0, "a proxied domain's mux is not the one behind a local socket")
  local warnings = drain_warnings()
  assert(#warnings == 1 and warnings[1]:find("remote-mux", 1, true),
    "the unpublished domain must be named once, got " .. #warnings)
  republished_sockets({}, "SSHMUX:host", "aarch64-apple-darwin", {})
  warnings = drain_warnings()
  assert(#warnings == 1 and warnings[1]:find("SSHMUX:host", 1, true))
end)

-- ── A closed pane versus a detached domain ──────────────────────────────────

test("a detached domain keeps the attention of panes still running on the server", function()
  write_activity(7101, "notify")
  local window_id = 7100

  attention.poll(window_double({
    tabs      = { { { id = 5, domain = "unix", attention = seeded_wire(7101) }, 40 } },
    focused   = false,
    window_id = window_id,
  }), { now_unix_ns = fixture_now })
  assert(attention.get_attention(7101) == "notify", "precondition: the remote pane is cached")

  -- The unix domain is detached: every one of its panes leaves this window in
  -- one tick, while the processes in them keep running.
  attention.poll(window_double({
    tabs      = { { 40 } },
    focused   = false,
    window_id = window_id,
  }), { now_unix_ns = fixture_now })

  assert(attention.get_attention(7101) == "notify", "its attention stays visible for the reattach")
end)

-- ── Activity metadata on the public read ──────────────────────────────────────

test("get_attention reports the activity's source and reserved tuple slot", function()
  write_activity(7201, "notify")
  poll({ 7201 })

  local atype, frame, source, reserved = attention.get_attention(7201)
  assert(atype == "notify", "the first two returns keep their meaning")
  assert(frame == nil, "a notify activity carries no frame")
  assert(source == "claude", "source should be the activity's source, got " .. tostring(source))
  assert(reserved == false, "the fourth tuple slot is reserved and always false")
end)

-- ── Hosts that repaint their own titles ─────────────────────────────────────

test("request_redraw = false performs no action when attention changes", function()
  local quiet = dofile(repo_root .. "/plugin/init.lua")
  quiet.apply_to_config({}, {
    auto_poll      = false,
    dir            = test_dir,
    review_key     = false,
    request_redraw = false,
  })

  write_activity(7301, "notify")
  local w = window_double({
    tabs = { { 7300, 7301 } }, focused = true, active_pane_id = 7300,
  })
  quiet.poll(w, { now_ms = 1000, now_unix_ns = fixture_now })

  assert(quiet.get_attention(7301) == "notify", "polling still fills the cache")
  assert(w.action_calls == 0, "no redraw action should be attempted at all")
  assert(#w.actions == 0, "and none recorded")
end)

-- ── Subagents ───────────────────────────────────────────────────────────────

--- Give the pane `count` live subagents, in place of any it had.
local function write_subagents(pane_id, count)
  if not path_exists(seeded_pane_root(pane_id) .. "/claim.json") then seed_pane(pane_id) end
  local agents = seeded_records_root(pane_id) .. "/agents"
  assert(os.execute("rm -rf " .. shell_quote(agents)) == 0)
  for index = 1, count do
    local presence = seeded_record(pane_id, "subagent_presence")
    presence.agent_id = "agent-" .. index
    presence.agent_key = internal.sha256(presence.agent_id)
    presence.event_id = next_event_id()
    presence.written_at_unix_ns = fixture_now
    write_json_path(agents .. "/" .. presence.agent_key .. ".json", presence)
  end
end

local function poll_at(pane_ids, spec)
  spec = spec or {}
  local w = window_double({
    tabs           = { pane_ids },
    focused        = spec.focused == true,
    active_pane_id = spec.active_pane_id,
    window_id      = spec.window_id,
  })
  attention.poll(w, { now_unix_ns = fixture_now, call_after = function() end })
  return w
end

test("live subagents keep a pane visible with no activity of its own", function()
  write_subagents(7511, 2)

  poll_at({ 7510, 7511 })

  local atype, frame, source, reserved, subagents = attention.get_attention(7511)
  assert(atype == nil, "a pane with no activity reports no type, got " .. tostring(atype))
  assert(frame == nil and source == nil, "and no frame or source")
  assert(reserved == false, "the fourth tuple slot stays false, got " .. tostring(reserved))
  assert(subagents == 2, "but its live subagents are reported, got " .. tostring(subagents))
end)

test("an acknowledged activity leaves its pane's subagent count behind", function()
  write_activity(7561, "stop")
  write_subagents(7561, 2)

  with_plugin_command(acknowledging_answer, function()
    poll_at({ 7560, 7561 }, { focused = true, active_pane_id = 7561 })
  end)
  assert(acknowledgement_exists(7561), "precondition: the viewed stop is acknowledged")

  poll_at({ 7560, 7561 })

  local atype, _, _, _, subagents = attention.get_attention(7561)
  assert(atype == nil, "the acknowledged activity is no longer effective")
  assert(subagents == 2, "but its subagents are still working, got " .. tostring(subagents))
  assert(internal.resolve_visible_attention({ seeded_key(7561) }).indicator == "+2 ",
    "so its tab shows the count alone")
end)

test("the tab indicator carries the subagent count", function()
  write_activity(7521, "stop")
  write_subagents(7522, 2)

  poll_at({ 7521, 7522 })

  local visible = internal.resolve_visible_attention({ seeded_key(7521), seeded_key(7522) })
  assert(visible.indicator == "✓+2 ",
    "the count rides in the indicator's own trailing space, got " .. tostring(visible.indicator))
  assert(visible.type == "stop", "the activity type is unchanged")
  assert(visible.color == "#12271c", "and so is its tint")

  local rendered = format_tab_title(tab(7521, 7522, true))
  assert(type(rendered) == "table", "the tab still carries a tint")
  assert(rendered[2].Text:find("✓+2 ", 1, true),
    "the rendered title should carry the count, got " .. tostring(rendered[2].Text))

  local count_only = internal.resolve_visible_attention({ seeded_key(7522) })
  assert(count_only.indicator == "+2 ",
    "with no activity the count is the whole indicator, got " .. tostring(count_only.indicator))
  assert(count_only.type == nil, "and it names no type")
  assert(count_only.color == nil, "a bare count keeps the tab's default colors")
end)

test("count-only keeps the default colors", function()
  local sentinel = { colors = { stop = "SENTINEL" } }
  internal.attention_cache["7591"] = { type = nil, subagents = 2 }
  local bare_visible = internal.resolve_visible_attention({ "7591" }, sentinel)
  assert(bare_visible.indicator == "+2 " and bare_visible.type == nil and bare_visible.color == nil,
    "a count must not turn into stop state")

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
    "a count read from records must also keep default colors")
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
  -- A poll maps the GUI's pane numbers to cache keys; these panes are unclaimed.
  built.poll(window_double({ tabs = { { 7601, 7602 } }, focused = false }))
  built._internal.attention_cache["7601"] = {
    type = "notify", activity_type = "notify", frame = nil, source = "claude",
    provider = "claude", subagents = 2, review = false,
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
  manual.poll(window_double({ tabs = { { 7601, 7602 } }, focused = false }))
  manual._internal.attention_cache["7601"] = {
    type = "notify", activity_type = "notify", frame = nil, source = "claude",
    provider = "claude", subagents = 2, review = false,
    binding_health = "valid",
  }
  local manual_rendered = manual.wrap_title_formatter(function(_, ctx)
    manual_ctx = ctx
    return "manual"
  end)(tab(7601, 7602, false), { "tabs" }, { "panes" }, {}, false, 80)

  for _, field in ipairs({
    "indicator", "attention_type", "attention_color", "subagents", "source",
    "provider", "review", "binding_health",
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

test("all rendered view fields participate in redraw equality", function()
  local baseline = {
    type = "notify", frame = 0, activity_type = "notify", event_id = "a",
    source = "claude", provider = "claude", subagents = 1,
    review = false, binding_phase = "active", pane_presence = "present",
    reader_confidence = "confirmed", binding_health = "valid",
    base_title = "base", settled_title = "settled",
  }
  for _, field in ipairs({
    "type", "frame", "activity_type", "event_id", "source", "provider",
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
    "one title sample must not settle")
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
  internal.sample_settled_title("17661", "launch", "valid", nil)
  internal.sample_settled_title("17661", "launch", "valid", nil)
  internal.sample_settled_title("17661", "launch", "bad\nvalue", nil)
  local rendered = format_tab_title(title_tab)
  assert(rendered:find("server-safe", 1, true),
    "an invalid fallback sample must not affect the server-owned title")
end)

local function has_control(text)
  return text:find("[%z\1-\31\127]") ~= nil or text:find("\194[\128-\159]") ~= nil
end

test("a directory name cannot carry escape sequences into the tab bar", function()
  local escaped = tab(17671, 17672, false)
  -- ESC [ 42 m, then CSI spelled as the single C1 character U+009B.
  escaped.active_pane.current_working_dir = { file_path = "/tmp/evil\27[42m\194\1550mname" }
  local rendered = rendered_text(format_tab_title(escaped))
  assert(not has_control(rendered), "a control character reached the tab bar")
  assert(rendered:find(": evilname ", 1, true), "the name must show without the sequences, got " .. rendered)
end)

test("a long or control-character tab text is published within the tab reader's bounds", function()
  local long_name = string.rep("\195\169", 200) -- 400 bytes of "é"
  local named = as_userdata({ tab_id = 9861, window_id = 9860, tab_index = 0, is_active = false,
    active_pane = gui_pane(9862), panes = { gui_pane(9862) }, tab_title = long_name })
  format_tab_title(named, { named })
  local text = assert(read_tab_publication(9860)).tabs[1].text
  assert(#text <= 256, "published text is " .. #text .. " bytes")
  assert(text:sub(-2) == "\195\169", "the cut must fall between characters")

  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    title_formatter = function() return "red\27[31m\7bell" end })
  local formatter = handlers["format-tab-title"][#handlers["format-tab-title"]]
  local plain = as_userdata({ tab_id = 9864, window_id = 9863, tab_index = 0, is_active = false,
    active_pane = gui_pane(9865), panes = { gui_pane(9865) } })
  formatter(plain, { plain })
  local formatted = assert(read_tab_publication(9863)).tabs[1].text
  assert(not has_control(formatted), "a formatter's control character was published")
  assert(formatted:find(": redbell ", 1, true),
    "the formatter's text must be published without the sequence, got " .. formatted)
end)

--- The rule `attention tabs` applies to a published tab text: the file is
--- read as JSON, which Rust decodes only from well-formed UTF-8 (no overlong
--- form, no surrogate, nothing past U+10FFFF), and the text must hold at most
--- 256 bytes and no character `char::is_control` is true for.
local function tab_reader_accepts(text)
  if #text > 256 then return false end
  local index, length = 1, #text
  while index <= length do
    local lead = text:byte(index)
    local size, low, high, code
    if lead < 0x80 then size, code = 1, lead
    elseif lead >= 0xC2 and lead <= 0xDF then size, low, high, code = 2, 0x80, 0xBF, lead - 0xC0
    elseif lead == 0xE0 then size, low, high, code = 3, 0xA0, 0xBF, 0
    elseif lead == 0xED then size, low, high, code = 3, 0x80, 0x9F, 0xD
    elseif lead >= 0xE1 and lead <= 0xEF then size, low, high, code = 3, 0x80, 0xBF, lead - 0xE0
    elseif lead == 0xF0 then size, low, high, code = 4, 0x90, 0xBF, 0
    elseif lead == 0xF4 then size, low, high, code = 4, 0x80, 0x8F, 4
    elseif lead >= 0xF1 and lead <= 0xF3 then size, low, high, code = 4, 0x80, 0xBF, lead - 0xF0
    else return false end
    for offset = 1, size - 1 do
      local byte = text:byte(index + offset)
      local first = offset == 1
      if not byte or byte < (first and low or 0x80) or byte > (first and high or 0xBF) then
        return false
      end
      code = code * 0x40 + (byte - 0x80)
    end
    if code < 0x20 or (code >= 0x7F and code <= 0x9F) then return false end
    index = index + size
  end
  return true
end

test("a formatter's broken UTF-8 is published as text the tab reader accepts", function()
  local R = "\239\191\189" -- U+FFFD, one per ill-formed part, as Rust's lossy decoding
  local cases = {
    { "a CJK title cut inside a character", ("中文标题"):sub(1, 4), "中" .. R },
    { "a lone continuation byte", "ab\128cd", "ab" .. R .. "cd" },
    { "an overlong slash", "a\192\175b", "a" .. R .. R .. "b" },
    { "an encoded surrogate", "a\237\160\128b", "a" .. R .. R .. R .. "b" },
    { "a code point past U+10FFFF", "a\244\144\128\128b", "a" .. R .. R .. R .. R .. "b" },
    { "a byte UTF-8 never uses", "a\255b", "a" .. R .. "b" },
    { "a four-byte character cut at the end", "a\240\159\142", "a" .. R },
    { "a C1 control spelled around an ESC", "a\194\27\128b", "a" .. R .. R .. "b" },
    { "well-formed text", "中文 é 🎉 plain", "中文 é 🎉 plain" },
  }
  local current
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    title_formatter = function() return current end })
  local formatter = handlers["format-tab-title"][#handlers["format-tab-title"]]
  for index, case in ipairs(cases) do
    current = case[2]
    local window_id = 9869 + index * 3
    local drawn = as_userdata({ tab_id = window_id + 1, window_id = window_id, tab_index = 0,
      is_active = false, active_pane = gui_pane(window_id + 2), panes = { gui_pane(window_id + 2) } })
    formatter(drawn, { drawn })
    local published = assert(read_tab_publication(window_id), case[1] .. ": nothing published")
    local text = published.tabs[1].text
    assert(tab_reader_accepts(text), case[1] .. ": the tab reader would refuse the window")
    assert(text:find(case[3], 1, true), case[1] .. ": expected the formatter's text as "
      .. case[3] .. ", got " .. text)
  end
end)

test("what a formatter returns is drawn repaired, in both renderers", function()
  local R = "\239\191\189"
  local cases = {
    { "a raw title with ESC and C1", "x\27[41mRED\194\1550m", ": xRED " },
    { "a CJK title cut by bytes", ("中文标题"):sub(1, 4), "中" .. R },
    -- What wezterm.format returns for a red foreground, bold, and "build".
    { "a styled wezterm.format result", "\27(B\27[0;1m\27[38:2::255:0:0mbuild\27(B\27[0m", ": build " },
    { "an OSC ended by BEL and one ended by ST", "\27]0;t\7a\27]2;u\27\\b", ": ab " },
    { "a device control string and a C1 OSC", "\27Pq#0\27\\c\194\157x\7d", ": cd " },
    { "a CSI left open at the end", "build\27[38;2", ": build " },
    { "an OSC left open at the end", "build\27]0;title", ": build " },
  }
  local current
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    title_formatter = function() return current end })
  local formatter = handlers["format-tab-title"][#handlers["format-tab-title"]]
  local wrapped = instance.wrap_title_formatter(function() return current end)
  for index, case in ipairs(cases) do
    current = case[2]
    local window_id = 9990 + index * 3
    local drawn = as_userdata({ tab_id = window_id + 1, window_id = window_id, tab_index = 0,
      is_active = false, active_pane = gui_pane(window_id + 2), panes = { gui_pane(window_id + 2) } })
    for name, render in pairs({ tab = formatter, manual = wrapped }) do
      local text = rendered_text(render(drawn, { drawn }))
      assert(tab_reader_accepts(text), case[1] .. ": the " .. name .. " renderer drew "
        .. text:gsub("[^\32-\126]", function(c) return string.format("\\%d", c:byte()) end))
      assert(text:find(case[3], 1, true), case[1] .. ": the " .. name
        .. " renderer must still draw the formatter's text, got " .. text)
    end
  end
end)

test("a tab with no name, directory or settled title shows the pane's current title", function()
  local bare = tab(17681, 17682, false)
  bare.active_pane.title = "vim\27]0;x"
  local rendered = rendered_text(format_tab_title(bare))
  assert(rendered:find(": vim ", 1, true), "the current title must fill an empty base, got " .. rendered)
  local context
  attention.wrap_title_formatter(function(_, ctx) context = ctx; return ctx.default_title end)(bare)
  assert(context.default_title == "vim" and context.settled_title == nil,
    "default_title carries the current title without claiming it settled")
end)

test("a change in the subagent count alone requests a redraw", function()
  write_activity(7531, "stop")
  write_subagents(7531, 2)

  -- 7530 is the active pane and has no records, so nothing is acknowledged
  -- and the stop on 7531 stays as it is throughout.
  local w = window_double({
    tabs = { { 7530, 7531 } }, focused = true, active_pane_id = 7530,
  })
  attention.poll(w, { now_unix_ns = fixture_now, call_after = function() end })
  assert(#w.actions == 1, "the activity appearing is the first visible change, got " .. #w.actions)

  attention.poll(w, { now_unix_ns = fixture_now, call_after = function() end })
  assert(#w.actions == 1, "an unchanged tick must not redraw again, got " .. #w.actions)

  write_subagents(7531, 3)
  attention.poll(w, { now_unix_ns = fixture_now, call_after = function() end })

  assert(select(5, attention.get_attention(7531)) == 3, "the third subagent should be counted")
  assert(#w.actions == 2,
    "the count changing is itself a visible change, got " .. #w.actions)
end)

-- ── The review flag ─────────────────────────────────────────────────────────

test("the review flag outranks a thinking activity without replacing it", function()
  write_activity(7601, "thinking")
  write_activity(7601, "review")

  poll_at({ 7600, 7601 })

  local atype, frame, _, _, _, flagged = attention.get_attention(7601)
  assert(atype == "review", "the flag should outrank thinking, got " .. tostring(atype))
  assert(frame == nil, "a review indicator carries no spinner frame, got " .. tostring(frame))
  assert(flagged == true, "the entry should record the flag, got " .. tostring(flagged))
  assert(internal.attention_cache[seeded_key(7601)].activity_type == "thinking",
    "the activity underneath is untouched")
  assert(internal.resolve_visible_attention({ seeded_key(7601) }).indicator == "◆ ",
    "and the tab should show the review glyph")
end)

test("a stop outranks the flag until that stop is acknowledged", function()
  write_activity(7611, "stop")
  write_activity(7611, "review")

  poll_at({ 7610, 7611 })
  local atype, _, _, _, _, flagged = attention.get_attention(7611)
  assert(atype == "stop", "stop outranks review, got " .. tostring(atype))
  assert(flagged == true, "but the entry still carries the flag, got " .. tostring(flagged))
  assert(internal.resolve_visible_attention({ seeded_key(7611) }).indicator == "✓ ",
    "and the tab shows the stop glyph")

  with_plugin_command(acknowledging_answer, function()
    poll_at({ 7610, 7611 }, { focused = true, active_pane_id = 7611 })
  end)
  assert(acknowledgement_exists(7611), "precondition: the viewed stop is acknowledged")

  local after, _, _, _, _, after_flagged = attention.get_attention(7611)
  assert(after == "review",
    "with the stop acknowledged the flag becomes visible, got " .. tostring(after))
  assert(after_flagged == true, "and is still recorded, got " .. tostring(after_flagged))
  assert(path_exists(user_review_path(7611)), "acknowledgement never removes the flag")
end)

test("Alt+B flags a pane with an activity, and one press clears the user's flags from the tab", function()
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir, integration_root = writer_root })
  local toggle = assert(config.keys and config.keys[1] and config.keys[1].action,
    "review key action was not registered")

  write_activity(7621, "thinking")
  write_activity(7622, "notify")
  local tabs = { { 7621, 7622 } }
  review.poll(window_double({ tabs = tabs, focused = false }), { now_unix_ns = fixture_now })

  local w = window_double({ tabs = tabs, focused = true, active_pane_id = 7621 })
  with_plugin_command(reviewing_answer, function() toggle(w, pane_from_entry(7621)) end)

  assert(path_exists(user_review_path(7621)), "Alt+B flags a pane an agent is working in")
  assert(review.get_attention(7621) == "review",
    "and the flag must take the pane's indicator without another poll")
  assert(w.action_calls == 1, "a successful flag should redraw once, got " .. w.action_calls)

  -- The sibling carries the user's flag too, and another owner's review.
  write_activity(7622, "review")
  local other = seeded_record(7622, "review")
  other.owner_id = "pi-bus"
  other.owner_key = internal.sha256(other.owner_id)
  other.event_id = next_event_id()
  local other_path = seeded_pane_root(7622) .. "/reviews/" .. other.owner_key .. ".json"
  write_json_path(other_path, other)
  local spawned = with_plugin_command(reviewing_answer, function() toggle(w, pane_from_entry(7621)) end)

  assert(#spawned == 2, "one press clears the user's flag from every pane of the tab")
  assert(not path_exists(user_review_path(7621)) and not path_exists(user_review_path(7622)),
    "the user's flags are gone")
  assert(path_exists(other_path), "another owner's review is that owner's to withdraw")
  assert(activity_exists(7621) and activity_exists(7622),
    "clearing the flag must leave both activities on disk")
  assert(review.get_attention(7621) == "thinking",
    "the pressed pane falls back to its own activity, got "
      .. tostring(review.get_attention(7621)))
end)

test("flagging a pane whose stop is already shown requests a redraw", function()
  write_activity(7671, "stop")

  -- 7670 is the active pane and has no records, so nothing is acknowledged.
  local w = window_double({
    tabs = { { 7670, 7671 } }, focused = true, active_pane_id = 7670,
  })
  attention.poll(w, { now_unix_ns = fixture_now })
  assert(#w.actions == 1, "the activity appearing is the first visible change, got " .. #w.actions)
  attention.poll(w, { now_unix_ns = fixture_now })
  assert(#w.actions == 1, "an unchanged tick must not redraw again, got " .. #w.actions)

  write_activity(7671, "review")
  attention.poll(w, { now_unix_ns = fixture_now })

  assert(select(6, attention.get_attention(7671)) == true, "the flag should be recorded")
  assert(#w.actions == 2, "the flag arriving is itself a change, got " .. #w.actions)
end)

-- ── Attention v2: protocol, identity, and wall-age reader ───────────────────

test("Lua accepts and rejects every shared protocol fixture row", function()
  assert(internal.sha256("") ==
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "SHA-256 empty-string vector must match")
  assert(internal.sha256("abc") ==
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "SHA-256 abc vector must match")
  local results = fixtures.parse_fixture_cases(protocol_fixture)
  assert(#results == #protocol_fixture.parse_cases, "every parse row must run")
  for _, result in ipairs(results) do
    assert(result.actual == result.expected,
      result.id .. " expected " .. tostring(result.expected) .. ", got " .. tostring(result.actual))
  end
end)

test("text checks refuse C1 controls the way Rust's char::is_control does", function()
  local api = dofile(repo_root .. "/plugin/protocol.lua")({
    wezterm = wezterm, protocol_path = repo_root .. "/protocol/v2.json" })
  -- U+0080, U+0085 (NEL) and U+009F, the first, a middle and the last C1.
  for _, text in ipairs({ "a\194\128b", "tty\194\133", "\194\159" }) do
    assert(not api.is_safe_text(text, 256), "C1 text passed: " .. text:gsub("[\128-\255]", "?"))
  end
  -- Neighbours that are not control characters: U+00A0, U+00E9 and U+2028.
  for _, text in ipairs({ "a\194\160b", "caf\195\169", "a\226\128\168b" }) do
    assert(api.is_safe_text(text, 256), "non-control text refused")
  end
  local claim = internal.deep_copy(protocol_fixture.record_samples.claim)
  claim.tty_path = "/dev/tty\194\133"
  local parsed, problem = api.parse_v2_record(claim, "claim")
  assert(not parsed and problem.code == "record_invalid", "a C1 path must make the record invalid")
  -- The escaped spelling decodes to the same character, so it fails the same way.
  claim.tty_path = "/dev/ttyNEL"
  local raw = encode_json(claim):gsub("NEL", "\\u0085")
  parsed, problem = api.parse_v2_record_json(raw, "claim")
  assert(not parsed and problem.code == "record_invalid", "an escaped C1 must make the record invalid")
end)

test("Lua and Python fixture semantics cover exact wall-age boundaries", function()
  local results = fixtures.fixture_eligibility_cases(protocol_fixture)
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

test("a record written after the poll's clock sample is fresh, not clock skew", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local activity_path = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id .. "/activity.json"
  local activity = decode_json(encode_json(samples.activity))
  activity.ttl_ms = 600000
  activity.written_at_unix_ns = "00000000610000000005"
  write_json_path(activity_path, activity)
  local clock = { "00000000610000000000", "00000000610000000009" }
  local sampled = 0
  local window = window_double({ tabs = { { { id = 4271, domain = "unix",
    attention = protocol_fixture.wire_sample } } }, focused = false })
  attention.poll(window, { call_after = function() end, utc_now = function()
    sampled = sampled + 1
    return clock[math.min(sampled, #clock)]
  end })
  local view = internal.attention_cache[internal.address_cache_key(protocol_fixture.wire_sample.address)]
  write_json_path(activity_path, samples.activity)
  assert(view.activity_type == "notify", "an activity written a moment after the sample was dropped")
  for _, item in ipairs(view.diagnostics) do
    assert(item.code ~= "clock_skew", "a later write is not a clock that is ahead")
  end
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

test("an end naming its binding event ends it though its stamp is older, as after a reboot", function()
  materialize_state_case(protocol_fixture.state_case)
  local samples = protocol_fixture.record_samples
  local bindings = test_dir .. "/v2/realms/" .. samples.claim.address.realm_id
    .. "/incarnations/" .. samples.claim.address.incarnation_id
    .. "/panes/42/launches/" .. samples.claim.launch_id
    .. "/bindings/" .. samples.binding.binding_id
  -- The monotonic clock restarts at boot: an end written after a reboot
  -- carries a smaller stamp than the binding recorded before it.
  local binding = decode_json(encode_json(samples.binding))
  binding.observed_mono_ns = "00000000005000000000"
  write_json_path(bindings .. "/binding.json", binding)
  local ending = decode_json(encode_json(samples.binding_end))
  ending.binding_event_id = binding.event_id
  write_json_path(bindings .. "/end.json", ending)
  local function phase(pane_id)
    local read = internal.resolve_pane_read(mux_pane(pane_id, {
      domain = "unix", attention = protocol_fixture.wire_sample,
    }))
    return internal.read_attention_view(read,
      protocol_fixture.state_case.now_unix_ns, { dir = test_dir, glob = wezterm.glob }).binding_phase
  end
  assert(phase(4283) == "ended", "the end names this binding event, whatever the clocks say")
  -- A resume records a new binding event; an older end naming the earlier one
  -- does not end it.
  binding.event_id = "00000000-0000-4000-8000-000000000015"
  write_json_path(bindings .. "/binding.json", binding)
  assert(phase(4284) == "active", "an end naming an earlier binding event does not end a resumed one")
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
  local previous_time = wezterm.time
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
  wezterm.time = previous_time
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

test("an invalid or future identity is diagnosed and shows nothing", function()
  local invalid_pane = {
    id = 7780, domain = "unix", published = 7781, attention = "not-json",
  }
  attention.poll(window_double({ tabs = { { invalid_pane } }, focused = false }))
  assert(attention.get_attention(7781) == nil,
    "a malformed identity reads nothing under the pane id it also published")
  local rendered = format_tab_title(tab(7780, 7782, false))
  assert(type(rendered) == "string" and not rendered:find("! ", 1, true),
    "a malformed identity renders no attention")
  local malformed_errors = drain_errors()
  assert(#malformed_errors == 1 and malformed_errors[1]:find("record_invalid", 1, true),
    "malformed identity must be diagnosed distinctly")

  local future = decode_json(encode_json(protocol_fixture.wire_sample))
  future.wire = 3
  local future_pane = { id = 7783, domain = "unix", published = 7781, attention = future }
  attention.poll(window_double({ tabs = { { future_pane } }, focused = false }))
  assert(attention.get_attention(7781) == nil, "a future identity reads nothing either")
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

test("a future core record stays visible as future schema", function()
  local future_wire = decode_json(encode_json(protocol_fixture.wire_sample))
  future_wire.address.pane_id = "44"
  local future_claim = decode_json(encode_json(protocol_fixture.record_samples.claim))
  future_claim.schema = 4
  future_claim.address.pane_id = "44"
  local claim_path = test_dir .. "/v2/realms/" .. future_wire.address.realm_id
    .. "/incarnations/" .. future_wire.address.incarnation_id
    .. "/panes/44/claim.json"
  write_json_path(claim_path, future_claim)

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

-- ── Attention v2: reader, review, lifecycle and cleanup regressions ─────────

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
  with_plugin_command(acknowledging_answer, function()
    attention.poll(window_double({
      tabs = { { active, sibling } }, focused = true, active_pane_id = active,
    }), {
      now_unix_ns = protocol_fixture.state_case.now_unix_ns,
      call_after = function() end,
    })
  end)

  local ack_raw = assert(read_path(binding_root_path .. "/ack.json"))
  local ack = assert(internal.parse_v2_record_json(ack_raw, "acknowledgement"))
  assert(ack.activity_event_id == samples.activity.event_id,
    "the acknowledgement must name the event the active pane displayed")
  assert(ack.target.kind == "binding" and ack.target.binding_id == samples.binding.binding_id,
    "the acknowledgement must name the selected binding")
  assert(not path_exists(sibling_root .. "/ack.json"),
    "focusing one claimed pane must not acknowledge its sibling")
  local key = internal.address_cache_key(wire.address)
  assert(internal.attention_cache[key].activity_type == nil,
    "the exact acknowledged activity must be suppressed on the same poll")
end)

--- A tab-source answer naming `socket`, with `incarnation_id` spelled out.
local function own_source_response(socket, incarnation_id)
  return '{"schema":1,"command":"tab-source","status":"ok","complete":true,"result":{'
    .. '"socket_path":' .. encode_json_string(socket) .. ',"realm_id":"' .. internal.sha256(socket)
    .. '","incarnation_id":"' .. incarnation_id .. '"},"diagnostics":[]}'
end

--- A plugin instance that has asked who its GUI is, and been told `socket`
--- with `incarnation_id`.
local function instance_knowing_its_mux(socket, incarnation_id)
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local previous = wezterm.run_child_process
  wezterm.run_child_process = function() return true, own_source_response(socket, incarnation_id), "" end
  instance._internal.acquire_tab_source(socket)
  wezterm.run_child_process = previous
  assert(instance._internal.tab_source(), "precondition: the source was answered")
  return instance
end

local function v2_ack_path(wire)
  return test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/"
    .. wire.address.incarnation_id .. "/panes/" .. wire.address.pane_id .. "/launches/"
    .. wire.launch_id .. "/bindings/" .. protocol_fixture.record_samples.binding.binding_id .. "/ack.json"
end

local function focus_v2_pane(instance, spec)
  with_plugin_command(acknowledging_answer, function()
    instance.poll(window_double({ tabs = { { spec } }, focused = true, active_pane_id = spec }),
      { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  end)
end

test("a local pane naming another mux's pane of the same number is refused, not acknowledged", function()
  local fixture_incarnation = protocol_fixture.wire_sample.address.incarnation_id
  -- This GUI's own mux is another realm than the one the pane's output names.
  local instance = instance_knowing_its_mux("/test/own-gui.sock", fixture_incarnation)
  local wire = materialize_v2_fixture(53)
  os.remove(v2_ack_path(wire))
  local spec = { id = 53, domain = "local", attention = wire }
  focus_v2_pane(instance, spec)
  assert(not path_exists(v2_ack_path(wire)), "another mux's notification must not be acknowledged")
  local key = internal.address_cache_key(wire.address)
  assert(instance._internal.attention_cache[key] == nil, "and must not be shown on this pane")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("another mux", 1, true),
    "the refusal says why, got " .. tostring(errors[1]))

  -- The same identity on a mux-client pane is that pane's own: its local
  -- number is this GUI's, and the published one is the server's.
  local client = { id = 9053, domain = "unix", attention = wire }
  focus_v2_pane(instance, client)
  assert(path_exists(v2_ack_path(wire)), "a mux-client pane's identity is not checked against this GUI")
  materialize_v2_fixture(53)
end)

test("a GUI with the manual renderer still asks who its mux is", function()
  local socket = "/test/own-gui-manual.sock"
  local instance = dofile(repo_root .. "/plugin/init.lua")
  local first = #(handlers["update-status"] or {}) + 1
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root })
  local previous = wezterm.run_child_process
  wezterm.run_child_process = function() return true, own_source_response(socket, string.rep("d", 64)), "" end
  local window = window_double({ tabs = {}, focused = false })
  local ok, failure = pcall(with_gui_socket, socket, function()
    for index = first, #(handlers["update-status"] or {}) do handlers["update-status"][index](window) end
  end)
  wezterm.run_child_process = previous
  assert(ok, failure)
  assert(instance._internal.tab_source(), "local panes are checked against the answer, so it is asked for")
end)

test("a local pane naming this GUI's own mux is read, and another incarnation of it is not", function()
  local socket = "/test/own-gui-2.sock"
  local fixture_incarnation = protocol_fixture.wire_sample.address.incarnation_id
  local instance = instance_knowing_its_mux(socket, fixture_incarnation)
  local wire = materialize_v2_fixture(54, internal.sha256(socket))
  os.remove(v2_ack_path(wire))
  focus_v2_pane(instance, { id = 54, domain = "local", attention = wire })
  assert(path_exists(v2_ack_path(wire)), "this GUI's own pane is acknowledged on focus")

  local restarted = instance_knowing_its_mux(socket, string.rep("c", 64))
  local stale = materialize_v2_fixture(55, internal.sha256(socket))
  os.remove(v2_ack_path(stale))
  focus_v2_pane(restarted, { id = 55, domain = "local", attention = stale })
  assert(not path_exists(v2_ack_path(stale)), "an earlier incarnation's records are not this pane's")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("another mux", 1, true), "refused, got " .. #errors)
end)

test("before a GUI knows its own mux, a local pane is checked against its socket's realm", function()
  local socket = "/test/own-gui-3.sock"
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local previous, real_time = wezterm.run_child_process, os.time
  local calls = 0
  wezterm.run_child_process = function()
    calls = calls + 1
    if calls == 1 then return false, "", "unused failure text" end
    return true, own_source_response(socket, protocol_fixture.wire_sample.address.incarnation_id), ""
  end
  local foreign = materialize_v2_fixture(56)
  local own = materialize_v2_fixture(57, internal.sha256(socket))
  local foreign_key = internal.address_cache_key(foreign.address)
  local own_key = internal.address_cache_key(own.address)
  local window = window_double({ tabs = { {
    { id = 56, domain = "local", attention = foreign },
    { id = 57, domain = "local", attention = own },
  } }, focused = false })
  local function refusals()
    local count = 0
    for _, message in ipairs(drain_errors()) do
      if message:find("another mux", 1, true) then count = count + 1 end
    end
    return count
  end
  local ok, failure = pcall(with_gui_socket, socket, function()
    instance.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
    assert(instance._internal.attention_cache[own_key] ~= nil, "a pane of the socket's realm is read at once")
    assert(instance._internal.attention_cache[foreign_key] == nil, "another realm's is not read yet")
    assert(#drain_errors() == 0, "nothing is wrong yet: the answer is still to come")
    instance._internal.acquire_tab_source(socket)
    instance.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
    assert(instance._internal.attention_cache[own_key] ~= nil, "still read while the retry waits")
    assert(instance._internal.attention_cache[foreign_key] == nil, "another realm's is still not read")
    assert(refusals() == 0, "a failed run with a retry to come refuses nothing")
    os.time = function() return real_time() + 60 end
    instance._internal.acquire_tab_source(socket)
    instance.poll(window, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  end)
  os.time = real_time
  wezterm.run_child_process = previous
  assert(ok, failure)
  assert(calls == 2, "the retry ran, got " .. calls)
  assert(instance._internal.attention_cache[own_key] ~= nil, "this GUI's own pane is read once answered")
  assert(instance._internal.attention_cache[foreign_key] == nil, "and another realm's is refused")
  assert(refusals() == 1, "the refusal is logged once the answer names this GUI's mux")
end)

test("Alt+B uses full addresses and clearing leaves other owners and activity", function()
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
  review.apply_to_config(config, { auto_poll = false, dir = test_dir, integration_root = writer_root })
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

  with_plugin_command(reviewing_answer, function() toggle(window, pane_a) end)
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

  with_plugin_command(reviewing_answer, function() toggle(window, pane_a) end)
  assert(not path_exists(user_path), "a press must remove the active pane's user review")
  assert(path_exists(pane_b_root .. "/reviews/" .. other.owner_key .. ".json"),
    "another owner's review is that owner's to withdraw")
  assert(read_path(activity_path) == activity_before,
    "clearing a review must preserve unrelated activity bytes")
end)

test("Alt+B names the launch the pane published, and says once why the command refused", function()
  local wire = materialize_v2_fixture(81)
  local pane_root = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/81"
  assert(os.execute("rm -f " .. shell_quote(pane_root) .. "/reviews/*.json") == 0)
  -- The claim names the fixture's launch; the pane still shows an older one.
  wire.launch_id = "00000000-0000-4000-8000-000000000999"
  local review = dofile(repo_root .. "/plugin/init.lua")
  local config = {}
  review.apply_to_config(config, { auto_poll = false, dir = test_dir, integration_root = writer_root })
  local toggle = assert(config.keys[#config.keys].action)
  local spec = { id = 9081, domain = "unix", attention = wire }
  local window = window_double({ tabs = { { spec } }, focused = true, active_pane_id = spec })
  local spawned = with_plugin_command(function()
    return false, '{"schema":1,"command":"plugin set-review","status":"unavailable",'
      .. '"complete":false,"result":{},"diagnostics":[{"code":"claim_stale","message":'
      .. '"the pane\'s claim does not name the launch the pane published","context":{},"help":""}]}'
  end, function() toggle(window, pane_from_entry(spec)) end)
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find("--launch-id " .. wire.launch_id, 1, true),
    "the command is asked about the launch the pane published")
  assert(window.action_calls == 0, "a refused flag claims no redraw")
  local errors = drain_errors()
  assert(#errors == 1 and errors[1]:find("claim_stale", 1, true), "the refusal must say why, got " .. #errors)
end)

test("a stop hidden behind a higher-ranked review flag is not acknowledged", function()
  local wire = materialize_v2_fixture(83)
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root,
    priority = { "thinking", "stop", "notify", "review" } })
  local v2_pane = { id = 83, domain = "unix", attention = wire }
  local spawned = with_plugin_command(acknowledging_answer, function()
    instance.poll(window_double({ tabs = { { v2_pane } }, focused = true, active_pane_id = v2_pane }),
      { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  end)
  assert(#spawned == 0, "the pane showed its review flag, not the notify")
end)

test("unpublished mux pane schedules one realm publish from the integration root", function()
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
    integration_root = writer_root,
  })
  local pane = { id = 9901, domain = "u2-test" }
  local window = window_double({ tabs = { { pane } }, focused = false })
  reloaded.poll(window, { call_after = function() end })
  reloaded.poll(window, { call_after = function() end })
  wezterm.background_child_process = original_background

  assert(#spawned == 1, "one realm should schedule one background publication")
  assert(spawned[1][1] == "env"
      and spawned[1][2] == "WEZTERM_ATTENTION_DIR=" .. test_dir
      and spawned[1][3] == writer_root .. "/bin/attention",
    "publication must use the integration root's command")
  assert(table.concat(spawned[1], " "):find(
    "hooks publish --socket /tmp/attention-u2-test.sock --quiet", 1, true),
    "publication must use the nested quiet realm command")
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  reloaded.poll(unpublished, { call_after = call_after })
  scheduled[1].callback()
  assert(#spawned == 2 and scheduled[2].delay == 5, "the first retry must use 5 seconds next")
  reloaded.poll(unpublished, { call_after = call_after })
  scheduled[2].callback()
  assert(#spawned == 3 and scheduled[3].delay == 10, "the second retry must use 10 seconds next")
  reloaded.poll(unpublished, { call_after = call_after })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
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
  reloaded.poll(window, { call_after = call_after })
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
  -- A consumer dates a pane's last request from these fields and keys an idle
  -- stretch by the binding, as a prompt-cache countdown in a WezTerm config
  -- does. It reads them without error handling beyond "absent means
  -- nothing to show", so a rename switches it off silently; this is where
  -- that becomes loud.
  assert(view.provider == samples.binding.provider and view.binding_id == snapshot.binding_id)
  assert(view.activity_type == "notify")
  for _, item in ipairs(view.lifecycle.observations) do
    assert(item.actor.kind == "lead" or item.actor.kind == "child")
    assert(type(item.written_at_unix_ns) == "string" and item.written_at_unix_ns:match("^%d+$"),
      "written_at_unix_ns must stay a decimal string: nanoseconds do not fit a Lua number")
  end
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
  raw:write('{"schema":4}'); raw:close()
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
  reloaded.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local window = window_double({ tabs = { { { id = 9971, domain = "unix", attention = wire } } }, focused = false })
  local pane = mux_pane(9971, { domain = "unix", attention = wire })
  local function read(focused)
    write_json_path(directory .. "/lifecycle.json", snapshot)
    local target = focused and window_double({ tabs = { { { id = 9971, domain = "unix", attention = wire } } }, focused = true, active_pane_id = { id = 9971, domain = "unix", attention = wire } }) or window
    with_plugin_command(acknowledging_answer, function()
      reloaded.poll(target, { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
    end)
    return assert(reloaded.get_attention_view(pane))
  end
  local view = read(false)
  local callback_consumer = dofile(repo_root .. "/examples/follow-up.lua").for_windows()
  local callback_scope = {address=view.address,launch_id=view.launch_id,target={kind="binding",binding_id=view.binding_id}}
  callback_consumer.on_view_change({kind="initial",window_id=90001,scope=callback_scope,view=view})
  assert(callback_consumer.appearance(90001,callback_scope)=="follow_up")
  callback_consumer.dismiss(90001,callback_scope)
  assert(callback_consumer.appearance(90001,callback_scope)=="base","local dismissal needs no provider callback")
  callback_consumer.on_view_change({kind="scope_lost",window_id=90001,previous_scope=callback_scope})
  assert(callback_consumer.appearance(90001,callback_scope)=="unknown")
  local consumer = dofile(repo_root .. "/examples/follow-up.lua").new()
  local other_consumer = dofile(repo_root .. "/examples/follow-up.lua").new()
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
  local module = dofile(repo_root .. "/examples/follow-up.lua")
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

test("an unchanged record is not parsed again, and a changed one is", function()
  local file = assert(io.open(repo_root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local fixture = decode_json(file:read("*a")); file:close()
  local id = 11101
  local wire = materialize_v2_fixture(id)
  local snapshot = decode_json(encode_json(fixture.cases[2].value))
  snapshot.address, snapshot.launch_id = wire.address, wire.launch_id
  snapshot.binding_id, snapshot.provider = protocol_fixture.record_samples.binding.binding_id, "claude"
  -- The harness encoder writes an empty table as {}, which is not an
  -- observation array, so both pools always hold something.
  snapshot.pools.requests.observations, snapshot.pools.general.observations = {}, {}
  for member = 1, 2 do
    for _, pool in ipairs({ "requests", "general" }) do
      local item = decode_json(encode_json(fixture.cases[2].value.pools.general.observations[1]))
      item.observation_id = string.format("00000000-0000-4000-8000-%012d", member + (pool == "requests" and 100 or 200))
      item.observed_mono_ns = string.format("%020d", member)
      item.correlation = { tool_call_id = pool .. member }
      if pool == "requests" then item.tool_name, item.tool_class, item.question_mode = "AskUserQuestion", "question", "blocking" end
      snapshot.pools[pool].observations[member] = item
    end
  end
  local binding_dir = test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/"
    .. wire.address.incarnation_id .. "/panes/" .. id .. "/launches/" .. wire.launch_id
    .. "/bindings/" .. snapshot.binding_id
  write_json_path(binding_dir .. "/lifecycle.json", snapshot)
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root })
  local pane = { id = id, domain = "unix", attention = wire }
  local window = window_double({ tabs = { { pane } }, focused = false })
  local options = { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end }
  local real_parse, parses = wezterm.json_parse, 0
  wezterm.json_parse = function(content) parses = parses + 1; return real_parse(content) end
  local ok, failure = pcall(function()
    instance.poll(window, options)
    local first = assert(instance.get_attention_view(mux_pane(id, pane)))
    assert(first.lifecycle.availability == "available" and #first.lifecycle.observations == 4,
      "precondition: the snapshot is valid")
    parses = 0
    instance.poll(window, options)
    assert(parses == 0, "an unchanged tree was parsed again: " .. parses .. " parses")
    local second = assert(instance.get_attention_view(mux_pane(id, pane)))
    assert(#second.lifecycle.observations == #first.lifecycle.observations
      and second.lifecycle.availability == "available", "the reused lifecycle must be the same facts")

    snapshot.pools.general.observations[2] = nil
    write_json_path(binding_dir .. "/lifecycle.json", snapshot)
    instance.poll(window, options)
    assert(parses > 0, "a changed lifecycle.json must be parsed")
    local third = assert(instance.get_attention_view(mux_pane(id, pane)))
    assert(#third.lifecycle.observations < #first.lifecycle.observations,
      "the changed snapshot's facts must replace the old ones")
  end)
  wezterm.json_parse = real_parse
  assert(ok, failure)
end)

test("consumer manifest classification agrees with Rust and rejects incompatible metadata", function()
  local load = dofile(repo_root .. "/plugin/protocol.lua")
  local api = load({ wezterm = wezterm, protocol_path = repo_root .. "/protocol/v2.json" })
  for _, row in ipairs({
    { "claude", "AskUserQuestion", "question", "blocking" },
    { "codex", "request_user_input", "question", "blocking" },
    { "codex", "request_user_input_async", "question", "nonblocking" },
    { "codex", "request_permissions", "permission" },
    { "pi", "AskUserQuestion", "generic" },
    { "unknown", "request_permissions", "generic" },
    { "codex", "request_user_input_async_extra", "generic" },
  }) do
    local class, mode = api.classify_lifecycle_tool(row[1], row[2])
    assert(class == row[3] and mode == row[4])
  end
  local manifest_file = assert(io.open(repo_root .. "/protocol/v2.json", "r"))
  local raw = manifest_file:read("*a"); manifest_file:close()
  local incompatible = test_dir .. "/consumer-manifest.json"
  write_json_path(incompatible, decode_json(raw:gsub('"manifest_schema": 2', '"manifest_schema": 1', 1)))
  local old = load({ wezterm = wezterm, protocol_path = incompatible })
  assert(old.protocol == nil and old.protocol_load_error)
  local invalid = decode_json(raw)
  invalid.tool_classification.codex.request_permissions.question_mode = "blocking"
  write_json_path(incompatible, invalid)
  local rejected = load({ wezterm = wezterm, protocol_path = incompatible })
  assert(rejected.protocol == nil and rejected.protocol_load_error)
  invalid = decode_json(raw)
  invalid.limits.safe_label_max_bytes = nil
  write_json_path(incompatible, invalid)
  local missing_bound = load({ wezterm = wezterm, protocol_path = incompatible })
  assert(missing_bound.protocol == nil and missing_bound.protocol_load_error)
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

test("GUI callback has per-window baselines and detached lifecycle updates", function()
  local wire = materialize_v2_fixture(13001, string.rep("c",64))
  local samples = protocol_fixture.record_samples
  local dir = test_dir .. "/v2/realms/" .. wire.address.realm_id .. "/incarnations/" .. wire.address.incarnation_id
    .. "/panes/13001/launches/" .. wire.launch_id .. "/bindings/" .. samples.binding.binding_id
  local snapshot = read_json_fixture(repo_root .. "/tests/fixtures/lifecycle/observations.json").cases[2].value
  snapshot.address, snapshot.launch_id, snapshot.binding_id, snapshot.provider = wire.address, wire.launch_id, samples.binding.binding_id, samples.binding.provider
  local function write_snapshot()
    local file=assert(io.open(dir.."/lifecycle.json","w"))
    local raw=encode_json(snapshot):gsub('"observations":{}','"observations":[]')
    assert(file:write(raw));assert(file:close())
  end
  write_snapshot()
  local messages, instance = {}, dofile(repo_root .. "/plugin/init.lua")
  local w1 = window_double({window_id=13011,tabs={{{id=13011,domain="mux",attention=wire}}},focused=false})
  local w2 = window_double({window_id=13012,tabs={{{id=13012,domain="mux",attention=wire}}},focused=false})
  local options = {now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end}
  instance.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false,settled_title_fallback=false,on_view_change=function(message)
    messages[#messages+1] = decode_json(encode_json(message))
    if message.view then message.view.provider="mutated"; message.scope.address.pane_id="mutated" end
    instance.poll(w1,options) -- reentrant delivery cannot recurse or mutate the baseline
  end})
  instance.apply_to_config({}, {on_view_change=function() error("replacement must be ignored") end})
  instance.poll(w1,options); instance.poll(w2,options)
  assert(#messages==2 and messages[1].kind=="initial" and messages[2].kind=="initial")
  assert(messages[1].window_id==13011 and messages[2].window_id==13012)
  instance.poll(w1,options); assert(#messages==2,"unchanged views do not replay")
  snapshot.snapshot_id="00000000-0000-4000-8000-000000000909"
  snapshot.pools.general.retention_floor_mono_ns="00000000000000000001"
  write_snapshot()
  instance.poll(w1,options); instance.poll(w2,options)
  assert(#messages==4 and messages[3].kind=="updated" and messages[4].kind=="updated",
    "messages="..#messages.." last="..messages[#messages].kind.." lifecycle="..tostring(messages[#messages].view.lifecycle and messages[#messages].view.lifecycle.availability))
  assert(messages[3].view.provider~="mutated" and messages[4].scope.address.pane_id=="13001")
  local original_open=io.open
  io.open=function(path,mode) if path==dir.."/lifecycle.json" then return nil,"Permission denied" end return original_open(path,mode) end
  local ok,problem=pcall(instance.poll,w1,options); io.open=original_open; assert(ok,problem)
  assert(messages[#messages].kind=="updated" and messages[#messages].view.lifecycle.availability=="cached")
  assert(messages[#messages].view.lifecycle.diagnostics[1].code=="probe_unavailable")
  instance.poll(w1,options); assert(messages[#messages].view.lifecycle.availability=="available")
  local count=#messages
  options.gui_windows={w2}; mux_windows_by_id[13011]=nil; instance.poll(w2,options)
  assert(#messages==count+1 and messages[#messages].kind=="scope_lost" and messages[#messages].window_id==13011)
  instance.poll(w2,options); assert(#messages==count+1,"closing one window cannot reset another")
  local fresh=dofile(repo_root.."/plugin/init.lua")
  fresh.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false,on_view_change=function(message) assert(message.kind=="initial") end})
  fresh.poll(w2,options)
  assert(#drain_errors()==0,"lifecycle read diagnostics belong to the lifecycle facet")
end)

test("a workspace switch hides a window without losing its views or its tab order", function()
  local wire = materialize_v2_fixture(13041, string.rep("f", 64))
  local messages, instance = {}, dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root,
    on_view_change = function(message) messages[#messages + 1] = message end })
  local shown = window_double({ window_id = 13041, focused = false,
    tabs = { { { id = 13041, domain = "mux", attention = wire } } } })
  local other = window_double({ window_id = 13042, focused = false, tabs = { { 13043 } } })
  local options = { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end }
  options.gui_windows = { shown }
  instance.poll(shown, options)
  assert(#messages == 1 and messages[1].kind == "initial")

  -- WezTerm reuses the GUI window for the other workspace's mux window; the
  -- first one leaves gui_windows() and stays in the mux.
  options.gui_windows = { other }
  instance.poll(other, options)
  for _, message in ipairs(messages) do
    assert(message.kind ~= "scope_lost", "a hidden window's views were reported lost")
  end
  options.gui_windows = { shown }
  instance.poll(shown, options)
  assert(#messages == 1, "switching back must not replay the unchanged view as initial")

  mux_windows_by_id[13041] = nil
  options.gui_windows = { other }
  instance.poll(other, options)
  assert(messages[#messages].kind == "scope_lost" and messages[#messages].window_id == 13041,
    "a window gone from the mux is a scope that was lost")

  publish_window(13044, 13045, 13046)
  window_double({ window_id = 13044, tabs = {}, focused = false })
  attention.poll(other, { gui_windows = { other } })
  assert(path_exists(tab_publication_path(13044)), "a hidden window keeps its tab order")
  mux_windows_by_id[13044] = nil
  attention.poll(other, { gui_windows = { other } })
  assert(not path_exists(tab_publication_path(13044)), "a closed window's tab order is withdrawn")
end)

test("GUI callback preserves binding targets through unavailable reads and replacements", function()
  local wire=materialize_v2_fixture(13002,string.rep("d",64))
  local samples=protocol_fixture.record_samples
  local pane_dir=test_dir.."/v2/realms/"..wire.address.realm_id.."/incarnations/"..wire.address.incarnation_id.."/panes/13002"
  local launch=pane_dir.."/launches/"..wire.launch_id
  os.remove(launch.."/bindings/"..samples.binding.binding_id.."/end.json")
  local messages={}
  local instance=dofile(repo_root.."/plugin/init.lua")
  instance.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false,on_view_change=function(message) messages[#messages+1]=message end})
  local window=window_double({window_id=13020,tabs={{{id=13020,domain="mux",attention=wire}}},focused=false})
  local options={now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end}
  instance.poll(window,options)
  local original_open=io.open
  io.open=function(path,mode) if path==launch.."/current-binding.json" then return nil,"Permission denied" end return original_open(path,mode) end
  local ok,problem=pcall(instance.poll,window,options); io.open=original_open; assert(ok,problem)
  assert(messages[#messages].kind=="updated" and messages[#messages].scope.target.binding_id==samples.binding.binding_id)
  instance.poll(window,options); assert(messages[#messages].kind=="updated")
  local pointer=decode_json(encode_json(samples.current_binding));pointer.address=wire.address;pointer.binding_id=string.rep("b",64)
  local count=#messages;write_json_path(launch.."/current-binding.json",pointer);instance.poll(window,options)
  assert(#messages==count+2 and messages[count+1].kind=="scope_lost" and messages[count+2].kind=="initial")
  assert(messages[count+2].scope.target.binding_id==pointer.binding_id and messages[count+2].view.provider==nil,"new unreadable binding cannot inherit old facts")
  os.remove(launch.."/current-binding.json");instance.poll(window,options)
  assert(messages[#messages].kind=="initial" and messages[#messages].scope.target.kind=="launch")
  pointer.binding_id=samples.binding.binding_id;write_json_path(launch.."/current-binding.json",pointer);instance.poll(window,options)
  assert(messages[#messages].kind=="initial" and messages[#messages].scope.target.kind=="binding")
  local ended=decode_json(encode_json(samples.binding_end));ended.address=wire.address;ended.observed_mono_ns="00000000009000000000"
  write_json_path(launch.."/bindings/"..pointer.binding_id.."/end.json",ended);instance.poll(window,options)
  assert(messages[#messages].kind=="updated" and messages[#messages].view.binding_phase=="ended")
  local unclaimed=window_double({window_id=13020,tabs={{13020}},focused=false})
  instance.poll(unclaimed,options);assert(messages[#messages].kind=="scope_lost")
  count=#messages;instance.poll(unclaimed,options);assert(#messages==count,"an unclaimed pane cannot fabricate a scope")
  local errors=drain_errors();assert(#errors==2,"only the injected pointer failure and missing new binding are expected")
end)

test("GUI callback exceptions do not corrupt future polls and title opt-out does no work", function()
  local instance=dofile(repo_root.."/plugin/init.lua")
  instance._internal.sample_settled_title("prior","launch","old",nil)
  local calls=0
  instance.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false,auto_clear={},settled_title_fallback=false,on_view_change=function() calls=calls+1;error("synthetic callback error") end})
  assert(next(instance._internal.settled_title_state)==nil)
  local wire=materialize_v2_fixture(13003,string.rep("e",64))
  local pane=mux_pane(13030,{domain="mux",attention=wire})
  local titles=0;pane.get_title=function() titles=titles+1;return tostring(titles) end
  local window=window_double({window_id=13030,focused=true})
  window.mux_window=function() return {tabs=function()return {{panes=function()return {pane}end}}end}end
  local options={now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end}
  drain_errors();instance.poll(window,options);local redraws=window.action_calls
  instance.poll(window,options);instance.poll(window,options)
  assert(titles==0 and next(instance._internal.settled_title_state)==nil)
  assert(window.action_calls==redraws,"title churn cannot redraw when disabled")
  assert(calls==1,"exception does not replay unchanged facts")
  local errors=drain_errors();assert(#errors>=1)
  for _,message in ipairs(errors)do assert(not message:find("title is changing",1,true))end
  local other=materialize_v2_fixture(13004,string.rep("e",64))
  pane.get_user_vars=function()return {WEZTERM_ATTENTION=encode_json(other)}end
  instance.poll(window,options);assert(calls==3,"scope loss and new initial still deliver after exceptions")
end)

test("an on_view_change error is logged with its text, once per distinct error", function()
  local failures = { "first failure", "first failure", "second failure" }
  local calls = 0
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    renderer = "manual", integration_root = writer_root, on_view_change = function()
      calls = calls + 1
      error(failures[calls] or "later failure", 0)
    end })
  local options = { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end }
  for index = 1, 3 do
    local wire = materialize_v2_fixture(13070 + index, string.rep("9", 64))
    instance.poll(window_double({ window_id = 13080 + index, focused = false,
      tabs = { { { id = 13070 + index, domain = "mux", attention = wire } } } }), options)
  end
  local logged = {}
  for _, message in ipairs(drain_errors()) do
    if message:find("on_view_change", 1, true) then logged[#logged + 1] = message end
  end
  assert(calls == 3, "every delivery must run, got " .. calls)
  assert(#logged == 2 and logged[1]:find("first failure", 1, true)
      and logged[2]:find("second failure", 1, true),
    "expected one line per distinct error with its text, got " .. #logged)
end)

test("scalar lookup refuses two full pane addresses", function()
  local a = materialize_v2_fixture(42, string.rep("a",64))
  local b = materialize_v2_fixture(42, string.rep("b",64))
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false})
  local w = window_double({tabs={{{id=12001,domain="a",attention=a},{id=12002,domain="b",attention=b}}},focused=false})
  instance.poll(w,{now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end})
  assert(instance.get_attention_view(mux_pane(12001,{domain="a",attention=a})))
  assert(instance.get_attention_view(mux_pane(12002,{domain="b",attention=b})))
  assert(instance.get_attention(42)==nil,"scalar lookup selected a realm")
end)

test("a shared full address survives one window dropping it", function()
  local a = materialize_v2_fixture(42, string.rep("e",64))
  local b = materialize_v2_fixture(43, string.rep("e",64))
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, {auto_poll=false,dir=test_dir,review_key=false})
  local options={now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end}
  local function window(id,wire) return window_double({window_id=id,tabs={{{id=id,domain="shared",attention=wire}}},focused=false}) end
  instance.poll(window(9020,a),options); instance.poll(window(9021,a),options)
  assert(instance.get_attention(42)=="notify","the same full address is not ambiguous")
  instance.poll(window(9020,b),options)
  assert(instance.get_attention(42)=="notify","another window still owns the cache entry")
  instance.poll(window(9021,b),options)
  assert(instance.get_attention(42)==nil,"no window still observes the old address")
  assert(instance.get_attention(43)=="notify")
end)

test("a first observation through Alt+B participates in scalar ambiguity", function()
  local a=materialize_v2_fixture(42,string.rep("a",64))
  local b=materialize_v2_fixture(42,string.rep("b",64))
  local instance=dofile(repo_root .. "/plugin/init.lua")
  local config={}
  instance.apply_to_config(config,{auto_poll=false,dir=test_dir,integration_root=writer_root})
  local pa={id=12011,domain="a",attention=a}
  local pb={id=12012,domain="b",attention=b}
  local wa=window_double({window_id=9022,tabs={{pa}},focused=false})
  local wb=window_double({window_id=9023,tabs={{pb}},focused=false})
  instance.poll(wa,{now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end})
  assert(instance.get_attention(42)=="notify")
  with_plugin_command(reviewing_answer, function()
    config.keys[#config.keys].action(wb,pane_from_entry(pb))
  end)
  assert(instance.get_attention_view(pane_from_entry(pb)))
  assert(instance.get_attention(42)==nil,"overlay observation must not select either realm")
  instance.poll(wa,{now_unix_ns=protocol_fixture.state_case.now_unix_ns,call_after=function()end})
  assert(instance.get_attention(42)==nil,"a sibling poll must retain the overlay observation")
end)

test("Alt+B replaces the same pane's earlier identity", function()
  local id=12013
  local instance=dofile(repo_root .. "/plugin/init.lua")
  local config={}
  instance.apply_to_config(config,{auto_poll=false,dir=test_dir,integration_root=writer_root})
  instance.poll(window_double({window_id=9024,tabs={{{id=id,domain="local"}}},focused=false}),
    {now_ms=1000,call_after=function()end})
  local wire=materialize_v2_fixture(id,string.rep("f",64))
  local entry={id=id,domain="local",attention=wire}
  local window=window_double({window_id=9024,tabs={{entry}},focused=false})
  with_plugin_command(reviewing_answer, function()
    config.keys[#config.keys].action(window,pane_from_entry(entry))
  end)
  local view=assert(instance.get_attention_view(pane_from_entry(entry)))
  assert(view.type and instance.get_attention(id)==view.type,"one pane must not count as two scalar owners")
end)

test("every v2 record has a bounded file read", function()
  local api=dofile(repo_root .. "/plugin/protocol.lua")({wezterm=wezterm,protocol_path=repo_root .. "/protocol/v2.json"})
  local original=io.open
  local requested
  io.open=function(path,mode)
    if path~="/synthetic/claim.json" then return original(path,mode) end
    return {read=function(_,count) requested=count; return "{}" end,close=function()return true end}
  end
  local ok=pcall(api.read_record_file,"/synthetic/claim.json","claim")
  io.open=original
  assert(ok)
  assert(requested==api.protocol.limits.max_json_bytes+1,"unbounded read: " .. tostring(requested))
end)

test("a retry needs a new live observation when inventory fails", function()
  local spawned,scheduled={},{}
  local old=wezterm.background_child_process
  wezterm.background_child_process=function(argv) spawned[#spawned+1]=argv; return true end
  local instance=dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({unix_domains={{name="lost-window",socket_path="/tmp/attention-lost-window.sock"}}},{auto_poll=false,dir=test_dir,review_key=false,integration_root=writer_root})
  local window=window_double({window_id=9002,tabs={{{id=12004,domain="lost-window"}}},focused=false})
  local opts={gui_windows=function()error("inventory unavailable")end,call_after=function(_,f)scheduled[#scheduled+1]=f end}
  instance.poll(window,opts); instance.poll(window,opts)
  assert(#spawned==1)
  for i=1,5 do if scheduled[i] then scheduled[i]() end end
  assert(#spawned==1,"republication continued without another live observation: " .. #spawned)
  instance.poll(window,opts); instance.poll(window,opts)
  assert(#spawned==2,"fresh polls must restart publication")
  instance.poll(window,opts)
  scheduled[#scheduled]()
  assert(#spawned==3,"a fresh observation permits the retry")
  wezterm.background_child_process=old
end)

--- Load a fresh copy of the plugin with the given process environment, which
--- the plugin reads when it loads.
local function load_with_environment(environment)
  local real_getenv = os.getenv
  os.getenv = function(name)
    if environment[name] ~= nil then return environment[name] or nil end
    if name == "WEZTERM_ATTENTION_DIR" or name == "XDG_STATE_HOME" then return nil end
    return real_getenv(name)
  end
  local ok, instance = pcall(dofile, repo_root .. "/plugin/init.lua")
  os.getenv = real_getenv
  assert(ok, instance)
  return instance
end

test("an unknown option or a value of the wrong kind is named, and the default used", function()
  local before = #(handlers["format-tab-title"] or {})
  local instance = dofile(repo_root .. "/plugin/init.lua")
  local ok, failure = pcall(instance.apply_to_config, {}, { auto_poll = false, dir = test_dir,
    review_key = false, integration_root = writer_root,
    acknowledge_types = { "stop" }, renderer = "tabs", colors = "red", show_provider = "yes" })
  assert(ok, "a wrong option must not break the config: " .. tostring(failure))
  local warnings = table.concat(drain_warnings(), "\n")
  assert(warnings:find("acknowledge_types", 1, true) and warnings:find("auto_clear", 1, true),
    "a misnamed option must be named with the real one: " .. warnings)
  assert(warnings:find("renderer", 1, true) and warnings:find("tabs", 1, true), "bad renderer: " .. warnings)
  assert(warnings:find("colors", 1, true) and warnings:find("show_provider", 1, true), warnings)
  assert(#handlers["format-tab-title"] == before + 1,
    "an unrecognised renderer falls back to the default tab renderer")
  assert(instance._active_colors.stop == "#12271c" and instance._active_show_provider == false)
end)

test("options that went with flat markers are named as ignored, and change nothing", function()
  local before = #(handlers["format-tab-title"] or {})
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root, stale_after_ms = { thinking = 1 }, format_tab_title = false })
  local warnings = table.concat(drain_warnings(), "\n")
  assert(warnings:find("unknown option stale_after_ms is ignored; it applied only to flat marker files", 1, true),
    "stale_after_ms is named with the reason it went: " .. warnings)
  assert(warnings:find('unknown option format_tab_title is ignored; the option is renderer = "manual"', 1, true),
    "format_tab_title is named with the option that replaced it: " .. warnings)
  assert(#handlers["format-tab-title"] == before + 1, "format_tab_title does not choose the renderer")
  assert(instance.remove_marker == nil, "remove_marker went with the flat files it removed")
end)

test("a dir option the writer would refuse is named, and the default used", function()
  local cases = {
    { "~/.local/state/wezterm-attention", "not an absolute path" },
    { "relative/state", "not an absolute path" },
    { test_dir .. "/bad\27name", "control character" },
    { test_dir .. "/bad\233name", "UTF-8" },
    { "/" .. string.rep("d", 4096), "4096 bytes" },
  }
  for _, case in ipairs(cases) do
    local commands = {}
    local real_execute = os.execute
    os.execute = function(command) commands[#commands + 1] = command; return 0 end
    local config = {}
    local ok, failure = pcall(function()
      dofile(repo_root .. "/plugin/init.lua").apply_to_config(config, { auto_poll = false,
        review_key = false, renderer = "manual", integration_root = writer_root, dir = case[1] })
    end)
    os.execute = real_execute
    assert(ok, failure)
    local warnings = drain_warnings()
    local named = false
    for _, message in ipairs(warnings) do
      if message:find("option dir", 1, true) and message:find(case[2], 1, true)
          and not message:find("\27", 1, true) then named = true end
    end
    assert(named, "the warning names the rule for " .. case[2] .. ": " .. table.concat(warnings, "\n"))
    local exported = config.set_environment_variables.WEZTERM_ATTENTION_DIR
    assert(exported ~= case[1] and exported:sub(1, 1) == "/", "the refused dir must not reach panes")
    assert(#commands == 1 and not commands[1]:find(case[1], 1, true),
      "the refused dir must not be created")
  end
end)

test("a wrong value inside an option table is named, and its default used", function()
  write_activity(9760, "thinking")
  local cases = {
    { indicators = { thinking_frames = "* " }, name = "indicators.thinking_frames" },
    { indicators = { thinking_frames = {} }, name = "indicators.thinking_frames" },
    { indicators = { thinking_frames = { "a ", 2 } }, name = "indicators.thinking_frames" },
    { indicators = { stop = 1 }, name = "indicators.stop" },
    { colors = { thinking = { "#000000" } }, name = "colors.thinking" },
    { review_key = { "b", "ALT" }, name = "review_key" },
    { review_key = { key = "b", mods = 3 }, name = "review_key" },
  }
  for _, case in ipairs(cases) do
    local config = {}
    local instance = dofile(repo_root .. "/plugin/init.lua")
    local handler = #(handlers["format-tab-title"] or {}) + 1
    local ok, failure = pcall(instance.apply_to_config, config, { auto_poll = false, dir = test_dir,
      integration_root = writer_root, indicators = case.indicators, colors = case.colors,
      review_key = case.review_key })
    assert(ok, "a wrong value must not break the config: " .. tostring(failure))
    local warnings = table.concat(drain_warnings(), "\n")
    assert(warnings:find("option " .. case.name, 1, true) and warnings:find("default", 1, true),
      case.name .. " must be named: " .. warnings)
    instance.poll(window_double({ tabs = { { 9760 } }, focused = false }),
      { now_unix_ns = fixture_now })
    local drawn, rendered = pcall(handlers["format-tab-title"][handler], tab(9760, 9761, false))
    assert(drawn, case.name .. ": the tab must still draw: " .. tostring(rendered))
    local text = rendered_text(rendered)
    assert(text:find("[◌◔◑◕]") and rendered[1].Background.Color == "#1c1730",
      case.name .. ": the default spinner and tint are drawn, got " .. text)
    local key = config.keys[#config.keys]
    assert(key.key == "b" and key.mods == "ALT", case.name .. ": the review key is Alt+B")
  end
end)

test("the state root and tabs directory are created private to the user", function()
  local root = test_dir .. "/private-root"
  local instance = dofile(repo_root .. "/plugin/init.lua")
  instance.apply_to_config({}, { auto_poll = false, review_key = false, renderer = "manual", dir = root })
  for _, path in ipairs({ root, root .. "/tabs" }) do
    local listing = assert(io.popen("ls -ld " .. shell_quote(path)))
    local mode = (listing:read("*l") or ""):sub(1, 10)
    listing:close()
    assert(mode == "drwx------", path .. " was created " .. mode)
  end
end)

test("on Windows the directories are made with cmd.exe's mkdir", function()
  local commands = {}
  local real_execute, real_config = os.execute, package.config
  package.config = "\\\n;\n?\n!\n-\n"
  os.execute = function(command) commands[#commands + 1] = command; return 0 end
  local ok, failure = pcall(function()
    local instance = dofile(repo_root .. "/plugin/init.lua")
    instance.apply_to_config({}, { auto_poll = false, review_key = false, renderer = "manual",
      dir = "C:/Users/someone/state" })
  end)
  os.execute, package.config = real_execute, real_config
  assert(ok, failure)
  assert(#commands == 1, "one directory command, got " .. #commands)
  assert(not commands[1]:find("-p", 1, true) and not commands[1]:find("umask", 1, true),
    "a POSIX command reached cmd.exe: " .. commands[1])
  assert(commands[1]:find('mkdir "C:\\Users\\someone\\state\\tabs"', 1, true),
    "the tabs directory must be named in Windows form: " .. commands[1])
end)

test("the default state root follows the same order as the writer", function()
  local home_default = test_dir .. "/.local/state/wezterm-attention"
  local cases = {
    { env = { WEZTERM_ATTENTION_DIR = test_dir .. "/explicit", XDG_STATE_HOME = test_dir .. "/xdg" },
      root = test_dir .. "/explicit" },
    { env = { WEZTERM_ATTENTION_DIR = "", XDG_STATE_HOME = test_dir .. "/xdg" },
      root = test_dir .. "/xdg/wezterm-attention" },
    { env = { XDG_STATE_HOME = test_dir .. "/xdg/" }, root = test_dir .. "/xdg/wezterm-attention" },
    { env = { XDG_STATE_HOME = "relative/state" }, root = home_default },
    { env = { XDG_STATE_HOME = "" }, root = home_default },
    { env = {}, root = home_default },
    { env = { WEZTERM_ATTENTION_DIR = "relative/dir" }, root = home_default, warned = true },
  }
  -- The writer takes a root only when it is at most the manifest's path bound
  -- in bytes and holds no control character, C1 included.
  local manifest = assert(io.open(repo_root .. "/protocol/v2.json", "r"))
  local limit = decode_json(manifest:read("*a")).limits.path_max_bytes
  manifest:close()
  local function path_of(bytes) return ("/" .. string.rep("a", 7)):rep(bytes / 8) end
  assert(#path_of(limit) == limit, "precondition: the bound is a multiple of 8")
  for _, unsafe in ipairs({ test_dir .. "/x\1y", test_dir .. "/x\27[31my", test_dir .. "/x\194\133y",
      test_dir .. "/x\127y", path_of(limit) .. "b" }) do
    cases[#cases + 1] = { env = { XDG_STATE_HOME = unsafe }, root = home_default }
    cases[#cases + 1] = { env = { WEZTERM_ATTENTION_DIR = unsafe }, root = home_default, warned = true }
  end
  -- The writer reads its environment as text, so bytes that are not UTF-8,
  -- a lone or cut-short sequence or an overlong form, name no root it can use;
  -- it refuses the one that would decide the root, and the log says so here.
  for _, broken in ipairs({ test_dir .. "/x\233y", test_dir .. "/x\226\130", test_dir .. "/x\192\175y" }) do
    cases[#cases + 1] = { env = { XDG_STATE_HOME = broken }, root = home_default,
      warned = "XDG_STATE_HOME" }
    cases[#cases + 1] = { env = { WEZTERM_ATTENTION_DIR = broken }, root = home_default, warned = true }
  end
  cases[#cases + 1] = { env = { XDG_STATE_HOME = test_dir .. "/état" },
    root = test_dir .. "/état/wezterm-attention" }
  cases[#cases + 1] = { env = { XDG_STATE_HOME = path_of(limit) },
    root = path_of(limit) .. "/wezterm-attention" }
  cases[#cases + 1] = { env = { WEZTERM_ATTENTION_DIR = path_of(limit) }, root = path_of(limit) }
  local real_execute = os.execute
  for index, case in ipairs(cases) do
    local instance = load_with_environment(case.env)
    -- A root at the path bound is longer than this system lets mkdir create.
    os.execute = function(command)
      if #command > 1000 then return 0 end
      return real_execute(command)
    end
    local ok, failure = pcall(instance.apply_to_config, {}, { auto_poll = false, review_key = false,
      renderer = "manual", integration_root = writer_root })
    os.execute = real_execute
    assert(ok, failure)
    assert(instance._active_dir == case.root,
      "case " .. index .. ": expected " .. case.root .. ", got " .. tostring(instance._active_dir))
    local warnings = drain_warnings()
    if case.warned then
      local name = case.warned == true and "WEZTERM_ATTENTION_DIR" or case.warned
      assert(#warnings == 1 and warnings[1]:find(name, 1, true)
          and not warnings[1]:find("[\128-\255]"),
        "case " .. index .. ": " .. name .. " must be named once in the log, without its bytes")
    else
      assert(#warnings == 0, "case " .. index .. " warned: " .. tostring(warnings[1]))
    end
  end
end)

test("loading without a module path fails with directions, and an explicit path loads", function()
  local chunk = assert(loadfile(repo_root .. "/plugin/init.lua"))
  -- WezTerm's Lua has no debug library, so dofile gives the plugin no way to
  -- find its own directory. The harness has one; hide it.
  local saved_debug = rawget(_G, "debug")
  _G.debug = nil
  local ok, failure = pcall(chunk)
  local explicit_ok, explicit = pcall(chunk, "wezterm-attention", repo_root .. "/plugin/init.lua")
  _G.debug = saved_debug
  assert(not ok and tostring(failure):find("wezterm.plugin.require", 1, true)
      and tostring(failure):find("loadfile", 1, true),
    "the failure must say how to load the plugin, got: " .. tostring(failure))
  assert(explicit_ok and type(explicit.apply_to_config) == "function",
    "a module path passed by the caller must be enough: " .. tostring(explicit))
end)

--- A copy of the plugin in its own directory with no writer built, the layout
--- `wezterm.plugin.require` produces before anyone runs install-cli.sh.
local function unbuilt_plugin_copy()
  local root = test_dir .. "/unbuilt-copy"
  assert(os.execute("rm -rf " .. shell_quote(root) .. " && mkdir -p " .. shell_quote(root)
    .. " && cp -R " .. shell_quote(repo_root .. "/plugin") .. " " .. shell_quote(repo_root .. "/protocol")
    .. " " .. shell_quote(repo_root .. "/bin") .. " " .. shell_quote(root)) == 0)
  return root
end

test("a discovered root without the writer is named once and starts no process", function()
  local root = unbuilt_plugin_copy()
  local started = 0
  local real_run, real_background = wezterm.run_child_process, wezterm.background_child_process
  wezterm.run_child_process = function() started = started + 1; return false, "", "" end
  wezterm.background_child_process = function() started = started + 1; return true end
  local ok, failure = pcall(function()
    local instance = dofile(root .. "/plugin/init.lua")
    local config = { unix_domains = { { name = "unbuilt", socket_path = "/tmp/attention-unbuilt.sock" } } }
    instance.apply_to_config(config, { auto_poll = false, dir = test_dir, review_key = false })
    local warnings = drain_warnings()
    assert(#warnings == 1 and warnings[1]:find(root .. "/libexec/attention-rs", 1, true)
        and warnings[1]:find("integration_root", 1, true),
      "the missing writer must be named once with the way out: " .. tostring(warnings[1]))
    assert(config.set_environment_variables.WEZTERM_ATTENTION_ROOT == nil,
      "a root without its writer is not exported")
    instance._internal.acquire_tab_source("/test/unbuilt.sock")
    local window = window_double({ tabs = { { { id = 9971, domain = "unbuilt" } } }, focused = false })
    instance.poll(window, { call_after = function() end })
    instance.poll(window, { call_after = function() end })
  end)
  wezterm.run_child_process, wezterm.background_child_process = real_run, real_background
  assert(ok, failure)
  assert(started == 0, "a root with no writer must start no process, started " .. started)
end)

test("an explicit integration root is the one whose command runs", function()
  local argv
  local real_run = wezterm.run_child_process
  wezterm.run_child_process = function(args) argv = args; return true, tab_source_response(args[4]), "" end
  local ok, failure = pcall(function()
    local instance = dofile(repo_root .. "/plugin/init.lua")
    instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
      renderer = "manual", integration_root = writer_root })
    instance._internal.acquire_tab_source("/test/explicit.sock")
    assert(argv and argv[1] == writer_root .. "/bin/attention",
      "tab source must run the integration root's command, ran " .. tostring(argv and argv[1]))
    assert(#drain_warnings() == 0, "a root with its writer is not worth a warning")

    local missing = test_dir .. "/explicit-without-writer"
    local unbuilt = dofile(repo_root .. "/plugin/init.lua")
    unbuilt.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
      renderer = "manual", integration_root = missing })
    local warnings = drain_warnings()
    assert(#warnings == 1 and warnings[1]:find(missing .. "/libexec/attention-rs", 1, true),
      "an explicit root without its writer must be named: " .. tostring(warnings[1]))
  end)
  wezterm.run_child_process = real_run
  assert(ok, failure)
end)

-- A producer reads WEZTERM_ATTENTION_ROOT as "write through this checkout's
-- writer". Exporting it for a checkout whose writer was never built would make
-- every callback die at the shim; without it, a producer says the command is
-- not installed.
test("the v2 root is exported only once the writer it selects is installed", function()
  local root = test_dir .. "/integration-root"
  assert(os.execute("mkdir -p " .. shell_quote(root .. "/bin")) == 0)
  assert(os.execute("mkdir -p " .. shell_quote(root .. "/libexec")) == 0)
  local shim = assert(io.open(root .. "/bin/attention", "w"))
  assert(shim:write("#!/bin/sh\nexit 3\n"))
  assert(shim:close())

  drain_errors()
  local unbuilt = dofile(repo_root .. "/plugin/init.lua")
  local unbuilt_config = {}
  unbuilt.apply_to_config(unbuilt_config, {
    auto_poll = false, dir = test_dir, review_key = false, integration_root = root,
  })
  local unbuilt_env = unbuilt_config.set_environment_variables or {}
  assert(unbuilt_env.WEZTERM_ATTENTION_ROOT == nil,
    "a checkout with no writer must not claim the v2 root")
  assert(unbuilt_env.WEZTERM_ATTENTION_DIR == test_dir,
    "the state directory is exported all the same")
  assert(#drain_errors() == 0,
    "a checkout not yet built is named as a warning, not reported as a fault")

  local writer = assert(io.open(root .. "/libexec/attention-rs", "w"))
  assert(writer:write("#!/bin/sh\nexit 0\n"))
  assert(writer:close())

  local built = dofile(repo_root .. "/plugin/init.lua")
  local built_config = {}
  built.apply_to_config(built_config, {
    auto_poll = false, dir = test_dir, review_key = false, integration_root = root,
  })
  local built_env = built_config.set_environment_variables or {}
  assert(built_env.WEZTERM_ATTENTION_ROOT == root,
    "an installed writer must be selected")
  assert(#drain_errors() == 0, "an installed writer must log nothing")
end)

-- A tab that closes between tabs() and panes() must not abort the whole poll,
-- or every tab after it loses its refresh for that tick. Surviving the race is
-- only half of it: the panes of the tab that vanished are then missing from
-- this tick's inventory, and the absence check retires the cache entries of
-- panes it cannot see. A partial inventory must not drive that.
test("a tab that vanishes mid-poll costs neither the later tabs nor what they show", function()
  write_activity(7701, "stop")
  write_activity(7702, "notify")

  -- One window across all three polls: the absence check compares this tick's panes with
  -- what the same window reported last tick, so a fresh id would have nothing to
  -- compare against and every assertion below would pass vacuously.
  local window = 7700
  local kept, racing = 61, 62
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = kept, panes = { 7701 } }, { tab_id = racing, panes = { 7702 } } } }))
  assert(attention.get_attention(7701) == "stop" and attention.get_attention(7702) == "notify",
    "both panes should be cached before the race")

  write_activity(7701, "notify")
  local raced = window_double({ window_id = window, focused = false,
    tabs = { { tab_id = racing, gone = true }, { tab_id = kept, panes = { 7701 } } } })
  local ok, poll_error = pcall(attention.poll, raced)
  assert(ok, "a tab closing mid-poll must not abort the poll: " .. tostring(poll_error))
  assert(attention.get_attention(7701) == "notify",
    "a tab listed after the vanished one must still be refreshed")
  assert(attention.get_attention(7702) == "notify",
    "a pane absent only because its tab could not be read is not a closed pane")

  -- Protection lasts only while the tab is still listed. Once WezTerm drops it,
  -- the panes it held are absent like any other closed pane -- a tab that keeps
  -- failing cannot hold them in the cache forever.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = kept, panes = { 7701 } } } }))
  assert(attention.get_attention(7702) == nil, "a dropped tab's panes must be retired like any other")
end)

-- WezTerm keeps a window's tabs as objects and drops them from the mux
-- separately, and the pruning that reconciles the two returns early while any
-- background activity is in flight. So a tab can stay listed and unreadable for
-- more than one tick, and a pane can be moved into a tab after its last
-- successful read -- which is why what that tab held before does not bound what
-- it holds now, and why nothing in the window can be called absent until it
-- answers. Progress comes from keeping the question, not from answering it early.
test("an unreadable tab defers retiring its panes, and answering releases it", function()
  write_activity(7801, "stop")
  write_activity(7802, "notify")
  write_activity(7803, "stop")

  local window = 7800
  local kept, racing = 81, 82
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = kept, panes = { 7801, 7803 } }, { tab_id = racing, panes = { 7802 } } } }))
  assert(attention.get_attention(7803) == "stop", "the third pane starts present")

  -- 7803 closes for real while the racing tab is still listed and unreadable.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = racing, gone = true }, { tab_id = kept, panes = { 7801 } } } }))
  assert(attention.get_attention(7802) == "notify", "the unreadable tab's own pane is kept")
  assert(attention.get_attention(7803) == "stop",
    "and so is the closed one: a tab that cannot be read might have gained it")

  -- The obligation is what carries progress, not retiring during the gap. Once
  -- every listed tab answers, the pane that really closed is retired --
  -- including one that closed while nothing could be concluded.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = kept, panes = { 7801 } } } }))
  assert(attention.get_attention(7803) == nil, "a closed pane is retired once the window can be read")
end)

-- The tab list captured at the top of a poll is walked again further down, to
-- find the tab holding the focused pane. Guarding the first walk and not the
-- second leaves the same race, later: it would abort after the cache was
-- rebuilt and before the view callbacks were delivered, so the acknowledgement
-- and the consumers would both be lost for that tick.
test("a vanished tab does not stop the focused pane being acknowledged", function()
  write_activity(7901, "stop")

  local ok
  with_plugin_command(acknowledging_answer, function()
    ok = pcall(poll_focused, {
      window_id = 7900,
      active_pane_id = 7901,
      tabs = { { tab_id = 91, gone = true }, { tab_id = 92, panes = { 7901 } } },
    })
  end)
  assert(ok, "a tab closing must not abort the acknowledgement walk")
  assert(acknowledgement_exists(7901),
    "the focused pane must still be acknowledged when another tab has gone")
end)

-- A consumer is told a scope is lost so it can drop the state it kept for that
-- scope -- a dismissal, a policy. A tab that could not be read this tick has not
-- taken its panes away, and saying so would make the consumer throw that state
-- out and rebuild it as new when the pane comes back, re-demanding attention the
-- user already dismissed.
test("a scope nobody could look at is not a scope that was lost", function()
  local wire = materialize_v2_fixture(13101, string.rep("d", 64))
  local messages, instance = {}, dofile(repo_root .. "/plugin/init.lua")
  local options = { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end }
  instance.apply_to_config({}, { auto_poll = false, dir = test_dir, review_key = false,
    settled_title_fallback = false,
    on_view_change = function(message) messages[#messages + 1] = message.kind end })

  local pane = { id = 13101, domain = "mux", attention = wire }
  local present = window_double({ window_id = 13111, focused = false,
    tabs = { { tab_id = 131, panes = { pane } } } })
  instance.poll(present, options)
  assert(#messages == 1 and messages[1] == "initial", "the scope should arrive once")

  local raced = window_double({ window_id = 13111, focused = false,
    tabs = { { tab_id = 131, gone = true } } })
  assert(pcall(instance.poll, raced, options), "an unreadable tab must not abort delivery")
  assert(#messages == 1,
    "an unreadable tab must report nothing: the scope is unknown, not lost")

  instance.poll(present, options)
  assert(#messages == 1,
    "the scope survived, so its return is not a new one")
end)

-- Acknowledging says the user was shown this marker, so it is refused for a pane
-- this tick did not read: the cache entry the acknowledgement needs is built by
-- that read. A pane in a tab that stopped answering is exactly that case, and it
-- stays unacknowledged until a tick can see it again. Membership is answered
-- from the inventory rather than by walking the tab list a second time, and this
-- pins that the two agree.
test("a pane this tick could not read is not acknowledged", function()
  write_activity(7951, "stop")

  local window, holding = 7950, 95
  with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = window, active_pane_id = 7951,
      tabs = { { tab_id = holding, panes = { 7951 } } } })
  end)
  assert(acknowledgement_exists(7951), "a pane that was read and focused is acknowledged")

  assert(os.remove(seeded_records_root(7951) .. "/ack.json"))
  write_activity(7951, "notify")
  local spawned = with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = window, active_pane_id = 7951,
      tabs = { { tab_id = holding, gone = true } } })
  end)
  assert(#spawned == 0, "an activity the poll never read must not be recorded as seen")

  with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = window, active_pane_id = 7951,
      tabs = { { tab_id = holding, panes = { 7951 } } } })
  end)
  assert(acknowledgement_exists(7951), "the next tick that can read it acknowledges it")
end)
-- Deleting a pane's files needs proof the domain is still attached, because
-- detaching a domain drops all of its panes at once and looks exactly like them
-- closing. A domain remembered from a tab that did not answer is not that proof:
-- it says only that the domain was there once. Keeping the two apart is the
-- whole point -- remembering must prevent a premature deletion without ever
-- authorising one.
test("a remembered domain keeps what a pane showed but cannot retire it", function()
  write_activity(7601, "stop")
  write_activity(7602, "notify")

  local window, racing, sibling = 7600, 61, 62
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = racing, panes = { 7601 } }, { tab_id = sibling, panes = { 7602 } } } }))
  assert(attention.get_attention(7602) == "notify", "both panes start observed")

  -- One tab will not answer; the other answers and holds nothing. No pane is
  -- counted on the domain, so nothing establishes that it is still attached.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = racing, gone = true }, { tab_id = sibling, panes = {} } } }))
  assert(attention.get_attention(7602) == "notify",
    "no pane was counted on the domain, so nothing may be retired for absence")

  -- Every listed tab answers, and the domain is observed. Now the absent pane is
  -- retired -- remembering did not freeze it, it deferred it.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = sibling, panes = { 7601 } } } }))
  assert(attention.get_attention(7602) == nil, "an observed domain in a readable window retires it")
end)

-- Acknowledging records that the user was shown a publication, so it needs a
-- pane this tick actually looked at. The inventory also carries panes nobody
-- could look at, to keep their records from being swept; using that same set for
-- membership would dismiss a notification that was never displayed. An unfocused
-- poll leaves exactly the dangerous state: the marker is cached and eligible,
-- and no acknowledgement has been written yet.
test("a carried pane is not evidence the user saw it", function()
  write_activity(7851, "notify")

  local window, holding = 7850, 85
  local readable = { { tab_id = holding, panes = { 7851 } } }
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = readable, active_pane_id = 7851 }))
  assert(attention.get_attention(7851) == "notify", "the notification is cached and eligible")

  local spawned = with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = window, active_pane_id = 7851,
      tabs = { { tab_id = holding, gone = true } } })
  end)
  assert(#spawned == 0,
    "a pane carried across a tab that would not answer was not shown to anyone")

  with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = window, active_pane_id = 7851, tabs = readable })
  end)
  assert(acknowledgement_exists(7851),
    "a tick that can see the pane acknowledges it, so this is not simply disabled")
end)

-- The poll reads the activity it shows, and the acknowledgement is written
-- later, by the attention command. Anything published in between is something
-- the user has not seen, and dismissing it would retire a notification that
-- was never shown: the command is told the event the poll read, and refuses
-- one that has moved on.
test("an event published between the two reads is not the one dismissed", function()
  local wire = materialize_v2_fixture(73)
  local samples = protocol_fixture.record_samples
  local binding_root_path = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/73"
    .. "/launches/" .. wire.launch_id .. "/bindings/" .. samples.binding.binding_id
  local activity_path = binding_root_path .. "/activity.json"
  local ack_path = binding_root_path .. "/ack.json"

  local ack_before = assert(read_path(ack_path))
  local pane_spec = { id = 9073, domain = "unix", attention = wire }
  local republished = false
  local spawned = with_plugin_command(acknowledging_answer, function()
    attention.poll(window_double({
      tabs = { { pane_spec } }, focused = true, active_pane_id = pane_spec,
      -- is_focused() is asked after the inventory is built and before the
      -- acknowledgement, which is exactly the gap a producer can publish into.
      on_focus_check = function()
        if republished then return end
        republished = true
        local activity = decode_json(assert(read_path(activity_path)))
        activity.event_id = "00000000-0000-4000-8000-000000000073"
        write_json_path(activity_path, activity)
      end,
    }), { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  end)
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find(
      "--activity-event-id " .. samples.activity.event_id, 1, true),
    "the command is told the event the poll read")

  assert(republished, "the test must have published into the gap")
  assert(read_path(ack_path) == ack_before,
    "the event the poll read is gone, so there is nothing this user has seen to dismiss")
  assert(internal.attention_cache[internal.address_cache_key(wire.address)].event_id
      == "00000000-0000-4000-8000-000000000073",
    "the newly published event is taken into the cache, so the next tick can show it")
end)

-- A mux pane that reattaches comes back as a new GUI pane with a fresh local id
-- and no identity variable yet; it publishes one a moment later. Until it does,
-- it cannot be told apart from a replacement for a stored identity this window
-- remembers. Counting it as proof the domain is back, and therefore that the
-- remembered identity is gone, deletes the records of a pane that is about to
-- say it is still here -- and a review flag the user set is not rebuilt by the
-- identity arriving.
test("an unresolved pane on a domain is not proof an identity there is gone", function()
  write_activity(8842, "stop")

  local window = 4200
  local anchor_tab = { tab_id = 421, panes = { { id = 900, domain = "local" } } }
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 422, panes = { { id = 8700, attention = seeded_wire(8842), domain = "mux" } } } } }))
  assert(attention.get_attention(8842) == "stop", "the remote pane is observed through its published identity")

  -- Detached: nothing on that domain is enumerated, so absence cannot be decided.
  attention.poll(window_double({ window_id = window, focused = false, tabs = { anchor_tab } }))
  assert(attention.get_attention(8842) == "stop", "an unobserved domain decides nothing")

  -- Reattached, identity not yet published. The domain is back; the question of
  -- which stored identity this pane carries is not yet answerable.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 422, panes = { { id = 8701, domain = "mux" } } } } }))
  assert(attention.get_attention(8842) == "stop", "a pane that has not said who it is cannot say who it is not")

  -- Every pane on the domain identified, and none of them is 8842.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 422, panes = { { id = 8702, published = 8843, domain = "mux" } } } } }))
  assert(attention.get_attention(8842) == nil, "a fully identified domain that excludes it retires it")
end)

-- The acknowledgement compares the event the poll saw with the one it is about
-- to dismiss. Seeing no event is an answer -- there was nothing to dismiss --
-- and must refuse, not waive the comparison. Otherwise a pane whose activity was
-- already cleared acknowledges whatever gets published a moment later.
test("seeing no event is a reason to refuse, not a reason to skip the check", function()
  local wire = materialize_v2_fixture(74)
  local samples = protocol_fixture.record_samples
  local binding_root_path = test_dir .. "/v2/realms/" .. wire.address.realm_id
    .. "/incarnations/" .. wire.address.incarnation_id .. "/panes/74"
    .. "/launches/" .. wire.launch_id .. "/bindings/" .. samples.binding.binding_id
  local activity_path = binding_root_path .. "/activity.json"
  local ack_path = binding_root_path .. "/ack.json"

  local activity = decode_json(assert(read_path(activity_path)))
  assert(os.remove(activity_path))
  local ack_before = assert(read_path(ack_path))

  local published = false
  attention.poll(window_double({
    tabs = { { { id = 9074, domain = "unix", attention = wire } } }, focused = true,
    active_pane_id = { id = 9074, domain = "unix", attention = wire },
    on_focus_check = function()
      if published then return end
      published = true
      activity.event_id = "00000000-0000-4000-8000-000000000074"
      write_json_path(activity_path, activity)
    end,
  }), { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })

  assert(published, "the test must have published into the gap")
  assert(read_path(ack_path) == ack_before,
    "the poll saw no publication here, so it has no standing to dismiss one")
end)

-- Publication retries are retired when the domain is done. "None of the panes I
-- could read is unpublished" is not that: the tab that could not be read is
-- where the unpublished pane was. Reporting the readable subset as the whole
-- domain cancels the retry the unpublished pane is still waiting for.
test("a domain seen through one readable tab is not a domain reported as published", function()
  local spawned, scheduled = {}, {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv) spawned[#spawned + 1] = argv; return true end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "partial-realm", socket_path = "/tmp/attention-partial.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local options = { call_after = function(delay, callback)
    scheduled[#scheduled + 1] = { delay = delay, callback = callback }
  end }

  local waiting = { tab_id = 1, panes = { { id = 9940, domain = "partial-realm" } } }
  local resolved = { tab_id = 2, panes = { { id = 9941, published = 9941, domain = "partial-realm" } } }
  local both = { window_id = 8160, focused = false, tabs = { waiting, resolved } }
  reloaded.poll(window_double(both), options)
  reloaded.poll(window_double(both), options)
  assert(#spawned == 1 and #scheduled == 1, "the unpublished pane starts one schedule")

  reloaded.poll(window_double({ window_id = 8160, focused = false,
    tabs = { { tab_id = 1, gone = true }, resolved } }), options)
  scheduled[1].callback()
  assert(#spawned == 2, "a partial look at the domain must not retire its retry")
  wezterm.background_child_process = original_background
end)

-- The uncertainty an unidentified pane creates has to survive its tab going
-- quiet. If it lives only in what this tick enumerated, then the tick after --
-- where that tab stops answering but a sibling on the same domain still does --
-- has a present domain, no uncertainty, and deletes the very records the
-- unidentified pane might have turned out to own.
test("identity uncertainty survives the tab that raised it going quiet", function()
  write_activity(8844, "stop")

  local window = 4400
  local anchor_tab = { tab_id = 441, panes = { { id = 910, domain = "local" } } }
  local sibling = { tab_id = 443, panes = { { id = 703, published = 8845, domain = "mux" } } }
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 442, panes = { { id = 8704, attention = seeded_wire(8844), domain = "mux" } } } } }))
  assert(attention.get_attention(8844) == "stop", "observed through its published identity")

  attention.poll(window_double({ window_id = window, focused = false, tabs = { anchor_tab } }))
  assert(attention.get_attention(8844) == "stop", "an unobserved domain decides nothing")

  -- Reattached: one tab holds a pane that has not said who it is, another holds
  -- an identified pane on the same domain.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 442, panes = { { id = 8705, domain = "mux" } } }, sibling } }))
  assert(attention.get_attention(8844) == "stop", "an unresolved pane blocks the conclusion while it is enumerated")

  -- The tab holding it stops answering. The sibling still answers, so the domain
  -- is present -- but nothing has become any more certain about identity 8844.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 442, gone = true }, sibling } }))
  assert(attention.get_attention(8844) == "stop", "a tab going quiet cannot resolve what it had not resolved")

  -- Every pane on the domain identified, none of them 8844.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, sibling } }))
  assert(attention.get_attention(8844) == nil, "a fully identified domain that excludes it retires it")
end)

-- Two windows share one display cache. If the acknowledgement takes its expected
-- publication from that cache rather than from the poll that is acknowledging,
-- another window's poll can slide a newer publication in between this poll's
-- read and its decision -- and the comparison then finds the cache and the disk
-- agreeing about an event this poll never saw.
test("another window's poll cannot decide what this one acknowledges", function()
  local seen = write_activity(46, "notify")

  local watcher = window_double({ window_id = 4602, focused = false,
    tabs = { { tab_id = 461, panes = { 46 } } } })
  local raced = false
  local spawned = with_plugin_command(acknowledging_answer, function()
    poll_focused({ window_id = 4601, active_pane_id = 46,
      tabs = { { tab_id = 461, panes = { 46 } } },
      on_focus_check = function()
        if raced then return end
        raced = true
        -- A newer publication, and another window reads it into the shared cache
        -- without acknowledging it.
        write_activity(46, "notify")
        attention.poll(watcher)
      end,
    })
  end)

  assert(raced, "the test must have raced the focus query")
  assert(#spawned == 1 and plugin_arguments(spawned[1]):find("--activity-event-id " .. seen, 1, true),
    "the command is told the publication this poll read, not the one in the shared cache")
  assert(not acknowledgement_exists(46),
    "this poll read publication one, so it has no standing to dismiss publication two")
end)

-- A tab that fails the first time it is ever read leaves nothing behind to bound
-- it: there is no remembered membership saying which panes or domains it held.
-- Its scope is the whole window, so while it is listed and unread, nothing in
-- this window can be concluded absent -- otherwise weakening one read, from
-- "enumerated with an unidentified pane" to "could not be read at all", would
-- turn a refusal to delete into permission.
test("a tab nobody has ever read bounds nothing, so it settles nothing", function()
  write_activity(48, "stop")

  local window = 4800
  local anchor_tab = { tab_id = 481, panes = { { id = 920, domain = "local" } } }
  local sibling = { tab_id = 483, panes = { { id = 706, published = 49, domain = "mux" } } }
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 482, panes = { { id = 707, attention = seeded_wire(48), domain = "mux" } } } } }))
  assert(attention.get_attention(48) == "stop", "observed through its published identity")

  attention.poll(window_double({ window_id = window, focused = false, tabs = { anchor_tab } }))
  assert(attention.get_attention(48) == "stop", "an unobserved domain decides nothing")

  -- A tab id this window has never read successfully, failing on its first read,
  -- alongside an identified sibling on the domain.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 484, gone = true }, sibling } }))
  assert(attention.get_attention(48) == "stop",
    "an unread tab could be holding it, and nothing says otherwise")

  -- Still unread on the next tick: the answer does not drift with repetition.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 484, gone = true }, sibling } }))
  assert(attention.get_attention(48) == "stop", "repeating an unanswered question does not answer it")

  -- Every listed tab read, none of them holding it.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, sibling } }))
  assert(attention.get_attention(48) == nil, "a fully read window that excludes it retires it")
end)

-- A pane that starts publishing another address is the same physical pane
-- under a new storage key. Its records belong to it and stay. The old key is
-- not thereby still current, though: leaving it in the cache leaves a reading
-- nothing will ever retire, because it is no longer in the inventory the
-- absence check compares against.
test("a pane that changes storage key keeps its records and gives up the old key", function()
  write_activity(50, "stop")

  local window = 5000
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = 501, panes = { { id = 50, domain = "unix", attention = seeded_wire(50) } } } } }))
  assert(attention.get_attention(50) == "stop", "the first identity is cached")

  -- The same GUI pane, now publishing another address.
  local wire = materialize_v2_fixture(5051)
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { { tab_id = 501, panes = { { id = 50, domain = "unix", attention = wire } } } } }),
    { now_unix_ns = protocol_fixture.state_case.now_unix_ns, call_after = function() end })
  assert(activity_exists(50), "the pane is alive, so its records stay")
  assert(attention.get_attention(50) == nil,
    "the old key is not what this pane goes by any more")
end)

-- Uncertainty elsewhere must not erase something this poll established. When the
-- same physical pane is read under a new storage key, that is a fact, and the old
-- key is retired by it. Letting an unrelated unreadable tab downgrade that to
-- "undecided" keeps the old key alive, and the tick after -- once the pane has
-- taken a fresh local id, as a reconnected client pane does -- nothing connects
-- the two any more and its files are deleted while the pane is still running.
-- The two histories below differ only in whether an unrelated tab answers.
test("a proven replacement survives uncertainty about something else", function()
  local function upgrade_under(pane_id, unrelated_answers)
    write_activity(pane_id, "stop")
    local window = 5200 + pane_id
    local anchor_tab = { tab_id = 521, panes = { { id = 930, domain = "local" } } }
    local unrelated = unrelated_answers
      and { tab_id = 522, panes = { { id = 931, domain = "local" } } }
      or { tab_id = 522, gone = true }
    local options = { now_unix_ns = protocol_fixture.state_case.now_unix_ns,
      call_after = function() end }

    attention.poll(window_double({ window_id = window, focused = false,
      tabs = { anchor_tab, { tab_id = 523, panes = { { id = pane_id, domain = "unix",
        attention = seeded_wire(pane_id) } } } } }), options)
    assert(attention.get_attention(pane_id) == "stop", "the pane starts under its first key")

    -- The same GUI pane publishes another address, while the unrelated tab
    -- either answers or does not.
    local wire = materialize_v2_fixture(pane_id + 400)
    attention.poll(window_double({ window_id = window, focused = false,
      tabs = { anchor_tab, unrelated,
        { tab_id = 523, panes = { { id = pane_id, domain = "unix", attention = wire } } } } }),
      options)
    return attention.get_attention(pane_id) == nil
  end

  assert(upgrade_under(52, true), "the old key retires when the unrelated tab answers")
  assert(upgrade_under(54, false),
    "and equally when it does not: the replacement was observed either way")
end)

-- A pane being alive says nothing about which name it answers to now. Only a
-- pane read under a different name retires the old one; a pane that has not said
-- who it is leaves the question open, because retiring the key on liveness alone
-- would drop a reading that is still the right one.
test("a live pane that has not identified itself retires no name", function()
  write_activity(58, "stop")
  local window = 5800
  local anchor_tab = { tab_id = 581, panes = { { id = 950, domain = "local" } } }

  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab,
      { tab_id = 582, panes = { { id = 810, attention = seeded_wire(58), domain = "mux" } } } } }))
  assert(attention.get_attention(58) == "stop", "observed under its published name")

  -- Same GUI pane, still listed, no longer saying who it is.
  attention.poll(window_double({ window_id = window, focused = false,
    tabs = { anchor_tab, { tab_id = 582, panes = { { id = 810, domain = "mux" } } } } }))
  assert(attention.get_attention(58) == "stop",
    "an unidentified pane cannot disown a name, and the reading stands")
end)

-- An identity that could not be read leaves the domain's publication status
-- unknown: "none of the panes I could read is unpublished" is not a claim this
-- tick can make when one of them could not be read at all.
test("an unreadable identity leaves a domain's publication unconcluded", function()
  local spawned, scheduled = {}, {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv) spawned[#spawned + 1] = argv; return true end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "invalid-realm", socket_path = "/tmp/attention-invalid.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local options = { call_after = function(delay, callback)
    scheduled[#scheduled + 1] = { delay = delay, callback = callback }
  end }

  local waiting = { tab_id = 1, panes = { { id = 9950, domain = "invalid-realm" } } }
  local both = { window_id = 8170, focused = false, tabs = { waiting,
    { tab_id = 2, panes = { { id = 9951, published = 9951, domain = "invalid-realm" } } } } }
  reloaded.poll(window_double(both), options)
  reloaded.poll(window_double(both), options)
  assert(#spawned == 1 and #scheduled == 1, "the unpublished pane starts one schedule")

  -- The unpublished pane is replaced by one whose identity will not parse. The
  -- domain is fully enumerated, and still nothing can be said about it.
  reloaded.poll(window_double({ window_id = 8170, focused = false, tabs = {
    { tab_id = 1, panes = { { id = 9952, domain = "invalid-realm", attention = "{ not json" } } },
    { tab_id = 2, panes = { { id = 9951, published = 9951, domain = "invalid-realm" } } },
  } }), options)
  scheduled[1].callback()
  assert(#spawned == 2, "an unreadable identity must not report the domain resolved")
  wezterm.background_child_process = original_background
  drain_errors()
end)

-- A domain whose every tab went quiet still has whatever obligation it had.
-- Dropping it from the window's domains during the gap loses the retry: the tick
-- that could finally settle it has nothing left to settle.
test("a domain nobody could see this tick keeps its retry", function()
  local spawned, scheduled = {}, {}
  local original_background = wezterm.background_child_process
  wezterm.background_child_process = function(argv) spawned[#spawned + 1] = argv; return true end
  local reloaded = dofile(repo_root .. "/plugin/init.lua")
  reloaded.apply_to_config({
    unix_domains = { { name = "quiet-realm", socket_path = "/tmp/attention-quiet.sock" } },
  }, { auto_poll = false, dir = test_dir, review_key = false,
    integration_root = writer_root })
  local options = { call_after = function(delay, callback)
    scheduled[#scheduled + 1] = { delay = delay, callback = callback }
  end }

  local holding = { tab_id = 1, panes = { { id = 9960, domain = "quiet-realm" } } }
  local both = { window_id = 8180, focused = false, tabs = { holding } }
  reloaded.poll(window_double(both), options)
  reloaded.poll(window_double(both), options)
  assert(#spawned == 1 and #scheduled == 1, "the unpublished pane starts one schedule")

  -- Its only tab stops answering. The domain is now in no evidence at all.
  reloaded.poll(window_double({ window_id = 8180, focused = false,
    tabs = { { tab_id = 1, gone = true } } }), options)
  scheduled[1].callback()
  assert(#spawned == 2, "the obligation survives a tick that could not see it")
  wezterm.background_child_process = original_background
end)

-- decide_absence refuses for a pane that is alive but identified nowhere, and
-- this is the case that reaches it. A handle answers with its id while its mux
-- resolution fails, so its domain reads as unknown and lands in a bucket of its
-- own -- leaving the domain the retained entry was recorded on observed and
-- resolved, with every earlier refusal bypassed.
test("a pane that answers only with its id decides nothing about its old name", function()
  local target = 8870
  write_activity(target, "stop")

  attention.poll(window_double({ window_id = 8871, focused = false,
    tabs = { { tab_id = 887, panes = { { id = 8872, attention = seeded_wire(target), domain = "mux" } },  },
             { tab_id = 888, panes = { { id = 8873, published = 8874, domain = "mux" } } } } }))
  assert(attention.get_attention(target) == "stop", "observed under its published name")

  -- Same handle, now resolving to nothing, beside an identified sibling on the
  -- domain the old entry was recorded on. Deciding nothing keeps the reading;
  -- calling it replaced would retire it on the word of a pane that never said
  -- which name replaced it.
  attention.poll(window_double({ window_id = 8871, focused = false,
    tabs = { { tab_id = 887, panes = { { id = 8872, unresolvable = true } } },
             { tab_id = 888, panes = { { id = 8873, published = 8874, domain = "mux" } } } } }))
  assert(attention.get_attention(target) == "stop",
    "and the reading stands, because nothing has taken the name over")
  drain_errors()
end)

os.execute("rm -rf " .. shell_quote(test_dir))

io.write(string.format("%d passed, %d failed\n", passed, failed))
if failed > 0 then os.exit(1) end
