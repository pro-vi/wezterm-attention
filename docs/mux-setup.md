# Mux setup

This setup keeps attention on the right pane across local and attached WezTerm GUIs. Writing v2 records needs macOS or Linux (glibc, including aarch64), the two tested platforms, and the `attention` command built by `scripts/install-cli.sh` (see [Install](../README.md#install)). Without the command, the plugin still reads and renders v1 flat markers.

## WezTerm configuration

Load the plugin before any other `format-tab-title` handler:

```lua
local wezterm = require("wezterm")
local config = wezterm.config_builder()
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

attention.apply_to_config(config, {
  -- Only when `attention` was built in a clone of your own rather than in the
  -- copy wezterm.plugin.require loaded.
  -- integration_root = "/absolute/path/to/wezterm-attention",
  show_provider = true,
})
```

The plugin always exports `WEZTERM_ATTENTION_DIR` to new panes. It exports `WEZTERM_ATTENTION_ROOT` only when `libexec/attention-rs` exists under the integration root, which is the plugin's own checkout unless `integration_root` names another; otherwise it logs once that the command is missing.

After a GUI attaches to an existing mux, it waits for two polls with the same pane count, republishes valid claims, and retries after 2, 5, 10, and 30 seconds while any pane remains unpublished. One schedule is shared per socket. The child PATH includes WezTerm's executable directory. A timer rechecks GUI-window inventory before retrying. If inventory is unavailable, another pane poll must renew the unpublished observation; otherwise that retry is retired. Fresh polls can restart publication. This retires retry evidence, not pane state.

Republication needs the socket of the mux behind the domain:

- A unix domain with an absolute `socket_path` uses that path.
- A unix domain with no `socket_path`, including WezTerm's implicit `unix` domain, uses WezTerm's default socket: `$XDG_RUNTIME_DIR/wezterm/sock` on Linux when `XDG_RUNTIME_DIR` is set, else `~/.local/share/wezterm/sock`.
- A domain with a `proxy_command` or a relative `socket_path` has no socket on this machine to republish through. The plugin logs once for panes on such a domain; they show attention again after their shell's next prompt republishes.

The domain lists are read when a poll needs them, so `unix_domains` set after `apply_to_config` still counts. The `local` domain, every exec domain, every serial port and every WSL domain are this GUI's own panes and need no republication.

Publication validates the socket, pane ID, tty owner, character-device type, claim, and opened tty fingerprint. It writes terminal output only; it never writes pane input.

## Bash launch claims

Source the Bash integration from `~/.bashrc`. The variable is set only inside WezTerm, and only once the command is built, so guard the line:

```bash
[ -n "${WEZTERM_ATTENTION_ROOT:-}" ] && . "$WEZTERM_ATTENTION_ROOT/shell/wezterm-attention.bash"
```

Bash automatically gives each top-level `claude`, `codex`, or `pi` command a new launch ID. The
supported command set can be changed before sourcing:

```bash
export WEZTERM_ATTENTION_COMMANDS='claude codex pi'
```

Sourcing the file a second time installs nothing new, so `source ~/.bashrc` after an edit is safe; if your rc file assigned `PROMPT_COMMAND` in the meantime, the second source puts the prompt hook back. When [bash-preexec](https://github.com/rcaloras/bash-preexec) is loaded (atuin and some prompt tools load it), the claim runs through its `preexec_functions`, once per command line, so `claude && codex` share one launch ID there; without it, each simple command gets its own. The integration keeps `$?` and `$_` intact for your later prompt commands and commands. After an agent returns to the prompt, the prompt hook records the return and unsets `WEZTERM_ATTENTION_LAUNCH_ID`.

## Zsh launch claims

Zsh exposes compound command lists too late to distinguish every executed agent command. Its
supported path is therefore explicit. In `~/.zshrc`:

```zsh
[[ -n "${WEZTERM_ATTENTION_ROOT:-}" ]] && source "$WEZTERM_ATTENTION_ROOT/shell/wezterm-attention.zsh"
```

Then launch agents through the claim:

```zsh
wezterm_attention_claim && claude
wezterm_attention_claim && codex
wezterm_attention_claim && pi
```

Call `wezterm_attention_claim` once immediately before each supported agent launch. The zsh prompt
hook still republishes the current claim automatically. Both run only inside a WezTerm pane: outside one, the prompt hook does nothing and `wezterm_attention_claim` returns 0, so `wezterm_attention_claim && claude` still starts the agent. Inside a pane whose `WEZTERM_ATTENTION_ROOT` names no built command, the claim returns 1.

### Where a redirect may go

`attention hooks claim` requires a terminal on its standard input: when standard input is not a
terminal it fails with `unsafe_tty: stdin is not a terminal` and the helper leaves
`WEZTERM_ATTENTION_LAUNCH_ID` unset. In the `wezterm_attention_claim && <agent>` form above, the
helper returns that failure and the `&&` stops the agent from starting, which is what the `&&` is
for.

After any command line other than one holding only `wezterm_attention_claim`, the zsh prompt hook
unsets `WEZTERM_ATTENTION_LAUNCH_ID`, so a program started after the agent exits does not inherit
its identity. Claiming on one line and starting the agent on the next still works: the line that
held only the claim keeps the ID for the next one.

The danger is a wrapper that calls the helper and launches the agent regardless of its status. There
the agent runs with no identity, every callback from that run is discarded, and nothing in the
agent's own output says so.

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

### When a claim is refused

A claim belongs to the pane's own terminal. A program started inside the pane, such as tmux, screen or an editor's terminal, inherits `WEZTERM_PANE` but runs on a different terminal, and a claim from there would take the pane away from the agent that owns it. So `attention hooks claim` asks the mux for the pane and refuses with `unsafe_tty` when the mux lists the pane on another terminal, or does not list it at all. When the mux cannot be asked, it refuses if `TMUX` or `STY` is set, and otherwise proceeds. Start agents from the pane's own shell, not from a tmux or screen session inside it.

## Provider hooks

Registration is yours; this repository does not edit Claude or Codex settings. The README has complete, copyable blocks for [Claude Code](../README.md#claude-code-hooks) and [Codex](../README.md#codex-hooks). They come from the package's own description:

```sh
attention hooks describe --provider claude --json
attention hooks describe --provider codex --json
```

For each `registration=register` row, run the `attention` link on your PATH with that row's `arguments` and pass the original callback JSON on stdin. Ignored rows are not registrations. `requires_launch_identity` qualifies rich facts and executable delivery; it does not remove legacy support. Evidence references describe parser, fixture and native-contact coverage, not live activation.

Do not register a `SubagentStart` attention writer. A child becomes visible only after its first
tool work. `SubagentStop` records ordered stopped evidence for that exact child.

The bundled Pi extension is installed with `pi install` rather than registered here; see [Pi extension](../README.md#pi-extension). Its handlers enqueue without awaiting filesystem work and drain the queue during shutdown.

## How attention finds the wezterm command

`attention` runs `wezterm cli` to list a mux's panes. It looks for that command in this order:

1. `wezterm` in each absolute `PATH` entry. Relative entries such as `.` are skipped.
2. `$WEZTERM_EXECUTABLE_DIR/wezterm`.
3. `wezterm` in the directory of `$WEZTERM_EXECUTABLE`.
4. `/Applications/WezTerm.app/Contents/MacOS/wezterm`.

It never runs `$WEZTERM_EXECUTABLE` itself: inside a pane that variable names `wezterm-gui` or `wezterm-mux-server`, and neither of them is the CLI. A `PATH` entry is trusted as your own choice. A candidate from steps 2 to 4 is used only when it is still named `wezterm` after symlinks are resolved, and every directory above it is owned by root or by you and is not writable by group or others. A root-owned directory whose group is `admin` or `wheel` may be group-writable, which is how macOS installs `/Applications`, but never writable by others.

Every `wezterm cli` call passes `--no-auto-start`, so a query against a stale socket never starts a new mux server. A socket that refuses connections is not read as a server that exited, since a live server whose accept queue is full refuses too: its records are kept and reported as `socket_refused`. A GUI that quit is recognised instead by its `gui-sock-<pid>` process being gone. A call that runs past its deadline is killed together with its whole process group. When a pane listing fails on a socket that does not refuse, the diagnostic names the executable and what went wrong. The code is `realm_unavailable` for `wezterm cli list via <path> timed out after 5000 ms`, `… exited with status N`, `… was killed by signal N`, `… could not be started` and `… output could not be read`, and `record_invalid` for `… output exceeded its bound`. A control character in the path prints as `?`. A `realm_unavailable` listing is a probe that did not answer: `doctor` and `sweep` are then incomplete and exit 1.

## Verify the installation

```bash
attention doctor
attention bindings --json
attention sweep --json
```

`doctor` covers CLI-visible files, sockets, processes, permissions, versions and the environment it runs in. It explicitly does not observe GUI user variables. Run inside a pane, the `environment` probe checks that the pane's socket has a server identity that hooks can find; outside a pane it says `unobserved`. Any probe that had nothing to check says `unobserved` rather than `healthy`, and is listed in `result.unobserved`; when every probe but `versions` was unobserved and there are no diagnostics, the whole report says `unobserved`. Without `--json`, `doctor` prints the status word on stdout and each diagnostic on stderr as `attention: <code>: <message>`.

`sweep` previews and removes nothing unless `--apply` is given. `attention sweep --apply` makes up a fresh operation id for that run and reports it in `result.operation_id`. Pass `--operation-id` only to retry a run that was interrupted: a run under an id already used is treated as a replay of it. See [Record contract](record-contract.md#compatibility-and-precedence).
