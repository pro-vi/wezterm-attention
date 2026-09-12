local wezterm = require("wezterm")
local passed, result = pcall(function()
  local root = assert(os.getenv("ATTENTION_FROZEN_READER_ROOT"))
  local state = assert(os.getenv("WEZTERM_ATTENTION_DIR"))
  package.path = root .. "/?/init.lua;" .. package.path
  package.loaded.plugin = nil
  local attention = require("plugin")
  local api = attention._internal
  assert(api.protocol.records.lifecycle_snapshot, "frozen reader must load the new manifest")
  local wire = assert(api.parse_wire_json(os.getenv("ATTENTION_TEST_WIRE")))
  local view = api.read_attention_view({ address = wire.address, launch_id = wire.launch_id, marker_id = wire.address.pane_id, cache_key = api.address_cache_key(wire.address) }, "99999999999999999999", { dir = state })
  assert(view.type == "thinking" and view.provider == "codex" and view.binding_health == "valid")
  assert(view.lifecycle == nil, "old reader does not interpret the sidecar")
  assert(select("#", attention.get_attention("42", { dir = state })) == 6, "six-value API remains usable")
  assert(attention.get_attention("42", { dir = state }) == "thinking", "v1 projection remains readable")
  return "frozen reader accepts the additive manifest and preserves core rendering"
end)
local file = assert(io.open(assert(os.getenv("ATTENTION_COMPAT_RESULT")), "w"))
file:write((passed and "ok - " or "not ok - ") .. tostring(result) .. "\n")
file:close()
return {}
