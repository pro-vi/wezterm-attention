# Setup

The README's [Install](../README.md#install) is the short version. This page has each step in full, and the hook registration for each agent.

Upgrading from 0.6? Follow [the upgrade steps in the changelog](../CHANGELOG.md#upgrading-from-06).

## The plugin

This needs WezTerm `20230320-124340-559cb7b0` or newer, the first release with `wezterm.plugin.require`. A stock release is enough; Attention depends on no patched build ([ADR 0007](adr/0007-stock-wezterm-only.md)). Add the plugin before any other `format-tab-title` handler:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

`apply_to_config` adds its `Alt+B` and `Alt+M` keys to `config.keys`. Assign your own `config.keys = { ... }` before this call: an assignment after it replaces the list and drops both.

## The `attention` command

`wezterm.plugin.require` clones this repository into WezTerm's own plugin directory, and by default the plugin looks for the command only in that copy. Build it there. This needs macOS or Linux (glibc, including aarch64), the two tested platforms, and a Rust toolchain with `cargo` (the minimum version is 1.94, the `rust-version` in `Cargo.toml`). WezTerm creates the directory the first time it loads a config that requires the plugin, so start WezTerm once first.

```sh
case "$(uname)" in
  Darwin) plugins="$HOME/Library/Application Support/wezterm/plugins" ;;
  *) plugins="${XDG_DATA_HOME:-$HOME/.local/share}/wezterm/plugins" ;;
esac
checkout="$plugins/httpssCssZssZsgithubsDscomsZspro-visZswezterm-attention"
sh "$checkout/scripts/install-cli.sh"
```

If the plugin cannot find the command, it logs once per config load, naming the path it checked: `.../libexec/attention-rs is missing, so panes get no WEZTERM_ATTENTION_ROOT and agents record nothing ...`. Open the WezTerm debug overlay (`Ctrl+Shift+L`) to read it.

Reload the config afterwards. New panes then get `WEZTERM_ATTENTION_ROOT`, the checkout path. The plugin exports it only once the command is built: a producer that sees it runs that checkout's command, which would fail in every callback before the build. It exports `WEZTERM_ATTENTION_DIR`, the state directory, whether or not the command is built, unless `integration_root` is not an absolute path: then it exports neither variable and logs `v2 integration root is unavailable`. Panes of a `wezterm-mux-server` get both variables from the server's own run of the config, which a GUI reload does not repeat; see [Mux setup](mux-setup.md#the-mux-server-runs-the-config-too).

`"$checkout/bin/attention" --version` prints the commit the command was built from, with `-dirty` if `src`, `protocol`, `build.rs`, `Cargo.toml` or `Cargo.lock` had uncommitted changes, new files included, when it was built. Compare it with `git -C "$checkout" rev-parse --short=12 HEAD` to see whether the build is current.

Put the command on your PATH by linking the launcher:

```sh
mkdir -p ~/.local/bin
ln -s "$checkout/bin/attention" ~/.local/bin/attention
```

The launcher follows the link to find its checkout, so it works from anywhere on PATH.

**After every `wezterm.plugin.update_all()`, run `install-cli.sh` again.** Updating replaces the Lua files with the newest commit on the repository's default branch, and leaves the previously built command in place, so the plugin can run ahead of the writer.

Then register the [Claude Code](#claude-code-hooks) and [Codex](#codex-hooks) hooks. On macOS that is enough: an agent's first session start claims its pane by itself, so zsh needs no claim step. Bash still claims each agent command it starts, and on Linux a shell claim is the only way to claim; [Mux setup](mux-setup.md) covers both shells. See [Record contract](record-contract.md) for precedence and [Mux pane moves](mux-pane-moves.md) before moving the final pane out of a server tab.

### Using your own clone instead

To load the plugin from a clone you manage, run `scripts/install-cli.sh` in that clone and load it by path. `dofile` does not work: it passes no module path, and WezTerm's Lua has no `debug` library to find one.

```lua
local clone = "/absolute/path/to/wezterm-attention"
local attention = loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua")
attention.apply_to_config(config)
```

With this form, update with `git pull` and rerun `install-cli.sh` in the clone.

This form is also how to stay on a release. `wezterm.plugin.require` cannot name a tag, and `update_all()` moves its copy to the newest commit on the default branch (WezTerm source `4fbd6b8e9`). Clone a release's tag instead:

```sh
git clone --branch v1.0.0 https://github.com/pro-vi/wezterm-attention /absolute/path/to/wezterm-attention
sh /absolute/path/to/wezterm-attention/scripts/install-cli.sh
```

To move to a later release, run `git fetch --tags` and `git checkout` with the new tag in the clone, then rerun `install-cli.sh`. If you keep `wezterm.plugin.require` and build the command in your own clone, pass `integration_root = "/absolute/path/to/wezterm-attention"` to `apply_to_config`; the Lua then updates through `update_all` and the command through your clone, separately.

## Claude Code hooks

This repository does not edit Claude Code's settings; registration is yours. `attention hooks describe --provider claude --json` lists every native event with `registration` set to `register` or `ignored`. For each `register` row, run `attention` with that row's `arguments`, in the command form below; Claude Code passes the callback JSON on stdin, and the command reads it unchanged.

Register the `attention` link on your PATH, not `$WEZTERM_ATTENTION_ROOT/bin/attention`. Hooks are global: they also run in editors, other terminals, ssh sessions and cron, where that variable is unset, and on macOS the plugin directory's path contains a space, which splits an unquoted command. A hook that has nothing to record there, such as one outside WezTerm or in a pane with no claim, exits 0 and does not interrupt the agent. If the agent's PATH does not include `~/.local/bin`, put the link's absolute path in each `command` instead.

Each command is `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude <Event>`, for every event. On macOS this lets an agent that no shell claimed for record itself: its first `SessionStart` checks that the hook's direct parent is the process the variable names, that this process runs on the terminal the mux lists for the pane, and that it runs in the pane's foreground job, then claims the pane for that process. Its later events are accepted from that same process on the terminal it claimed from, checked against the claim without asking the mux again. Keep `$PPID` literal in the file, so the shell the agent starts for the hook reads it there, where it names the agent. Keep the whole command one assignment and one `exec`: a shell that stays behind, as in `...; true`, `a && b` or a wrapper script that runs `attention` without `exec`, becomes the writer's parent instead, and the event is refused (`self_claim_parent_unverified`) without interrupting the agent. An agent started from a claiming shell inherits its launch id and makes no claim of its own, but on macOS the same command form still checks each of its events: the process the variable names must run on the terminal the shell claimed from, or the event is refused and records nothing. So an agent that runs on a terminal of its own inside the pane (tmux, screen, zellij, `script`) is refused (`unsafe_tty`), and so is one whose session runs in a background process with no terminal (`session_detached`). A hook registered without the variable is not checked, and its events go to whichever pane its environment names. A pane a shell has claimed refuses events from an agent that did not inherit that claim's launch id. Set `WEZTERM_ATTENTION_ENABLE_SELF_CLAIM=0` in the agent's environment to accept inherited launch ids only; on Linux that is the only mode in 1.0, the assignment is harmless there, and an inherited launch id is not checked against the agent's terminal.

In `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart":       [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude SessionStart" }] }],
    "UserPromptSubmit":   [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude UserPromptSubmit" }] }],
    "PreToolUse":         [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PreToolUse" }] }],
    "PostToolUse":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PostToolUse" }] }],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PostToolUseFailure" }] }],
    "PermissionRequest":  [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PermissionRequest" }] }],
    "PermissionDenied":   [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PermissionDenied" }] }],
    "Notification":       [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Notification" }] }],
    "Elicitation":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Elicitation" }] }],
    "ElicitationResult":  [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude ElicitationResult" }] }],
    "PreCompact":         [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PreCompact" }] }],
    "PostCompact":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude PostCompact" }] }],
    "Stop":               [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Stop" }] }],
    "StopFailure":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude StopFailure" }] }],
    "SubagentStart":      [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude SubagentStart" }] }],
    "SubagentStop":       [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude SubagentStop" }] }],
    "SessionEnd":         [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude SessionEnd" }] }]
  }
}
```

If you give `SessionStart` a `timeout`, keep it well above 5 s, the most a session start waits for the mux to list its panes; each lock it then waits for adds up to 2 s more. A start Claude Code stops before it claims the pane leaves the whole session unrecorded (see [Troubleshooting](troubleshooting.md#troubleshooting)).

Merge these into any hooks you already have, and keep them synchronous: do not add `async: true` or `asyncRewake: true`, either of which lets one subagent's events arrive out of order, so that a subagent can stay counted after it stopped. `SubagentStart` counts a subagent from its start, and again when it resumes, which Claude Code 2.1.283 reports as a new start under the same id. `SubagentStop` removes that same subagent. A root `Stop` removes only the subagents its `background_tasks` no longer lists, because background subagents can outlive it: Claude Code 2.1.283 sent the lead's `Stop` while a subagent was still running, and 2.1.284 lists it there. A `StopFailure` (the turn ended on an API error) shows `notify`. A turn the user stops with Esc runs no hook, as of Claude Code 2.1.300; where the prompt hook proved its host, on macOS, its turn follower sees the stop in the status file Claude Code keeps for its process and clears the tab (see [accepted limitations](accepted-limitations.md#a-claude-turn-stopped-with-esc-is-recorded-only-from-its-status-file)).

A lead Stop can optionally run an application-owned check before its finish activity is recorded:

```sh
WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Stop \
  --hold-check jev=/absolute/application/stop-hold
```

Only a non-empty native `background_tasks` array with available reply text runs the check. Its input contains the checked scope, reply and unchanged array; neither content is stored by Attention. A validated hold shows fixed `⏾` with thinking color and is not automatically acknowledged. While a sub-agent waits for permission, no check runs, the turn end is recorded unheld, and the tab shows `notify` until the wait ends. Timeout or failure supplies no hold. The checks share an outer budget, 5000 ms unless `--check-budget-ms` sets it; ordinary consumer and native-work time are additional. See [the exact input, output and record contract](consumer-guide.md#pre-write-hold-checks) and [ADR 0005](adr/0005-application-hold-notes-on-recorded-turn-ends.md). Update matching readers and writers before activation; this repository does not register or install the check for you.

`--quiet-check NAME=/absolute/executable` runs the same way on a lead Claude or Codex `Stop`, for an application that can prove nobody needs to be called, such as a controller that submitted the prompt itself. A validated quiet note shows `↪` with a blue tint and records `turn_end.held` false; a waiting permission or a held turn end takes precedence, and a failure, timeout or missing lifecycle history leaves the ordinary turn end. See [quiet checks in the consumer guide](consumer-guide.md#pre-write-hold-checks).

## Codex hooks

Codex reads lifecycle hooks from `~/.codex/hooks.json`, and asks you to approve each new or edited hook once (`/hooks` in Codex). `attention hooks describe --provider codex --json` lists the rows; the same rule and command form apply as for Claude Code, for the same reasons, and so does the advice to register the link on your PATH.

**Start Codex with `--no-daemon`.** Codex 0.157 and later runs its sessions in one shared background server by default, and that server keeps the environment of the terminal that started it, so the hooks of every session would name that first pane. Attention refuses those events and the tab shows nothing: with `session_detached` where a shell claimed the pane, and where none did, with `session_detached` for the session start and `claim_stale` for the events after it. Start Codex as `codex --no-daemon`, and resume or fork the same way (`codex --no-daemon resume`), so the session runs in the Codex process in your pane. The refusal needs the command form below; on Linux the events are not checked.

A plain `codex` typed without the flag still starts that server. Codex 0.157.1 has a setting that stops it, in `$CODEX_HOME/config.toml`:

```toml
[features]
daemon_auto_start = false
```

The setting does not keep a session out of a server that is already running, so use both. If one is running, quit the sessions attached to it, run `codex app-server daemon stop` once for each `CODEX_HOME`, stop the `codex app-server daemon pid-update-loop` process that `stop` leaves running (`pgrep -fl pid-update-loop` shows its pid), and resume the sessions with `--no-daemon`.

```json
{
  "hooks": {
    "SessionStart":      [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex SessionStart" }] }],
    "UserPromptSubmit":  [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex UserPromptSubmit" }] }],
    "PreToolUse":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex PreToolUse" }] }],
    "PostToolUse":       [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex PostToolUse" }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex PermissionRequest" }] }],
    "PreCompact":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex PreCompact" }] }],
    "PostCompact":       [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex PostCompact" }] }],
    "Stop":              [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex Stop" }] }],
    "Interrupt":         [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex Interrupt" }] }],
    "SubagentStart":     [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex SubagentStart" }] }],
    "SubagentStop":      [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex SubagentStop" }] }],
    "SessionEnd":        [{ "hooks": [{ "type": "command", "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex SessionEnd" }] }]
  }
}
```

`SubagentStart` counts a subagent from its start. Child attribution needs matching native `agent_id` values; see [contact evidence](reviews/lifecycle-contact-results.md) for the paths that were exercised. A root `Stop` writes the lead stop and stops counting every subagent whose last event came before it. Codex (source at commit `985cf47a4`) does not make a parent wait for its subagents, but in the sessions recorded with Codex 0.157.1 none worked after its parent's `Stop`; one that does is counted again at its next event (see [accepted limitations](accepted-limitations.md#the-sub-agent-count-depends-on-how-claude-code-and-codex-send-hooks)). An `Interrupt` clears the tab's activity for that session, because Codex runs no `Stop` after one. A turn that ends on a model error runs no hook either, as of Codex 0.162.0; a turn follower records it from the session rollout as `notify` labelled with Codex's error code (see [accepted limitations](accepted-limitations.md#a-codex-turn-that-ends-on-a-model-error-is-recorded-only-from-its-rollout)). A child's `PermissionRequest` shows `notify` on the tab, since Codex has no `Notification` hook.

## Pi extension

Install this repository as a Pi package:

```bash
pi install git:github.com/pro-vi/wezterm-attention
```

The extension registers no commands and leaves print mode alone. It forwards `session_start`, `input`, `agent_start`, `tool_execution_start`, `tool_execution_end`, `message_end`, `session_before_compact`, `session_compact`, `agent_settled`, `session_shutdown` and the `wezterm-attention:mark` bus through one serialized writer queue. `agent_end` is deliberately not treated as the end of a turn, because Pi can retry or continue after it. The writer processes use Node's built-in child-process API, as Pi's Node runtime does.

Other extensions may emit `thinking`, `stop`, `notify`, `review`, or `clear` on the bus. Review uses the `pi-bus` owner. Clear writes an ordered activity-clear record and clears that owner without hiding newer activity.

Pi writes through the `attention` command that `WEZTERM_ATTENTION_ROOT` names. Where it is unset in a WezTerm pane (the command is not built, or the pane was opened before it was), Pi records nothing and shows one warning. Outside WezTerm, where `WEZTERM_PANE` is unset, it records nothing and says nothing. Its records need a launch claim. On macOS, Pi's session start claims the pane for the Pi process itself, so `pi` needs no claim step; the extension tells the writer its own pid for that. On Linux, or with self-claim switched off, a pane where `pi` was started without a claim refuses every event, Pi shows one warning, and the tab shows nothing for Pi; start Pi from a shell that claims for it, see [Mux setup](mux-setup.md).

For cached lifecycle observations, question-publication evidence and consumer-owned presentation, see the [consumer guide](consumer-guide.md).
