-- Fixture interpreter for the Lua protocol specs.
--
-- These functions build adversarial inputs: they apply dotted-path patches to a
-- sample record, repeat strings to breach length bounds, drop fields, and run
-- the declared cases against the production parsers. None of that is needed by a
-- running plugin, and it used to live in `plugin/protocol.lua`, which meant every
-- WezTerm install shipped and loaded a test harness.
--
-- Production validation and counting stay in the production modules; this file
-- only drives them.

return function(protocol_api)
  local deep_copy = protocol_api.deep_copy
  local parse_wire_value = protocol_api.parse_wire_value
  local parse_wire_json = protocol_api.parse_wire_json
  local parse_v2_record = protocol_api.parse_v2_record
  local parse_v2_record_json = protocol_api.parse_v2_record_json

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

  --- A copy of the fixture's sample `name` with the top-level fields of
  --- `patch` replaced.
  local function patched_sample(fixture, name, patch)
    local value = deep_copy(fixture.record_samples[name])
    for field, replacement in pairs(patch or {}) do value[field] = deep_copy(replacement) end
    return value
  end

  local function fixture_ends_binding_cases(fixture)
    local results = {}
    for _, case in ipairs(fixture.ends_binding_cases or {}) do
      results[#results + 1] = {
        id = case.id,
        actual = protocol_api.ends_binding(
          patched_sample(fixture, "binding_end", case["end"]),
          patched_sample(fixture, "binding", case.binding)),
        expected = case.expected,
      }
    end
    return results
  end

  --- Each coverage row through the plugin's own parser, counter and count
  --- text. A row whose set cannot be read now but was read before gets that
  --- earlier set back, as the record reader hands it back.
  local function fixture_children_coverage_cases(fixture)
    local results = {}
    for _, case in ipairs(fixture.children_coverage_cases or {}) do
      local binding = patched_sample(fixture, "binding", case.binding)
      local binding_end, end_problem, end_status
      if case["end"] == "unavailable" then
        end_problem = { code = "probe_unavailable", message = "record could not be read", context = {} }
        end_status = "unavailable"
      elseif case["end"] ~= "absent" then
        binding_end, end_problem = parse_v2_record(patched_sample(fixture, "binding_end", case["end"]), "binding_end")
        end_status = binding_end and "valid" or "invalid"
      end
      local set, problem, status
      if case.children == "absent" then
        status = "missing"
      elseif case.children == "unavailable" then
        problem = { code = "probe_unavailable", message = "record could not be read", context = {} }
        status = "unavailable"
        if case.previous_children then
          set = assert(parse_v2_record(
            patched_sample(fixture, "child_presence_set", case.previous_children), "child_presence_set"))
          status = "cached"
        end
      else
        set, problem = parse_v2_record(
          patched_sample(fixture, "child_presence_set", case.children), "child_presence_set")
        status = set and "valid" or "invalid"
      end
      local facet = protocol_api.children_facet(set, status, problem, binding, binding_end, end_status, end_problem)
      results[#results + 1] = {
        id = case.id,
        actual = {
          count = facet.count, waiting = facet.waiting, coverage = facet.coverage,
          renders = protocol_api.subagent_count_text(facet.count, facet.uncertain),
        },
        expected = case.expected,
        earlier_read = case.previous_children ~= nil,
      }
    end
    return results
  end

  return {
    fixture_set_path = fixture_set_path,
    fixture_remove_path = fixture_remove_path,
    fixture_case_value = fixture_case_value,
    parse_fixture_cases = parse_fixture_cases,
    fixture_ends_binding_cases = fixture_ends_binding_cases,
    fixture_children_coverage_cases = fixture_children_coverage_cases,
  }
end
