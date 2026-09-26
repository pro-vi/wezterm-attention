-- Read one pane's view through the plugin reader inside a real WezTerm, so a
-- Rust test can check what the tab would show for the records the Rust writer
-- left behind, rather than for records a Lua fixture built from the reader's
-- own rule.
--
-- Run as `wezterm --config-file <this file> show-keys --lua`. The answer is
-- one line in WEZTERM_ATTENTION_VIEW_RESULT:
--   ok activity=<type or none> source=<source or none>
-- or `not ok - <error>`.

local wezterm = require("wezterm")

local result_path = assert(os.getenv("WEZTERM_ATTENTION_VIEW_RESULT"),
  "WEZTERM_ATTENTION_VIEW_RESULT is required")

local function run()
  local root = assert(os.getenv("WEZTERM_ATTENTION_TEST_ROOT"),
    "WEZTERM_ATTENTION_TEST_ROOT is required")
  local dir = assert(os.getenv("WEZTERM_ATTENTION_DIR"), "WEZTERM_ATTENTION_DIR is required")
  local now = assert(os.getenv("WEZTERM_ATTENTION_VIEW_NOW"), "WEZTERM_ATTENTION_VIEW_NOW is required")
  package.path = root .. "/?/init.lua;" .. package.path
  local internal = assert(require("plugin")._internal, "plugin test seams are unavailable")
  local wire = assert(internal.parse_wire_json(assert(os.getenv("WEZTERM_ATTENTION_VIEW_WIRE"),
    "WEZTERM_ATTENTION_VIEW_WIRE is required")))
  local view = internal.read_attention_view({
    kind = "claimed",
    address = wire.address,
    launch_id = wire.launch_id,
    marker_id = wire.address.pane_id,
    cache_key = internal.address_cache_key(wire.address),
  }, now, { dir = dir, glob = wezterm.glob })
  return string.format("activity=%s source=%s",
    tostring(view.activity_type or "none"), tostring(view.source or "none"))
end

local passed, result = xpcall(run, function(error_value) return tostring(error_value) end)
local result_file = assert(io.open(result_path, "w"))
assert(result_file:write((passed and "ok " or "not ok - ") .. tostring(result) .. "\n"))
assert(result_file:close())

return {}
