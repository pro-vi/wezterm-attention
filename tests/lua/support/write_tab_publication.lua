-- Drive the production tab-order encoder so a reader test can consume the
-- exact bytes the plugin writes, not a fixture built from the reader's rule.

local dir = assert(os.getenv("WEZTERM_ATTENTION_DIR"), "WEZTERM_ATTENTION_DIR")
local repo = assert(os.getenv("ATTENTION_REPO"), "ATTENTION_REPO")
local source_response = os.getenv("ATTENTION_TAB_SOURCE_RESPONSE")
local tab_source
if source_response then
  package.path = repo .. "/?/init.lua;" .. package.path
  local attention = require("plugin")
  tab_source = assert(attention._internal.parse_tab_source_response(source_response), "source response rejected")
end

local overlays = dofile(repo .. "/plugin/overlays.lua")({
  -- A source is encoded with WezTerm's encoder, which only a WezTerm run has.
  wezterm = { log_error = function() end, json_encode = source_response and require("wezterm").json_encode },
  now_ms = function() return 1789884000123 end,
})

local made_directory = os.execute("mkdir -p " .. dir .. "/tabs")
assert(made_directory == 0 or made_directory == true)
local v2 = "v2:" .. string.rep("a", 64) .. ":" .. string.rep("b", 64) .. ":16"
assert(overlays.publish_tab_order(dir, 0, {
  { number = 11, text = " 11: braid ", marker_ids = { v2 } },
  { number = 4, text = " 4: construal ", marker_ids = { "4", "9" } },
}, tab_source), "publish_tab_order did not write")
