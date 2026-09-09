# Mux setup

This setup enables pane-correct attention across local and attached WezTerm GUIs. V2 requires a POSIX system and the Rust CLI installed by `scripts/install-cli.sh`. Unsupported platforms keep the v1 reader and renderer.

## WezTerm configuration

Load the plugin before any other `format-tab-title` handler:

```lua
local wezterm = require("wezterm")
local config = wezterm.config_builder()
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

attention.apply_to_config(config, {
  -- Set this only when automatic checkout discovery fails.
  integration_root = "/absolute/path/to/wezterm-attention",
  show_provider = true,
})
```

The plugin exports `WEZTERM_ATTENTION_ROOT` and `WEZTERM_ATTENTION_DIR` to new panes. After a GUI attaches to an existing mux, it waits for two polls with the same pane count, republishes valid claims, and retries after 2, 5, 10, and 30 seconds while any pane remains unpublished. One schedule is shared per socket. The child PATH includes WezTerm's executable directory.

Publication validates the socket, pane ID, tty owner, character-device type, claim, and opened tty fingerprint. It writes terminal output only; it never writes pane input.

## Bash launch claims

Source the Bash integration after `WEZTERM_ATTENTION_ROOT` is set:

```bash
source "$WEZTERM_ATTENTION_ROOT/shell/wezterm-attention.bash"
```

Bash automatically gives each top-level `claude`, `codex`, or `pi` command a new launch ID. The
supported command set can be changed before sourcing:

```bash
export WEZTERM_ATTENTION_COMMANDS='claude codex pi'
```

## Zsh launch claims

Zsh exposes compound command lists too late to distinguish every executed agent command. Its
supported path is therefore explicit:

```zsh
source "$WEZTERM_ATTENTION_ROOT/shell/wezterm-attention.zsh"

wezterm_attention_claim && claude
wezterm_attention_claim && codex
wezterm_attention_claim && pi
```

Call `wezterm_attention_claim` once immediately before each supported agent launch. The zsh prompt
hook still republishes the current claim automatically.

## Provider hooks

Provider registration remains user-owned. This repository does not edit Claude or Codex settings.
Each command receives the provider's original JSON on stdin:

```text
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude SessionStart
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude PreToolUse
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude PermissionRequest
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude Stop
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude SubagentStop
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event claude SessionEnd

$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex SessionStart
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex PreToolUse
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex PermissionRequest
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex Stop
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex SubagentStop
$WEZTERM_ATTENTION_ROOT/bin/attention hooks event codex SessionEnd
```

Do not register a `SubagentStart` attention writer. A child becomes visible only after its first
tool work. `SubagentStop` records ordered stopped evidence for that exact child.

The bundled Pi extension forwards `session_start`, agent activity, the shared attention bus, and
`session_shutdown` through the same command. Its handlers enqueue without awaiting filesystem work
and drain the queue during shutdown.

## Verify the installation

```bash
"$WEZTERM_ATTENTION_ROOT/bin/attention" doctor
"$WEZTERM_ATTENTION_ROOT/bin/attention" bindings --json
"$WEZTERM_ATTENTION_ROOT/bin/attention" sweep --json
```

`doctor` covers CLI-visible files, sockets, processes, permissions, and versions. It explicitly
does not observe GUI user variables. `sweep` is preview-only unless `--apply --operation-id UUID` is
present.
