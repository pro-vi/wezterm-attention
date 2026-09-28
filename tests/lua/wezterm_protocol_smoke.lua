local wezterm = require("wezterm")

local result_path = assert(os.getenv("WEZTERM_ATTENTION_SMOKE_RESULT"),
  "WEZTERM_ATTENTION_SMOKE_RESULT is required")

local function run()
  local root = assert(os.getenv("WEZTERM_ATTENTION_TEST_ROOT"),
    "WEZTERM_ATTENTION_TEST_ROOT is required")
  package.path = root .. "/?/init.lua;" .. package.path
  package.loaded.plugin = nil
  local attention = require("plugin")
  local internal = assert(attention._internal, "plugin test seams are unavailable")
  -- The fixture interpreter is test code and loads from tests/, not from the
  -- plugin. It drives the production parsers, which still come from internal.
  local fixtures = dofile(root .. "/tests/lua/support/protocol_fixtures.lua")(internal)
  assert(internal.protocol_path == root .. "/protocol/v2.json",
    "Lua module loader path did not resolve the checkout protocol manifest")

local function read_json(path)
  local file = assert(io.open(path, "r"))
  local content = assert(file:read("*a"))
  assert(file:close())
  return wezterm.json_parse(content)
end

  local fixture = read_json(root .. "/tests/fixtures/v2/protocol-cases.json")
  local protocol_api = dofile(root .. "/plugin/protocol.lua")({ wezterm = wezterm, protocol_path = root .. "/protocol/v2.json" })
  local lifecycle_file = assert(io.open(root .. "/tests/fixtures/lifecycle/observations.json", "r"))
  local lifecycle_cases = assert(protocol_api.decode_json(lifecycle_file:read("*a")))
  lifecycle_file:close()
  for _, case in ipairs(lifecycle_cases.cases) do
    local parsed, problem = protocol_api.parse_v2_record(case.value, case.value.kind)
    assert((parsed and "valid" or problem.code) == case.expected, "lifecycle " .. case.id)
  end
  for _, case in ipairs(lifecycle_cases.raw_cases) do
    local parsed, problem = protocol_api.parse_v2_record_json(case.raw, "lifecycle_snapshot")
    assert((parsed and "valid" or problem.code) == case.expected, "lifecycle " .. case.id)
  end
  local lifecycle_dir = os.getenv("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_DIR")
  local parity_path = os.getenv("WEZTERM_ATTENTION_FACTS_PARITY")
  if parity_path then
    local function equal(a,b)
      if type(a)~=type(b) then return false end
      if type(a)~="table" then return a==b end
      for key,value in pairs(a) do if not equal(value,b[key]) then return false end end
      for key in pairs(b) do if a[key]==nil then return false end end
      return true
    end
    for _,case in ipairs(read_json(parity_path)) do
      local actual=internal.lifecycle_facet(case.snapshot,case.snapshot and "valid" or "missing",nil,case.now,
        case.children,case.children and "valid" or "missing",nil)
      assert(equal(actual,case.expected),"Rust/Lua lifecycle projection differs: "..case.id)
    end
  end
  if lifecycle_dir then
    local wire = assert(internal.parse_wire_json(os.getenv("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE")))
    local view = internal.read_attention_view({ address = wire.address, launch_id = wire.launch_id, marker_id = wire.address.pane_id, cache_key = internal.address_cache_key(wire.address) }, "99999999999999999999", { dir = lifecycle_dir })
    assert(view.lifecycle.availability == "available", "real CLI snapshot did not reach reader")
    local publication = os.getenv("WEZTERM_ATTENTION_LIFECYCLE_SCENARIO") == "publication"
    if not publication then
      assert(#view.lifecycle.observations == 1 and view.lifecycle.observations[1].correlation.tool_call_id == "cli-call", "real CLI lost native tool identity")
      assert(view.activity_type == "thinking", "lifecycle changed the legacy badge")
    end
    internal.attention_cache[internal.address_cache_key(wire.address)] = view
    local pane = { get_user_vars = function() return { WEZTERM_ATTENTION = os.getenv("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE") } end }
    local copy = assert(attention.get_attention_view(pane))
    package.loaded.plugin = nil
    local callback_instance = require("plugin")
    local deliveries = {}
    callback_instance.apply_to_config({}, { dir = lifecycle_dir, renderer = "manual", auto_poll = false,
      review_key = false, auto_clear = {}, settled_title_fallback = false,
      on_view_change = function(message) deliveries[#deliveries + 1] = message end })
    pane.pane_id = function() return 91001 end
    pane.get_domain_name = function() return "fixture" end
    pane.get_title = function() error("disabled title fallback sampled a title") end
    local window = { window_id = function() return 91002 end, is_focused = function() return false end,
      mux_window = function() return { tabs = function() return {{panes = function() return {pane} end}} end } end }
    local poll_options = { now_unix_ns = "99999999999999999999", call_after = function() end, gui_windows = {window} }
    callback_instance.poll(window, poll_options)
    callback_instance.poll(window, poll_options)
    assert(#deliveries == 1 and deliveries[1].kind == "initial" and deliveries[1].window_id == 91002,
      "installed Lua callback did not preserve initial/unchanged semantics")
    assert(deliveries[1].scope.launch_id == wire.launch_id and deliveries[1].view.lifecycle.availability == "available")
    if publication then
      local module = dofile(root .. "/examples/follow-up.lua")
      local consumer, second = module.new(), module.new()
      assert(copy.activity_type == "stop" and consumer.appearance(copy) == "follow_up", "publication remains usable after Stop")
      consumer.dismiss()
      assert(consumer.appearance(attention.get_attention_view(pane)) == "base")
      assert(second.appearance(attention.get_attention_view(pane)) == "follow_up", "dismissal cannot modify another consumer")
    else
      copy.lifecycle.observations[1].correlation.tool_call_id = "mutated"
      assert(attention.get_attention_view(pane).lifecycle.observations[1].correlation.tool_call_id == "cli-call", "getter shared nested lifecycle state")
    end
  end
  local parse_results = fixtures.parse_fixture_cases(fixture)
assert(#parse_results == #fixture.parse_cases, "not every protocol parse row ran")
for _, result in ipairs(parse_results) do
  assert(result.actual == result.expected,
    result.id .. " expected " .. tostring(result.expected) .. ", got " .. tostring(result.actual))
end

  local end_results = fixtures.fixture_ends_binding_cases(fixture)
  assert(#end_results == #fixture.ends_binding_cases and #end_results > 0,
    "not every binding end row ran")
  for _, result in ipairs(end_results) do
    assert(result.actual == result.expected, result.id .. " binding end expected "
      .. tostring(result.expected) .. ", got " .. tostring(result.actual))
  end

  -- Rust's inspect answers for the rows it can read, keyed by row id. A row
  -- with an earlier read is the plugin's alone: inspect reads once.
  local children_parity_path = os.getenv("WEZTERM_ATTENTION_CHILDREN_PARITY")
  local rust_children = children_parity_path and read_json(children_parity_path) or nil
  local coverage_results = fixtures.fixture_children_coverage_cases(fixture)
  assert(#coverage_results == #fixture.children_coverage_cases and #coverage_results > 0,
    "not every child coverage row ran")
  local compared = 0
  for _, result in ipairs(coverage_results) do
    for _, field in ipairs({ "count", "waiting", "coverage", "renders" }) do
      assert(result.actual[field] == result.expected[field], result.id .. " " .. field .. " expected "
        .. tostring(result.expected[field]) .. ", got " .. tostring(result.actual[field]))
    end
    if rust_children then
      local rust = rust_children[result.id]
      assert((rust == nil) == result.earlier_read, result.id .. ": Rust read the wrong rows")
      if rust then
        compared = compared + 1
        for _, field in ipairs({ "count", "waiting", "coverage" }) do
          assert(rust[field] == result.actual[field], "Rust/Lua child " .. field .. " differs: " .. result.id)
        end
      end
    end
  end

  local now = wezterm.time.now()
  local seconds = now:format_utc("%s")
  local fractional = now:format_utc("%9f")
  local joined = now:format_utc("%s%9f")
assert(seconds:match("^%d+$"), "UTC epoch seconds are not decimal")
assert(fractional:match("^%d%d%d%d%d%d%d%d%d$"),
  "UTC fractional part is not exactly nine decimal digits")
assert(joined == seconds .. fractional, "UTC %s%9f did not preserve seconds plus nanoseconds")
  local padded = internal.format_unix_ns20(joined)
assert(padded and #padded == 20 and padded:match("^%d+$"),
  "UTC value did not left-pad to UnixNs20")

  -- A GUI draws a pane by its own number only once it knows the pane is in a
  -- local domain, and a config evaluated outside a GUI has no mux to ask, so
  -- the pane is walked by a poll first. The poll reads a directory that does
  -- not exist, so it can neither see nor change any real state.
  local unused_dir = os.tmpname()
  os.remove(unused_dir)
  local local_pane = { pane_id = function() return 9001 end, get_domain_name = function() return "local" end,
    get_user_vars = function() return {} end, get_title = function() return "pane" end }
  local local_window = { window_id = function() return 91003 end, is_focused = function() return false end,
    mux_window = function() return { tabs = function() return {{panes = function() return {local_pane} end}} end } end }
  attention.poll(local_window, { dir = unused_dir, gui_windows = { local_window }, call_after = function() end })
  internal.attention_cache["9001"] = { type = nil, subagents = 2 }
  local visible = internal.resolve_visible_attention({ "9001" }, {
    colors = { stop = "SENTINEL" },
  })
assert(visible.indicator == "+2 " and visible.type == nil and visible.color == nil,
  "installed formatter gave count-only state a marker tint")
  local tab = {
    tab_index = 0,
    active_pane = { title = "pane", current_working_dir = nil },
    panes = { { pane_id = 9001 } },
  }
  local manual_context
  local rendered = attention.wrap_title_formatter(function(_, context)
    manual_context = context
    return "manual"
  end)(tab, {}, {}, { show_tab_index_in_tab_bar = false }, false, 80)
assert(type(rendered) == "string" and rendered:find("+2 manual", 1, true),
  "installed manual formatter did not render the neutral count")
assert(manual_context.attention.indicator == manual_context.attention[1]
    and manual_context.attention.color == nil,
  "installed formatter context lost named/positional parity")
  internal.attention_cache["9001"] = { type = nil, subagents = 0, subagents_uncertain = true }
  assert(internal.resolve_visible_attention({ "9001" }).indicator == "+? ",
    "installed formatter did not say that a count is unknown")
  internal.attention_cache["9001"] = nil

  for index = 1, 20 do
    local key = "live-smoke:" .. index
    assert(internal.sample_settled_title(key, "launch-a", "title-" .. index, "codex") == nil,
      "first title sample settled early")
    assert(internal.sample_settled_title(key, "launch-a", "title-" .. index, "codex") == "title-" .. index,
      "second equal title sample did not settle")
  end

  return string.format(
    "wezterm-attention protocol/formatter smoke: %d parse rows, %d binding end rows, "
      .. "%d child coverage rows (%d compared with Rust), UTC %d+9 digits",
    #parse_results, #end_results, #coverage_results, compared, #seconds)
end

local passed, result = xpcall(run, function(error_value) return tostring(error_value) end)
local result_file = assert(io.open(result_path, "w"))
assert(result_file:write((passed and "ok - " or "not ok - ") .. tostring(result) .. "\n"))
assert(result_file:close())

return {}
