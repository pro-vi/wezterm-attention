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

The plugin exports `WEZTERM_ATTENTION_ROOT` and `WEZTERM_ATTENTION_DIR` to new panes. After a GUI attaches to an existing mux, it waits for two polls with the same pane count, republishes valid claims, and retries after 2, 5, 10, and 30 seconds while any pane remains unpublished. One schedule is shared per socket. The child PATH includes WezTerm's executable directory. A timer rechecks GUI-window inventory before retrying. If inventory is unavailable, another pane poll must renew the unpublished observation; otherwise that retry is retired. Fresh polls can restart publication. This retires retry evidence, not pane state.

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

### Where a redirect may go

`attention hooks claim` requires a terminal on its standard input, and it never blocks a launch: when
standard input is not a terminal it fails with `unsafe_tty: stdin is not a terminal`, the helper
leaves `WEZTERM_ATTENTION_LAUNCH_ID` unset, and the agent runs with no identity. Every callback from
that run is then discarded, and nothing in the agent's own output says so.

`codex exec` and `claude -p` both commonly take a redirect, so put it on the agent and not on
anything that contains the claim:

```zsh
wezterm_attention_claim && codex exec "…" < /dev/null    # claim holds; the redirect is the agent's
```

The trap is a wrapper. If you write your own helper that calls `wezterm_attention_claim` and then
execs the agent, a redirect written on the wrapper applies to the claim inside it too:

```sh
with_attention_claim codex exec "…" < /dev/null          # claim fails, identity unset, callbacks lost
with_attention_claim sh -c 'codex exec "…" </dev/null'   # claim holds
```

A wrapper that ignores the claim's exit status makes this silent. Check it, or keep the redirect
inside the innermost command.

## Provider hooks

Provider registration remains user-owned. This repository does not edit Claude or Codex settings.
Read the package-owned registration descriptions: For each `registration=register` row, prepend the resolved Attention executable to `arguments` and forward the original callback JSON to that invocation. Ignored rows are not registrations. `requires_launch_identity` qualifies rich facts and executable delivery; it does not remove legacy support. Evidence references describe parser, fixture and native-contact coverage, not live activation. For Pi, install the reported `extension_entrypoint`; its bus event is `wezterm-attention:mark`.

```sh
attention hooks describe --provider claude --json
attention hooks describe --provider codex --json
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
