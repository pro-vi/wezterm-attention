return function(context)
  local wezterm = context.wezterm
  local protocol_path = context.protocol_path
  local M = context.M
  local defaults = context.defaults

  local container_kinds = setmetatable({}, { __mode = "k" })
  local function read_all(path, maximum)
    local file, open_err = io.open(path, "r")
    if not file then return nil, open_err end
    local content, read_err = file:read(maximum and maximum + 1 or "*a")
    if maximum and content == nil and read_err == nil then content = "" end
    local closed, close_err = file:close()
    if not content then return nil, read_err end
    if not closed then return nil, close_err end
    if maximum and #content > maximum then return nil, "lifecycle record exceeds its bound", "invalid" end
    return content
  end

  local function decode_json(content)
    local ok, value = pcall(wezterm.json_parse, content)
    if not ok then return nil, value end
    -- Preserve raw container kinds, including empty [] versus {}, which some
    -- WezTerm JSON decoders otherwise collapse to the same Lua table.
    local cursor = 1
    local function whitespace() cursor = content:find("%S", cursor) or (#content + 1) end
    local function quoted()
      local start = cursor
      cursor = cursor + 1
      while cursor <= #content do
        local char = content:sub(cursor, cursor)
        cursor = cursor + 1
        if char == "\\" then cursor = cursor + 1 elseif char == '"' then break end
      end
      return wezterm.json_parse(content:sub(start, cursor - 1))
    end
    local walk
    walk = function(node)
      whitespace()
      local char = content:sub(cursor, cursor)
      if char == '"' then quoted(); return end
      if char ~= "{" and char ~= "[" then
        cursor = content:find("[,}%]%s]", cursor) or (#content + 1)
        return
      end
      if type(node) == "table" then container_kinds[node] = char end
      cursor = cursor + 1
      whitespace()
      local index, closing = 1, char == "{" and "}" or "]"
      while content:sub(cursor, cursor) ~= closing do
        local key = index
        if char == "{" then key = quoted(); whitespace(); cursor = cursor + 1 end
        walk(type(node) == "table" and node[key] or nil)
        whitespace()
        if content:sub(cursor, cursor) ~= "," then break end
        cursor = cursor + 1; index = index + 1; whitespace()
      end
      cursor = cursor + 1
    end
    local shaped = pcall(walk, value)
    if not shaped then return nil, "JSON container shape unavailable" end
    return value
  end

  local protocol
  local protocol_load_error
  if protocol_path then
    local raw, read_err = read_all(protocol_path)
    if raw then
      local parsed, parse_err = decode_json(raw)
      if type(parsed) == "table"
          and type(parsed.limits) == "table"
          and type(parsed.enums) == "table"
          and type(parsed.records) == "table"
          and type(parsed.wire) == "table"
          and type(parsed.digests) == "table"
          and parsed.digests.algorithm == "sha256"
          and parsed.digests.encoding == "lowercase_hex"
          and type(parsed.limits.canonical_decimal_max_digits) == "number" then
        protocol = parsed
      else
        protocol_load_error = parse_err or "manifest is not a protocol object"
      end
    else
      protocol_load_error = read_err or "manifest could not be read"
    end
  else
    protocol_load_error = "plugin root could not be derived"
  end

  local function diagnostic(code, message, context)
    return {
      code = code,
      message = message,
      context = context or {},
      help = "attention doctor",
    }
  end

  local function invalid(message, context)
    return diagnostic("record_invalid", message, context)
  end

  local function is_integer(value)
    return type(value) == "number" and value == math.floor(value)
  end

  local function is_hex64(value)
    return type(value) == "string" and #value == 64 and not value:find("[^0-9a-f]")
  end

  local function is_uuid(value)
    return type(value) == "string"
      and #value == 36
      and not value:find("[A-F]")
      and value:match("^[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+$") ~= nil
      and #value:match("^([0-9a-f]+)") == 8
      and #value:match("^[0-9a-f]+%-([0-9a-f]+)") == 4
      and #value:match("^[0-9a-f]+%-[0-9a-f]+%-([0-9a-f]+)") == 4
      and #value:match("^[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-([0-9a-f]+)") == 4
      and #value:match("([0-9a-f]+)$") == 12
  end

  local function is_ns20(value)
    return type(value) == "string" and #value == 20 and not value:find("[^0-9]")
  end

  local function is_canonical_decimal(value, max_digits)
    if type(value) ~= "string" or not value:match("^%d+$") then return false end
    if #value > 1 and value:sub(1, 1) == "0" then return false end
    return not max_digits or #value <= max_digits
  end

  local function is_safe_text(value, max_bytes)
    return type(value) == "string"
      and #value > 0
      and #value <= max_bytes
      and not value:find("[%z\1-\31\127]")
  end

  local U32 = 4294967296

  local function u32(value)
    return value % U32
  end

  local function portable_bit_pair(left, right, want_xor)
    left, right = u32(left), u32(right)
    local result, place = 0, 1
    for _ = 1, 32 do
      local left_bit, right_bit = left % 2, right % 2
      if want_xor and left_bit ~= right_bit
          or not want_xor and left_bit == 1 and right_bit == 1 then
        result = result + place
      end
      left = (left - left_bit) / 2
      right = (right - right_bit) / 2
      place = place * 2
    end
    return result
  end


  local function load_bit_operators()
    local ok, bit = pcall(require, "bit")
    if ok and type(bit) == "table" then
      return {
        band = function(a, b) return u32(bit.band(a, b)) end,
        bxor = function(a, b) return u32(bit.bxor(a, b)) end,
        bnot = function(a) return u32(bit.bnot(a)) end,
        rshift = function(a, n) return u32(bit.rshift(a, n)) end,
        ror = function(a, n) return u32(bit.ror(a, n)) end,
      }
    end

    local loader = rawget(_G, "load") or rawget(_G, "loadstring")
    if loader then
      local chunk = loader([[
        return {
          band = function(a, b) return (a & b) & 0xffffffff end,
          bxor = function(a, b) return (a ~ b) & 0xffffffff end,
          bnot = function(a) return (~a) & 0xffffffff end,
          rshift = function(a, n) return (a >> n) & 0xffffffff end,
          ror = function(a, n)
            return ((a >> n) | ((a << (32 - n)) & 0xffffffff)) & 0xffffffff
          end,
        }
      ]])
      if chunk then
        local loaded, native = pcall(chunk)
        if loaded and type(native) == "table" then return native end
      end
    end

    return {
      band = function(a, b) return portable_bit_pair(a, b, false) end,
      bxor = function(a, b) return portable_bit_pair(a, b, true) end,
      bnot = function(a) return 4294967295 - u32(a) end,
      rshift = function(a, n) return math.floor(u32(a) / 2 ^ n) end,
      ror = function(a, n)
        local value = u32(a)
        return u32(math.floor(value / 2 ^ n) + (value % 2 ^ n) * 2 ^ (32 - n))
      end,
    }
  end

  local bit_operators = load_bit_operators()

  local function bit_and(left, right, ...)
    local result = bit_operators.band(left, right)
    if select("#", ...) > 0 then return bit_and(result, ...) end
    return u32(result)
  end

  local function bit_xor(left, right, ...)
    local result = bit_operators.bxor(left, right)
    if select("#", ...) > 0 then return bit_xor(result, ...) end
    return u32(result)
  end

  local function sha256(message)
    if type(message) ~= "string" then return nil end
    local constants = {
      0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
      0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
      0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
      0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
      0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
      0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
      0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
      0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
      0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
      0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
      0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    }
    local hashes = {
      0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
      0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    }
    local bytes = { message:byte(1, #message) }
    local bit_length = #bytes * 8
    bytes[#bytes + 1] = 0x80
    while #bytes % 64 ~= 56 do bytes[#bytes + 1] = 0 end
    for shift = 56, 32, -8 do bytes[#bytes + 1] = 0 end
    for shift = 24, 0, -8 do
      bytes[#bytes + 1] = math.floor(bit_length / 2 ^ shift) % 256
    end

    for offset = 1, #bytes, 64 do
      local words = {}
      for index = 0, 15 do
        local start = offset + index * 4
        words[index + 1] = bytes[start] * 0x1000000
          + bytes[start + 1] * 0x10000 + bytes[start + 2] * 0x100 + bytes[start + 3]
      end
      for index = 17, 64 do
        local word15, word2 = words[index - 15], words[index - 2]
        local sigma0 = bit_xor(bit_operators.ror(word15, 7),
          bit_operators.ror(word15, 18), bit_operators.rshift(word15, 3))
        local sigma1 = bit_xor(bit_operators.ror(word2, 17),
          bit_operators.ror(word2, 19), bit_operators.rshift(word2, 10))
        words[index] = u32(words[index - 16] + sigma0 + words[index - 7] + sigma1)
      end

      local a, b, c, d = hashes[1], hashes[2], hashes[3], hashes[4]
      local e, f, g, h = hashes[5], hashes[6], hashes[7], hashes[8]
      for index = 1, 64 do
        local sum1 = bit_xor(bit_operators.ror(e, 6),
          bit_operators.ror(e, 11), bit_operators.ror(e, 25))
        local choice = bit_xor(bit_and(e, f), bit_and(bit_operators.bnot(e), g))
        local temp1 = u32(h + sum1 + choice + constants[index] + words[index])
        local sum0 = bit_xor(bit_operators.ror(a, 2),
          bit_operators.ror(a, 13), bit_operators.ror(a, 22))
        local majority = bit_xor(bit_and(a, b), bit_and(a, c), bit_and(b, c))
        local temp2 = u32(sum0 + majority)
        h, g, f, e, d, c, b, a = g, f, e, u32(d + temp1), c, b, a, u32(temp1 + temp2)
      end
      hashes[1], hashes[2], hashes[3], hashes[4] =
        u32(hashes[1] + a), u32(hashes[2] + b), u32(hashes[3] + c), u32(hashes[4] + d)
      hashes[5], hashes[6], hashes[7], hashes[8] =
        u32(hashes[5] + e), u32(hashes[6] + f), u32(hashes[7] + g), u32(hashes[8] + h)
    end

    local result = {}
    for index = 1, 8 do result[index] = string.format("%08x", u32(hashes[index])) end
    return table.concat(result)
  end

  local function json_contains_null_literal(content)
    local in_string, escaped = false, false
    local index = 1
    while index <= #content do
      local char = content:sub(index, index)
      if in_string then
        if escaped then
          escaped = false
        elseif char == "\\" then
          escaped = true
        elseif char == '"' then
          in_string = false
        end
        index = index + 1
      elseif char == '"' then
        in_string = true
        index = index + 1
      elseif content:sub(index, index + 3) == "null" then
        return true
      else
        index = index + 1
      end
    end
    return false
  end

  local function list_contains(values, wanted)
    if type(values) ~= "table" then return false end
    for _, value in ipairs(values) do
      if value == wanted then return true end
    end
    return false
  end

  local function exact_fields(value, required, optional)
    if type(value) ~= "table" then return false, "value must be an object" end
    local allowed = {}
    for _, field in ipairs(required or {}) do
      allowed[field] = true
      if value[field] == nil then return false, "missing required field " .. field end
    end
    for _, field in ipairs(optional or {}) do allowed[field] = true end
    for field in pairs(value) do
      if not allowed[field] then return false, "unknown field " .. tostring(field) end
    end
    return true
  end

  local function validate_pane_address(value)
    local ok, err = exact_fields(value,
      { "realm_id", "incarnation_id", "pane_id" }, {})
    if not ok then return nil, err end
    if not is_hex64(value.realm_id) then return nil, "invalid realm_id" end
    if not is_hex64(value.incarnation_id) then return nil, "invalid incarnation_id" end
    local max_digits = protocol and protocol.limits.pane_id_max_digits or 20
    if not is_canonical_decimal(value.pane_id, max_digits) then return nil, "invalid pane_id" end
    return value
  end

  local function same_address(left, right)
    return left and right
      and left.realm_id == right.realm_id
      and left.incarnation_id == right.incarnation_id
      and left.pane_id == right.pane_id
  end

  local function validate_activity_target(value)
    if type(value) ~= "table" then return nil, "invalid activity target" end
    if value.kind == "launch" then
      local ok, err = exact_fields(value, { "kind" }, {})
      if not ok then return nil, err end
      return value
    end
    if value.kind == "binding" then
      local ok, err = exact_fields(value, { "kind", "binding_id" }, {})
      if not ok then return nil, err end
      if not is_hex64(value.binding_id) then return nil, "invalid target binding_id" end
      return value
    end
    return nil, "invalid activity target kind"
  end

  local validate_shape
  local function validate_field(field_type, value, expected_kind)
    local limits = protocol.limits
    if field_type == "lifecycle_pools" or field_type == "observation_pool" or field_type == "native_correlation" then
      local name = field_type == "lifecycle_pools" and "pools" or (field_type == "observation_pool" and "pool" or "correlation")
      return validate_shape(value, protocol.lifecycle_shapes[name]) ~= nil
    elseif field_type == "lifecycle_actor" then
      return type(value) == "table" and (value.kind == "lead" or value.kind == "child")
        and validate_shape(value, protocol.lifecycle_shapes[value.kind], value.kind) ~= nil
    elseif field_type == "observation_array" then
      if type(value) ~= "table" or container_kinds[value] == "{" or #value > limits.lifecycle_pool_max_count then return false end
      for key in pairs(value) do if type(key) ~= "number" or key % 1 ~= 0 or key < 1 or key > #value then return false end end
      container_kinds[value] = "["
      for _, item in ipairs(value) do
        local spec = type(item) == "table" and protocol.lifecycle_variants[item.kind]
        if not spec or not validate_shape(item, spec, item.kind) then return false end
      end
      return true
    elseif protocol.lifecycle_enums and protocol.lifecycle_enums[field_type] then
      return list_contains(protocol.lifecycle_enums[field_type], value)
    end
    if field_type == "record_kind" then
      return value == expected_kind
    elseif field_type == "record_schema" then
      return value == protocol.record_schema
    elseif field_type == "wire_version" then
      return value == protocol.wire_version
    elseif field_type == "decimal_ns20" or field_type == "monotonic_ns20"
        or field_type == "unix_ns20" then
      return is_ns20(value)
    elseif field_type == "hex64" then
      return is_hex64(value)
    elseif field_type == "uuid" then
      return is_uuid(value)
    elseif field_type == "canonical_decimal" then
      return is_canonical_decimal(value, limits.canonical_decimal_max_digits)
    elseif field_type == "pane_address" then
      return validate_pane_address(value) ~= nil
    elseif field_type == "activity_target" then
      return validate_activity_target(value) ~= nil
    elseif field_type == "absolute_path" then
      return is_safe_text(value, limits.path_max_bytes) and value:sub(1, 1) == "/"
    elseif field_type == "safe_label" then
      return is_safe_text(value, limits.safe_label_max_bytes)
    elseif field_type == "model" then
      return is_safe_text(value, limits.model_max_bytes)
    elseif field_type == "writer_version" then
      return is_safe_text(value, limits.writer_version_max_bytes)
    elseif field_type == "boolean" then
      return type(value) == "boolean"
    elseif field_type == "nonnegative_integer" then
      return is_integer(value) and value >= 0 and value <= limits.frame_max
    elseif field_type == "positive_integer" then
      return is_integer(value) and value > 0 and value <= limits.ttl_ms_max
    elseif field_type == "subagent_ttl_ms" then
      return value == limits.subagent_ttl_ms
    elseif field_type == "provider" then
      return list_contains(protocol.enums.providers, value)
    elseif field_type == "subagent_provider" then
      return list_contains(protocol.enums.subagent_providers, value)
    elseif field_type == "subagent_status" then
      return list_contains(protocol.enums.subagent_statuses, value)
    elseif field_type == "activity_type" then
      return list_contains(protocol.enums.activity_types, value)
    elseif field_type == "end_reason" then
      return list_contains(protocol.enums.end_reasons, value)
    end
    return false
  end

  validate_shape = function(value, spec, expected_kind)
    if type(value) == "table" and container_kinds[value] == "[" then return nil, "array is not an object" end
    local ok, err = exact_fields(value, spec.required, spec.optional)
    if not ok then return nil, err end
    for field, field_value in pairs(value) do
      local field_type = spec.types and spec.types[field]
      if not field_type or not validate_field(field_type, field_value, expected_kind) then
        return nil, "invalid " .. tostring(field)
      end
    end
    return value
  end

  local function compact_size(value)
    local kind = type(value)
    if kind == "string" then return #value + 2 + select(2, value:gsub('[\\"]', "")) end
    if kind == "boolean" then return value and 4 or 5 end
    if kind == "number" then return #string.format("%.0f", value) end
    if kind ~= "table" then return math.huge end
    local bytes, count = 2, 0
    for key, item in pairs(value) do
      bytes = bytes + compact_size(item)
      if container_kinds[value] ~= "[" then bytes = bytes + compact_size(key) + 1 end
      count = count + 1
    end
    return bytes + math.max(0, count - 1)
  end

  local function observation_pool(item)
    if item.kind == "tool_preflight" or item.kind == "tool_result" then return item.tool_class == "generic" and "general" or "requests" end
    if item.kind == "approval_requested" or item.kind == "automatic_denial" or item.kind == "elicitation_requested" or item.kind == "elicitation_action_selected" or item.kind == "notice" then return "requests" end
    return "general"
  end

  local function observation_key(item)
    local c = item.correlation or {}
    local namespace, id
    if item.kind == "tool_preflight" or item.kind == "tool_result" or item.kind == "approval_requested" or item.kind == "automatic_denial" then namespace, id = "tool_call_id", c.tool_call_id
    elseif item.kind == "elicitation_requested" or item.kind == "elicitation_action_selected" then namespace, id = "elicitation_id", c.elicitation_id
    elseif c.message_id then namespace, id = "message_id", c.message_id else namespace, id = "turn_id", c.turn_id end
    if not id then namespace = "observation_id" end
    -- Length prefixes prevent safe-label separators from merging identities.
    local parts = {}
    for _, text in ipairs({ item.kind, item.actor.kind, item.actor.agent_id or "", namespace or "", id or item.observation_id, c.turn_id or "", c.mcp_server_name or "" }) do parts[#parts + 1] = #text .. ":" .. text end
    return table.concat(parts)
  end

  local function classify_lifecycle_tool(provider, tool_name)
    if (provider == "claude" and tool_name == "AskUserQuestion") or (provider == "codex" and tool_name == "request_user_input") then return "question", "blocking" end
    if provider == "codex" and tool_name == "request_user_input_async" then return "question", "nonblocking" end
    if provider == "codex" and tool_name == "request_permissions" then return "permission", nil end
    return "generic", nil
  end

  local function validate_lifecycle(value)
    local limits, keys, ids = protocol.limits, {}, {}
    for name, pool in pairs(value.pools) do
      if #pool.observations > limits.lifecycle_pool_max_count or compact_size(pool) > limits.lifecycle_pool_max_bytes then return false end
      local prior
      for _, item in ipairs(pool.observations) do
        local allowed_sources = protocol.lifecycle_sources[value.provider] and protocol.lifecycle_sources[value.provider][item.kind]
        if not list_contains(allowed_sources, item.source_event) then return false end
        if item.correlation and item.correlation.mcp_server_name and item.kind ~= "elicitation_requested" and item.kind ~= "elicitation_action_selected" then return false end
        local key, order = observation_key(item), item.observed_mono_ns .. item.observation_id
        if keys[key] or ids[item.observation_id] or (prior and prior > order) or observation_pool(item) ~= name
          or compact_size(item) > limits.lifecycle_observation_max_bytes
          or (pool.retention_floor_mono_ns and item.observed_mono_ns <= pool.retention_floor_mono_ns) then return false end
        keys[key], ids[item.observation_id], prior = true, true, order
        if item.correlation and item.correlation.elicitation_id and not item.correlation.mcp_server_name then return false end
        if item.actor.kind == "child" and (value.provider == "pi" or sha256(item.actor.agent_id) ~= item.actor.agent_key) then return false end
        if item.kind == "tool_preflight" or item.kind == "tool_result" then
          local class, mode = classify_lifecycle_tool(value.provider, item.tool_name)
          if item.tool_class ~= class or item.question_mode ~= mode then return false end
          if mode == "nonblocking" then
            if item.actor.kind ~= "lead" then return false end
            if item.kind == "tool_result" and (item.source_event ~= "PostToolUse" or item.result_surface ~= "post_hook" or item.is_error == true or item.interrupted == true) then return false end
          end
        end
      end
    end
    local bytes = compact_size(value) + 1
    return bytes <= limits.lifecycle_max_json_bytes and bytes - compact_size(value.pools.requests) - compact_size(value.pools.general) <= limits.lifecycle_envelope_max_bytes
  end

  local function parse_wire_value(value)
    if not protocol then
      return nil, diagnostic("probe_unavailable", "v2 protocol manifest is unavailable", {
        detail = tostring(protocol_load_error),
      })
    end
    if type(value) == "table" and is_integer(value.wire)
        and value.wire > protocol.wire_version then
      return nil, diagnostic("future_schema", "WEZTERM_ATTENTION uses a future wire version")
    end
    local parsed, err = validate_shape(value, protocol.wire)
    if not parsed then return nil, invalid("invalid WEZTERM_ATTENTION: " .. tostring(err)) end
    return parsed
  end

  local function parse_wire_json(content)
    if type(content) ~= "string" or content == "" then
      return nil, invalid("WEZTERM_ATTENTION must be non-empty JSON")
    end
    if json_contains_null_literal(content) then
      return nil, invalid("WEZTERM_ATTENTION contains unsupported null")
    end
    local value, parse_err = decode_json(content)
    if value == nil then
      return nil, invalid("WEZTERM_ATTENTION is not valid JSON", { detail = tostring(parse_err) })
    end
    return parse_wire_value(value)
  end

  local function parse_v2_record(value, expected_kind)
    if not protocol then
      return nil, diagnostic("probe_unavailable", "v2 protocol manifest is unavailable", {
        detail = tostring(protocol_load_error),
      })
    end
    if type(value) ~= "table" then return nil, invalid("record must be an object") end
    if is_integer(value.schema) and value.schema > protocol.record_schema then
      return nil, diagnostic("future_schema", "record uses a future schema", {
        kind = tostring(value.kind), schema = value.schema,
      })
    end
    if expected_kind and value.kind ~= expected_kind then
      return nil, invalid("record kind does not match its path", {
        expected = expected_kind, actual = tostring(value.kind),
      })
    end
    local kind = expected_kind or value.kind
    local spec = type(kind) == "string" and protocol.records[kind] or nil
    if not spec then return nil, invalid("unknown record kind") end
    local parsed, err = validate_shape(value, spec, kind)
    if not parsed then return nil, invalid("invalid " .. kind .. " record: " .. tostring(err)) end
    if kind == "subagent_presence" and sha256(parsed.agent_id) ~= parsed.agent_key then
      return nil, invalid("subagent_presence agent_key does not match agent_id")
    end
    if kind == "review" and sha256(parsed.owner_id) ~= parsed.owner_key then
      return nil, invalid("review owner_key does not match owner_id")
    end
    if kind == "lifecycle_snapshot" and not validate_lifecycle(parsed) then return nil, invalid("lifecycle snapshot violates its contract") end
    return parsed
  end

  local function lifecycle_raw_valid(content)
    if #content > protocol.limits.lifecycle_max_json_bytes then return false end
    local quoted, escaped, depth, index = false, false, 0, 1
    while index <= #content do
      local char = content:sub(index, index)
      if quoted then
        if escaped then escaped = false elseif char:byte() == 92 then escaped = true elseif char == '"' then quoted = false end
      elseif char == '"' then quoted = true
      elseif char == "{" or char == "[" then
        depth = depth + 1
        if depth > protocol.limits.lifecycle_max_depth then return false end
      elseif char == "}" or char == "]" then depth = depth - 1
      elseif char:match("[%d%-]") then
        local finish = content:find("[^0-9eE+%.%-]", index) or (#content + 1)
        local token = content:sub(index, finish - 1)
        -- The only numeric wire fields here are unsigned integer schema values.
        if not is_canonical_decimal(token, 20) or (#token == 20 and token > "18446744073709551615") then return false end
        index = finish - 1
      end
      index = index + 1
    end
    return depth == 0 and not quoted
  end

  local function parse_v2_record_json(content, expected_kind)
    if type(content) ~= "string" or content == "" then
      return nil, invalid("record JSON is empty")
    end
    if protocol and #content > protocol.limits.max_json_bytes then
      return nil, invalid("record exceeds the JSON size limit")
    end
    if json_contains_null_literal(content) then
      return nil, invalid("record contains unsupported null")
    end
    if expected_kind == "lifecycle_snapshot" and not lifecycle_raw_valid(content) then
      return nil, invalid("lifecycle JSON exceeds its bounds or contains a noncanonical integer")
    end
    local value, parse_err = decode_json(content)
    if value == nil then
      return nil, invalid("record is not valid JSON", { detail = tostring(parse_err) })
    end
    if type(value) == "table" and value.kind == "lifecycle_snapshot" and expected_kind ~= "lifecycle_snapshot" and not lifecycle_raw_valid(content) then
      return nil, invalid("lifecycle JSON exceeds its bounds or contains a noncanonical integer")
    end
    return parse_v2_record(value, expected_kind)
  end

  local function compare_ns20(left, right)
    if not is_ns20(left) or not is_ns20(right) then return nil end
    if left < right then return -1 end
    if left > right then return 1 end
    return 0
  end

  local function unix_ns_parts(value)
    if not is_ns20(value) then return nil end
    return tonumber(value:sub(1, 11)), tonumber(value:sub(12, 20))
  end

  local function format_unix_ns20(value)
    if type(value) ~= "string" or value == "" or value:find("[^0-9]") or #value > 20 then
      return nil
    end
    return string.rep("0", 20 - #value) .. value
  end

  local function wezterm_now_unix_ns20()
    if not (wezterm.time and type(wezterm.time.now) == "function") then
      return nil, "probe_unavailable"
    end
    local ok, value = pcall(function()
      return wezterm.time.now():format_utc("%s%9f")
    end)
    if not ok then return nil, "probe_unavailable" end
    local padded = format_unix_ns20(value)
    if not padded then return nil, "probe_unavailable" end
    return padded
  end

  local function add_ms_to_unix_ns(value, ttl_ms)
    local seconds, nanos = unix_ns_parts(value)
    if not seconds or not is_integer(ttl_ms) or ttl_ms <= 0 then return nil end
    seconds = seconds + math.floor(ttl_ms / 1000)
    nanos = nanos + (ttl_ms % 1000) * 1000000
    if nanos >= 1000000000 then
      seconds = seconds + 1
      nanos = nanos - 1000000000
    end
    if seconds > 99999999999 then return nil end
    return string.format("%011d%09d", seconds, nanos)
  end

  local function seconds_until_after(now_value, boundary)
    local now_seconds, now_nanos = unix_ns_parts(now_value)
    local boundary_seconds, boundary_nanos = unix_ns_parts(boundary)
    if not now_seconds or not boundary_seconds then return nil end
    local seconds = boundary_seconds - now_seconds
    local nanos = boundary_nanos - now_nanos + 1
    if nanos < 0 then
      seconds = seconds - 1
      nanos = nanos + 1000000000
    elseif nanos >= 1000000000 then
      seconds = seconds + 1
      nanos = nanos - 1000000000
    end
    if seconds < 0 then return 0 end
    return seconds + nanos / 1000000000
  end

  local function age_exceeds_ms(now_value, written_value, ttl_ms)
    if not is_ns20(written_value) then return nil, "record_invalid" end
    if not is_ns20(now_value) then return nil, "probe_unavailable" end
    if now_value < written_value then return nil, "clock_skew" end
    local boundary = add_ms_to_unix_ns(written_value, ttl_ms)
    if not boundary then return nil, "record_invalid" end
    return now_value > boundary, nil, boundary
  end

  local function address_cache_key(address)
    return table.concat({ "v2", address.realm_id, address.incarnation_id, address.pane_id }, ":")
  end

  local function v2_pane_root(dir, address)
    return table.concat({
      dir, "v2/realms", address.realm_id, "incarnations", address.incarnation_id,
      "panes", address.pane_id,
    }, "/")
  end

  local function binding_root(dir, address, launch_id, binding_id)
    return table.concat({
      v2_pane_root(dir, address), "launches", launch_id, "bindings", binding_id,
    }, "/")
  end

  local function read_record_file(path, expected_kind)
    local content, read_err, read_status = read_all(path, expected_kind == "lifecycle_snapshot" and protocol.limits.lifecycle_max_json_bytes or nil)
    if not content then
      if read_status == "invalid" then return nil, invalid("lifecycle record exceeds its bound"), "invalid" end
      local message = tostring(read_err or "")
      if message:find("No such file", 1, true) or message:find("no such file", 1, true) then
        return nil, nil, "missing"
      end
      return nil, diagnostic("probe_unavailable", "record could not be read", {
        path = path, detail = message,
      }), "unavailable"
    end
    local record, parse_diagnostic = parse_v2_record_json(content, expected_kind)
    if not record then return nil, parse_diagnostic, "invalid" end
    return record, nil, "valid"
  end

  local function record_matches(record, expected)
    if expected.address and not same_address(record.address, expected.address) then return false end
    for _, field in ipairs({ "realm_id", "incarnation_id", "launch_id", "binding_id" }) do
      if expected[field] and record[field] ~= expected[field] then return false end
    end
    return true
  end

  local function identity_diagnostic(kind, path)
    return invalid(kind .. " record interior identity does not match its path", { path = path })
  end

  local function read_expected_record(path, kind, expected, required)
    local record, read_diagnostic, status = read_record_file(path, kind)
    if status == "missing" and not required then return nil, nil, status end
    if not record then
      return nil,
        read_diagnostic or invalid("required " .. kind .. " record is missing", { path = path }),
        status
    end
    if expected and not record_matches(record, expected) then
      return nil, identity_diagnostic(kind, path), "invalid"
    end
    return record, nil, status
  end

  local function read_expected_record_cached(path, kind, expected, required, cached)
    local record, read_diagnostic, status = read_expected_record(path, kind, expected, required)
    if record or status ~= "unavailable" or not cached then
      return record, read_diagnostic, status
    end
    local parsed = parse_v2_record(cached, kind)
    if parsed and (not expected or record_matches(parsed, expected)) then
      return parsed, read_diagnostic, "cached"
    end
    return nil, read_diagnostic, status
  end

  local function path_stem(path)
    return path:match("([^/]+)%.json$")
  end

  local function glob_paths(pattern, opts)
    local glob = (opts and opts.glob) or wezterm.glob
    if type(glob) ~= "function" then
      return nil, diagnostic("probe_unavailable", "record enumeration is unavailable", {
        pattern = pattern,
      })
    end
    local ok, paths = pcall(glob, pattern)
    if not ok or type(paths) ~= "table" then
      return nil, diagnostic("probe_unavailable", "record enumeration failed", {
        pattern = pattern,
      })
    end
    table.sort(paths)
    return paths
  end

  local function read_record_collection(
      pattern, kind, expected, cached_by_path, key_field, opts)
    local records = {}
    local diagnostics = {}
    local paths, glob_diagnostic = glob_paths(pattern, opts)
    if not paths then
      diagnostics[#diagnostics + 1] = glob_diagnostic
      for path, cached in pairs(cached_by_path or {}) do
        local parsed = parse_v2_record(cached, kind)
        if parsed and record_matches(parsed, expected)
            and (not key_field or path_stem(path) == parsed[key_field]) then
          records[path] = parsed
        end
      end
      return records, diagnostics
    end

    for _, path in ipairs(paths) do
      local record, record_diagnostic = read_expected_record_cached(
        path, kind, expected, true, cached_by_path and cached_by_path[path])
      if record and key_field and path_stem(path) ~= record[key_field] then
        record, record_diagnostic = nil, identity_diagnostic(kind, path)
      end
      if record then records[path] = record end
      if record_diagnostic then diagnostics[#diagnostics + 1] = record_diagnostic end
    end
    return records, diagnostics
  end

  local function collect_diagnostic(diagnostics, value)
    if value then diagnostics[#diagnostics + 1] = value end
  end

  local function health_from_diagnostics(diagnostics)
    for _, item in ipairs(diagnostics) do
      if item.code == "future_schema" then return "future_schema" end
    end
    return #diagnostics > 0 and "invalid" or "valid"
  end

  local function diagnostics_have_unavailable_io(diagnostics)
    for _, item in ipairs(diagnostics) do
      if item.code == "probe_unavailable" and item.context
          and (item.context.path or item.context.pattern) then
        return true
      end
    end
    return false
  end

  local function eligible_subagent(record, clear, floor, now_unix_ns)
    if record.status ~= "active" then return false end
    if clear and compare_ns20(record.observed_mono_ns, clear.observed_mono_ns) <= 0 then
      return false
    end
    if floor and compare_ns20(record.observed_mono_ns, floor.floor_mono_ns) <= 0 then
      return false
    end
    local expired, age_error, boundary =
      age_exceeds_ms(now_unix_ns, record.written_at_unix_ns, record.ttl_ms)
    if age_error then
      return false, diagnostic(age_error, "subagent wall age is not trustworthy", {
        agent_key = record.agent_key,
      })
    end
    if expired then return false end
    return true, nil, boundary
  end

  local function deep_copy(value, seen)
    if type(value) ~= "table" then return value end
    seen = seen or {}
    if seen[value] then return seen[value] end
    local copied = {}
    container_kinds[copied] = container_kinds[value]
    seen[value] = copied
    for key, item in pairs(value) do copied[deep_copy(key, seen)] = deep_copy(item, seen) end
    return copied
  end

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


  return {
    classify_lifecycle_tool = classify_lifecycle_tool,
    is_integer = is_integer,
    is_hex64 = is_hex64,
    is_uuid = is_uuid,
    is_ns20 = is_ns20,
    is_canonical_decimal = is_canonical_decimal,
    is_safe_text = is_safe_text,
    u32 = u32,
    portable_bit_pair = portable_bit_pair,
    load_bit_operators = load_bit_operators,
    bit_and = bit_and,
    bit_xor = bit_xor,
    json_contains_null_literal = json_contains_null_literal,
    list_contains = list_contains,
    exact_fields = exact_fields,
    validate_pane_address = validate_pane_address,
    same_address = same_address,
    validate_activity_target = validate_activity_target,
    validate_field = validate_field,
    validate_shape = validate_shape,
    read_all = read_all,
    decode_json = decode_json,
    protocol = protocol,
    protocol_load_error = protocol_load_error,
    diagnostic = diagnostic,
    invalid = invalid,
    sha256 = sha256,
    parse_wire_value = parse_wire_value,
    parse_wire_json = parse_wire_json,
    parse_v2_record = parse_v2_record,
    parse_v2_record_json = parse_v2_record_json,
    compare_ns20 = compare_ns20,
    unix_ns_parts = unix_ns_parts,
    format_unix_ns20 = format_unix_ns20,
    wezterm_now_unix_ns20 = wezterm_now_unix_ns20,
    add_ms_to_unix_ns = add_ms_to_unix_ns,
    seconds_until_after = seconds_until_after,
    age_exceeds_ms = age_exceeds_ms,
    address_cache_key = address_cache_key,
    v2_pane_root = v2_pane_root,
    binding_root = binding_root,
    read_record_file = read_record_file,
    record_matches = record_matches,
    identity_diagnostic = identity_diagnostic,
    read_expected_record = read_expected_record,
    read_expected_record_cached = read_expected_record_cached,
    path_stem = path_stem,
    glob_paths = glob_paths,
    read_record_collection = read_record_collection,
    collect_diagnostic = collect_diagnostic,
    health_from_diagnostics = health_from_diagnostics,
    diagnostics_have_unavailable_io = diagnostics_have_unavailable_io,
    eligible_subagent = eligible_subagent,
    deep_copy = deep_copy,
    fixture_set_path = fixture_set_path,
    fixture_remove_path = fixture_remove_path,
    fixture_case_value = fixture_case_value,
    parse_fixture_cases = parse_fixture_cases,
    fixture_eligibility_cases = fixture_eligibility_cases,
    now_ms = now_ms,
    frame_for_now = frame_for_now,
    normalize_epoch_ms = normalize_epoch_ms,
    stale_ttl_ms = stale_ttl_ms,
  }
end
