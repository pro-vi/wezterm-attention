#!/usr/bin/env sh
# Loads examples/wezterm.lua in a real WezTerm config evaluation
# (`wezterm show-keys`, which opens no window and touches no mux), with this
# checkout's plugin in place of the plugin download and a stand-in for the
# session plugin, then runs its status-bar handler once against a scratch
# repository.
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
wezterm=${ATTENTION_TEST_WEZTERM:-$(command -v wezterm)}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-examples-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
failures=0

mkdir -p "$scratch/home" "$scratch/config" "$scratch/repo"
git -C "$scratch/repo" init -q
git -C "$scratch/repo" -c user.name=spec -c user.email=spec@example.invalid \
  commit -q --allow-empty -m start
printf 'x\n' > "$scratch/repo/file"
git -C "$scratch/repo" add file
printf '#!/bin/sh\ntouch "%s/fsmonitor-ran"\n' "$scratch" > "$scratch/fsmonitor"
chmod 755 "$scratch/fsmonitor"
git -C "$scratch/repo" config core.fsmonitor "$scratch/fsmonitor"

cat > "$scratch/config/wezterm.lua" <<'EOF'
local wezterm = require("wezterm")
local root = os.getenv("ATTENTION_EXAMPLE_ROOT")
local result = assert(io.open(os.getenv("ATTENTION_EXAMPLE_RESULT"), "w"))
package.path = root .. "/?/init.lua;" .. package.path
local attention = require("plugin")
local stand_in
stand_in = setmetatable({}, { __index = function() return stand_in end, __call = function() end })
wezterm.plugin.require = function(url)
  if url:find("wezterm-attention", 1, true) then return attention end
  return stand_in
end
local handlers = {}
local register = wezterm.on
wezterm.on = function(name, handler)
  handlers[name] = handler
  return register(name, handler)
end
local ok, config = pcall(dofile, root .. "/examples/wezterm.lua")
result:write("loaded=", tostring(ok), "\n")
if not ok then result:write("error=", tostring(config), "\n"); result:close(); error(config) end
attention.poll = function() end
local window = { set_left_status = function() end,
  set_right_status = function(_, text) result:write("status=", text, "\n") end }
local pane = { get_current_working_dir = function() return { file_path = os.getenv("ATTENTION_EXAMPLE_REPO") } end,
  tab = function() return nil end }
local ran, problem = pcall(handlers["update-status"], window, pane)
result:write("update_status=", tostring(ran), " ", tostring(problem), "\n")
result:close()
return config
EOF

# Loads the example once; the key table goes to keys, the wrapper's report to
# result.
load_example() {
  rm -f "$scratch/result" "$scratch/fsmonitor-ran"
  env -i HOME="$scratch/home" PATH=/usr/bin:/bin ATTENTION_EXAMPLE_ROOT="$root" \
    ATTENTION_EXAMPLE_RESULT="$scratch/result" ATTENTION_EXAMPLE_REPO="$scratch/repo" \
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
name="the status bar reads the repository without running its fsmonitor hook"
check sh -c 'grep -q "^update_status=true" "$1" && grep -q "^status=.*+1" "$1" && [ ! -e "$2" ]' _ \
  "$scratch/result" "$scratch/fsmonitor-ran"

[ "$failures" -eq 0 ]
