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
  assert(internal.protocol_path == root .. "/protocol/v2.json",
    "Lua module loader path did not resolve the checkout protocol manifest")

local function read_json(path)
  local file = assert(io.open(path, "r"))
  local content = assert(file:read("*a"))
  assert(file:close())
  return wezterm.json_parse(content)
end

  local fixture = read_json(root .. "/tests/fixtures/v2/protocol-cases.json")
  local parse_results = internal.parse_fixture_cases(fixture)
assert(#parse_results == #fixture.parse_cases, "not every protocol parse row ran")
for _, result in ipairs(parse_results) do
  assert(result.actual == result.expected,
    result.id .. " expected " .. tostring(result.expected) .. ", got " .. tostring(result.actual))
end

  local eligibility_results = internal.fixture_eligibility_cases(fixture)
assert(#eligibility_results == #fixture.eligibility_cases,
  "not every protocol eligibility row ran")
for _, result in ipairs(eligibility_results) do
  assert(result.actual == result.expected,
    result.id .. " eligibility expected " .. tostring(result.expected)
      .. ", got " .. tostring(result.actual))
  assert(result.diagnostic == result.expected_diagnostic,
    result.id .. " diagnostic expected " .. tostring(result.expected_diagnostic)
      .. ", got " .. tostring(result.diagnostic))
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

  internal.attention_cache["9001"] = { type = nil, subagents = 2, puppet = false }
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
  internal.attention_cache["9001"] = nil

  for index = 1, 20 do
    local key = "live-smoke:" .. index
    assert(internal.sample_settled_title(key, "launch-a", "title-" .. index, "codex") == nil,
      "first title sample settled early")
    assert(internal.sample_settled_title(key, "launch-a", "title-" .. index, "codex") == "title-" .. index,
      "second equal title sample did not settle")
  end

  return string.format(
    "wezterm-attention protocol/formatter smoke: %d parse rows, %d eligibility rows, UTC %d+9 digits",
    #parse_results, #eligibility_results, #seconds)
end

local passed, result = xpcall(run, function(error_value) return tostring(error_value) end)
local result_file = assert(io.open(result_path, "w"))
assert(result_file:write((passed and "ok - " or "not ok - ") .. tostring(result) .. "\n"))
assert(result_file:close())

return {}
