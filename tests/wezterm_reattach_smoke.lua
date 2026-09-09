local wezterm = require("wezterm")

local root = assert(os.getenv("WEZTERM_ATTENTION_TEST_ROOT"))
local socket_path = assert(os.getenv("WEZTERM_ATTENTION_TEST_SOCKET"))
local state_root = assert(os.getenv("WEZTERM_ATTENTION_DIR"))
local result_path = assert(os.getenv("WEZTERM_ATTENTION_SMOKE_RESULT"))
local process_root = assert(os.getenv("WEZTERM_ATTENTION_TEST_PROCESS_ROOT"))
local enable_publish = os.getenv("WEZTERM_ATTENTION_ENABLE_PUBLISH") == "1"

package.path = root .. "/?/init.lua;" .. package.path
package.loaded.plugin = nil
local attention = require("plugin")

local config = wezterm.config_builder()
config.unix_domains = {
  {
    name = "attention-u2-smoke",
    socket_path = socket_path,
    no_serve_automatically = true,
  },
}
config.status_update_interval = 100
config.exit_behavior = "Close"
config.daemon_options = {
  pid_file = process_root .. "/mux.pid",
  stdout = process_root .. "/mux.stdout",
  stderr = process_root .. "/mux.stderr",
}

if enable_publish then
  attention.apply_to_config(config, {
    integration_root = root,
    dir = state_root,
    review_key = false,
  })
end

wezterm.on("update-status", function(window, _)
  local mux_window = window:mux_window()
  if not mux_window then return end
  local tabs = mux_window:tabs()
  local entries = {}
  for _, tab in ipairs(tabs) do
    for _, pane in ipairs(tab:panes()) do
      local vars = pane:get_user_vars()
      local value = type(vars) == "table" and vars.WEZTERM_ATTENTION or nil
      local pane_id = type(vars) == "table" and vars.WEZTERM_PANE or nil
      entries[#entries + 1] = {
        published = value ~= nil,
        pane = pane_id,
        domain = pane:get_domain_name(),
        wire = value and wezterm.json_parse(value) or nil,
        activity = enable_publish and pane_id and attention.get_attention(pane_id) or nil,
      }
    end
  end
  if not entries[1] then return end
  table.sort(entries, function(left, right) return tostring(left.pane) < tostring(right.pane) end)
  local all_published = true
  for _, entry in ipairs(entries) do
    if not entry.published then all_published = false end
  end
  local first = entries[1]
  local result = {
    published = all_published,
    pane = first.pane,
    domain = first.domain,
    panes = entries,
    plugin_root = enable_publish and config.set_environment_variables.WEZTERM_ATTENTION_ROOT or nil,
  }
  local file = assert(io.open(result_path, "w"))
  assert(file:write(wezterm.json_encode(result)))
  assert(file:close())
end)

return config
