local fixture = arg[1]
local make_detect_agent = dofile(fixture)

local process_reads = 0
local pane = {
  get_foreground_process_name = function()
    process_reads = process_reads + 1
    return "/usr/bin/zsh"
  end,
}
local detect = make_detect_agent({
  get_attention_view = function(received)
    assert(received == pane)
    return { provider = "claude", binding_phase = "active" }
  end,
})
assert(detect(pane, "plain") == "claude")
assert(process_reads == 0)

detect = make_detect_agent({ get_attention_view = function() return nil end })
pane.get_foreground_process_name = function() return "/opt/bin/codex-aarch64-apple-darwin" end
assert(detect(pane, "plain") == "codex")
pane.get_foreground_process_name = function() return "/usr/bin/zsh" end
assert(detect(pane, "π - session") == "pi")

print("ok - migrated detect_agent prefers the cached provider view")
