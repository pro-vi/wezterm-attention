-- WezTerm config with the wezterm-attention plugin: its options, the view
-- callback, and a poll from the config's own update-status handler.
-- Add your own keys, theme and status bar around it.

local wezterm = require("wezterm")
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

local config = wezterm.config_builder()

-- Optional: copy examples/follow-up.lua beside this config. It keeps, per
-- window and pane scope, a display choice ("follow_up", "base" or "unknown")
-- that your own rendering reads with follow_up.appearance(window_id, scope).
-- It performs no action when a view first appears. Without the file the rest
-- of this config still loads; an error inside the file is still reported.
local follow_up_path = wezterm.config_dir .. "/follow-up.lua"
local follow_up_file = io.open(follow_up_path, "r")
local follow_up = nil
if follow_up_file then
  follow_up_file:close()
  follow_up = dofile(follow_up_path).for_windows()
end

-- Adds the Alt+B review toggle to config.keys, so a config that assigns
-- config.keys does so before this call.
attention.apply_to_config(config, {
  -- This config polls from its own update-status handler below.
  auto_poll = false,
  -- Optional closed provider suffix: " · Claude", " · Codex", or " · Pi".
  show_provider = true,
  on_view_change = follow_up and follow_up.on_view_change or nil,
})

wezterm.on("update-status", function(window, pane)
  -- Poll attention markers manually; the plugin still owns tab formatting.
  -- Passing the event pane gives older WezTerm builds a compatibility
  -- transport for redraws. Current builds resolve the active pane at use time.
  attention.poll(window, { active_pane = pane })
  -- The rest of your status bar goes here.
end)

return config
