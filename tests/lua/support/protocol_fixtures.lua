-- Fixture interpreter for the Lua protocol specs.
--
-- These functions build adversarial inputs: they apply dotted-path patches to a
-- sample record, repeat strings to breach length bounds, drop fields, and run
-- the declared cases against the production parsers. None of that is needed by a
-- running plugin, and it used to live in `plugin/protocol.lua`, which meant every
-- WezTerm install shipped and loaded a test harness.
--
-- Production validation and eligibility stay in the production module; this file
-- only drives them.

return function(protocol_api)
  local deep_copy = protocol_api.deep_copy
  local parse_wire_value = protocol_api.parse_wire_value
  local parse_wire_json = protocol_api.parse_wire_json
  local parse_v2_record = protocol_api.parse_v2_record
  local parse_v2_record_json = protocol_api.parse_v2_record_json
  local eligible_subagent = protocol_api.eligible_subagent

  local function fixture_set_path(value, dotted, replacement)
    local parts = {}
    for part in dotted:gmatch("[^.]+") do parts[#parts + 1] = part end
    local cursor = value
    for index = 1, #parts - 1 do
      if type(cursor[parts[index]]) ~= "table" then cursor[parts[index]] = {} end
      cursor = cursor[parts[index]]
    end
    cursor[parts[#parts]] = replacement
  end

  local function fixture_remove_path(value, dotted)
    local parts = {}
    for part in dotted:gmatch("[^.]+") do parts[#parts + 1] = part end
    local cursor = value
    for index = 1, #parts - 1 do
      cursor = type(cursor) == "table" and cursor[parts[index]] or nil
      if type(cursor) ~= "table" then return end
    end
    cursor[parts[#parts]] = nil
  end

  local function fixture_case_value(case, fixture)
    local value
    if case.value ~= nil then
      value = deep_copy(case.value)
    elseif case.parser == "wire" then
      value = deep_copy(fixture.wire_sample)
    else
      value = deep_copy(fixture.record_samples[case.sample])
    end
    if type(value) == "table" then
      for dotted, replacement in pairs(case.patch or {}) do
        fixture_set_path(value, dotted, replacement)
      end
      for dotted, repeated in pairs(case.repeat_patch or {}) do
        fixture_set_path(
          value,
          dotted,
          (repeated.prefix or "") .. string.rep(repeated.text, repeated.count)
        )
      end
      for _, dotted in ipairs(case.remove or {}) do fixture_remove_path(value, dotted) end
    end
    return value
  end

  local function parse_fixture_cases(fixture)
    local results = {}
    for _, case in ipairs(fixture.parse_cases or {}) do
      local parsed, parse_diagnostic
      if case.parser == "wire_json" then
        parsed, parse_diagnostic = parse_wire_json(case.raw)
      elseif case.parser == "record_json" then
        parsed, parse_diagnostic = parse_v2_record_json(case.raw)
      else
        local value = fixture_case_value(case, fixture)
        if case.parser == "wire" then
          parsed, parse_diagnostic = parse_wire_value(value)
        else
          parsed, parse_diagnostic = parse_v2_record(value)
        end
      end
      results[#results + 1] = {
        id = case.id,
        actual = parsed and "valid" or (parse_diagnostic and parse_diagnostic.code),
        expected = case.expected,
      }
    end
    return results
  end

  local function fixture_eligibility_cases(fixture)
    local results = {}
    for _, case in ipairs(fixture.eligibility_cases or {}) do
      local presence = deep_copy(fixture.record_samples.subagent_presence)
      for _, field in ipairs({ "written_at_unix_ns", "observed_mono_ns", "status" }) do
        if case[field] ~= nil then presence[field] = case[field] end
      end
      local clear = deep_copy(fixture.record_samples.subagent_clear)
      local floor = deep_copy(fixture.record_samples.subagent_retention_floor)
      if case.clear_mono_ns == false then
        clear = nil
      elseif case.clear_mono_ns ~= nil then
        clear.observed_mono_ns = case.clear_mono_ns
      end
      if case.floor_mono_ns == false then
        floor = nil
      elseif case.floor_mono_ns ~= nil then
        floor.floor_mono_ns = case.floor_mono_ns
      end

      local parsed, parse_diagnostic = parse_v2_record(presence, "subagent_presence")
      local eligible = false
      local eligibility_diagnostic
      if parsed then
        eligible, eligibility_diagnostic = eligible_subagent(
          parsed, clear, floor, type(case.now_unix_ns) == "string" and case.now_unix_ns or nil)
      else
        eligibility_diagnostic = parse_diagnostic
      end
      results[#results + 1] = {
        id = case.id,
        actual = eligible == true,
        diagnostic = eligibility_diagnostic and eligibility_diagnostic.code or nil,
        expected = case.expected == true,
        expected_diagnostic = case.diagnostic,
      }
    end
    return results
  end

  return {
    fixture_set_path = fixture_set_path,
    fixture_remove_path = fixture_remove_path,
    fixture_case_value = fixture_case_value,
    parse_fixture_cases = parse_fixture_cases,
    fixture_eligibility_cases = fixture_eligibility_cases,
  }
end
