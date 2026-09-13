-- Existing fixture callers exercise the executable consumer example.
local source = debug and debug.getinfo(1, "S").source:sub(2)
local root = source and source:match("^(.*)/tests/fixtures/lifecycle/consumer.lua$")
root = root or os.getenv("WEZTERM_ATTENTION_TEST_ROOT") or os.getenv("WEZTERM_ATTENTION_ROOT")
assert(root, "consumer fixture requires its explicit integration root")
return dofile(root .. "/examples/follow-up.lua")
