#!/usr/bin/env sh
# Loads examples/wezterm.lua in a real WezTerm config evaluation
# (`wezterm show-keys`, which opens no window and touches no mux), with this
# checkout's plugin in place of the plugin download, then runs its
# update-status handler once.
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
wezterm=${ATTENTION_TEST_WEZTERM:-$(command -v wezterm)}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-examples-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
failures=0

mkdir -p "$scratch/home" "$scratch/config"

cat > "$scratch/config/wezterm.lua" <<'LUA'
local wezterm = require("wezterm")
local root = os.getenv("ATTENTION_EXAMPLE_ROOT")
local result = assert(io.open(os.getenv("ATTENTION_EXAMPLE_RESULT"), "w"))
package.path = root .. "/?/init.lua;" .. package.path
local attention = require("plugin")
wezterm.plugin.require = function(url)
  assert(url:find("wezterm-attention", 1, true), "the example loads only this plugin: " .. url)
  return attention
end
local handlers = {}
local register = wezterm.on
wezterm.on = function(name, handler)
  handlers[name] = handler
  return register(name, handler)
end
-- wezterm-mux-server reads the same config, and its Lua has no gui module.
if os.getenv("ATTENTION_EXAMPLE_NO_GUI") then wezterm.gui = nil end
local ok, config = pcall(dofile, root .. "/examples/wezterm.lua")
result:write("loaded=", tostring(ok), "\n")
if not ok then result:write("error=", tostring(config), "\n"); result:close(); error(config) end
local window, pane = {}, {}
attention.poll = function(polled, opts)
  result:write("polled=", tostring(polled == window and opts.active_pane == pane), "\n")
end
local ran, problem = pcall(handlers["update-status"], window, pane)
result:write("update_status=", tostring(ran), " ", tostring(problem), "\n")
result:close()
return config
LUA

# Loads the example once; the key table goes to keys, the wrapper's report to
# result. Arguments are extra environment assignments.
load_example() {
  rm -f "$scratch/result"
  env -i HOME="$scratch/home" PATH=/usr/bin:/bin ATTENTION_EXAMPLE_ROOT="$root" \
    ATTENTION_EXAMPLE_RESULT="$scratch/result" "$@" \
    "$wezterm" --config-file "$scratch/config/wezterm.lua" show-keys --lua \
    > "$scratch/keys" 2> "$scratch/stderr"
}
check() {
  if "$@"; then printf 'ok - %s\n' "$name"; else
    printf 'not ok - %s\n' "$name"
    sed 's/^/#   /' "$scratch/result" 2>/dev/null | tr -d '\033'
    failures=$((failures + 1))
  fi
}

load_example
name="the example config loads without follow-up.lua beside it"
check grep -q '^loaded=true$' "$scratch/result"

cp "$root/examples/follow-up.lua" "$scratch/config/follow-up.lua"
load_example
name="the example config loads with follow-up.lua beside it"
check grep -q '^loaded=true$' "$scratch/result"
name="the example keeps the plugin's Alt+B binding"
check grep -q "^    { key = 'b', mods = 'ALT', " "$scratch/keys"
name="the example's update-status handler polls with the event pane"
check sh -c 'grep -q "^polled=true$" "$1" && grep -q "^update_status=true" "$1"' _ "$scratch/result"

load_example ATTENTION_EXAMPLE_NO_GUI=1
name="the example config loads where WezTerm has no gui module, as in the mux server"
check grep -q '^loaded=true$' "$scratch/result"

[ "$failures" -eq 0 ]
