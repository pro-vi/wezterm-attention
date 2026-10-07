# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes, and WezTerm reflects them as colored tab indicators.

Two things write those signals. A small Rust command, `attention`, runs as a hook from Claude Code, Codex, Cursor Agent or Pi and records what a pane's agent is doing against a pane identity that survives detach, reattach and multiple mux sockets. A Lua reader in WezTerm polls those records and renders the tab. Programs other than WezTerm can read the same records: `attention bindings --json`, `attention tabs` and `attention inspect` return validated facts, so a script does not have to scrape a terminal to find out which pane an agent is in.

The records the `attention` command writes are called **v2 records** in these docs, after the manifest that defines them, `protocol/v2.json`; the name is for the record format, not a version of this project. They are the only format the plugin reads, and only the `attention` command writes them: see [Record contract](docs/record-contract.md).

The supported interface is the `attention` command — its JSON envelopes, diagnostic codes and exit codes, specified in the [consumer guide](docs/consumer-guide.md) — and the plugin's [Lua API](#public-api). The Rust crate the command is built from is not supported for use outside this repository; its items may change in any release.

Known compromises are listed in [docs/accepted-limitations.md](docs/accepted-limitations.md).

## What it looks like

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review (`Alt+B`) |

Tabs light up when an agent or script records a state for its pane—even when another pane in that tab is currently focused, and even when the tab itself is not the one you are on. Focusing a pane acknowledges only that pane's `stop` and `notify` (not a `notify` shown over its agent's finished turn while a sub-agent waits for permission); states of unfocused sibling panes remain visible until you visit them. `thinking` persists until its writer replaces or clears it, or until the TTL it was written with expires; a `review` flag persists until you press `Alt+B` again.

Only the active pane of the focused window is acknowledged. The writer's record stays in place; the plugin has the `attention` command record which activity you were shown, and only while that activity is still the pane's, so an unseen notification remains visible.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

The `review` flag is yours, not a writer's: it is a record of its own, so you can flag a pane that is mid-`thinking` or showing a `stop` without touching either. See [The review flag](#the-review-flag).

A pane can also report how many subagents are still working inside it. The tab appends that count to whatever indicator it is already showing — `✓+2` — or shows `+2` on its own when the pane has nothing else to show. See [Subagent activity](#subagent-activity).

## Install

There are two parts. The Lua plugin draws the tab bar and reads what writers record. The `attention` command is a small Rust program that agent hooks and scripts call to record what a pane's agent is doing. The plugin shows only what the command records, so both are needed.

Upgrading from 0.6? Follow [the upgrade steps in the changelog](CHANGELOG.md#upgrading-from-06).

### 1. Load the plugin

This needs WezTerm `20230320-124340-559cb7b0` or newer, the first release with `wezterm.plugin.require`. A stock release is enough; Attention depends on no patched build ([ADR 0007](docs/adr/0007-stock-wezterm-only.md)). Add the plugin before any other `format-tab-title` handler:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

`apply_to_config` adds its `Alt+B` key to `config.keys`. Assign your own `config.keys = { ... }` before this call: an assignment after it replaces the list and drops `Alt+B`.

### 2. Build the `attention` command

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

Reload the config afterwards. New panes then get `WEZTERM_ATTENTION_ROOT`, the checkout path. The plugin exports it only once the command is built: a producer that sees it runs that checkout's command, which would fail in every callback before the build. It exports `WEZTERM_ATTENTION_DIR`, the state directory, whether or not the command is built, unless `integration_root` is not an absolute path: then it exports neither variable and logs `v2 integration root is unavailable`.

`"$checkout/bin/attention" --version` prints the commit the command was built from, with `-dirty` if `src`, `protocol`, `build.rs`, `Cargo.toml` or `Cargo.lock` had uncommitted changes, new files included, when it was built. Compare it with `git -C "$checkout" rev-parse --short=12 HEAD` to see whether the build is current.

Put the command on your PATH by linking the launcher:

```sh
mkdir -p ~/.local/bin
ln -s "$checkout/bin/attention" ~/.local/bin/attention
```

The launcher follows the link to find its checkout, so it works from anywhere on PATH.

**After every `wezterm.plugin.update_all()`, run `install-cli.sh` again.** Updating replaces the Lua files with the newest commit on the repository's default branch, and leaves the previously built command in place, so the plugin can run ahead of the writer.

#### Using your own clone instead

To load the plugin from a clone you manage, run `scripts/install-cli.sh` in that clone and load it by path. `dofile` does not work: it passes no module path, and WezTerm's Lua has no `debug` library to find one.

```lua
local clone = "/absolute/path/to/wezterm-attention"
local attention = loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua")
attention.apply_to_config(config)
```

With this form, update with `git pull` and rerun `install-cli.sh` in the clone. If you keep `wezterm.plugin.require` and build the command in your own clone, pass `integration_root = "/absolute/path/to/wezterm-attention"` to `apply_to_config`; the Lua then updates through `update_all` and the command through your clone, separately.

Then register the [Claude Code](#claude-code-hooks), [Codex](#codex-hooks) and [Cursor Agent](#cursor-agent-hooks) hooks. On macOS that is enough: an agent's first session start claims its pane by itself, so zsh needs no claim step. Bash still claims each agent command it starts, and on Linux a shell claim is the only way to claim; [Mux setup](docs/mux-setup.md) covers both shells. See [Record contract](docs/record-contract.md) for precedence and [Mux pane moves](docs/mux-pane-moves.md) before moving the final pane out of a server tab.

## Render modes

There are two render modes:

| Mode | Who owns `format-tab-title` | Per-tab colors | Use when |
|------|---------------------------|----------------|----------|
| `tab` (default) | Plugin | Yes | You want it to just work |
| `manual` | You | Yes | You have a custom tab formatter |

```lua
-- Default: plugin owns everything
attention.apply_to_config(config)

-- Manual: you own format-tab-title, plugin provides helpers
attention.apply_to_config(config, { renderer = "manual" })
wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
  return ctx.default_title  -- your custom logic here
end))
```

## Custom tab titles

The plugin draws each tab as ` <number>: <indicator><base title> `, with ` · <provider>` before the closing space when `show_provider = true`. The number is left out when `config.show_tab_index_in_tab_bar = false`.

The default base title is the first of these that is not empty:

1. The tab's own title, as set by `tab:set_title()` or `wezterm cli set-tab-title`.
2. The last component of the pane's current directory. `show_directory = false` skips it.
3. The pane's title, once it has stayed the same for two polls. `settled_title_fallback = false` skips it, and the poll then samples no titles at all.
4. The pane's title as it is now.

Text from the first two sources, and the title from the last, has escape sequences and control characters removed and is cut to 256 bytes on a character boundary; a title with a control character in it is never used as the settled title.

In `tab` mode, pass a `title_formatter` to replace the base title without losing indicators:

```lua
attention.apply_to_config(config, {
  title_formatter = function(tab, ctx)
    -- ctx.default_title: the base title from the rule above
    -- ctx.tab_title, ctx.directory, ctx.settled_title: its sources, nil when empty
    -- ctx.attention: { indicator, type, color, subagents, subagents_uncertain,
    --   source, provider, review, binding_health }
    local pane = tab.active_pane
    return pane.title  -- just the pane title, no directory
  end,
})
```

`ctx.attention[1]`, `[2]` and `[3]` are the indicator, type and color, for formatters written against the positional form.

The tab's `type`, `color`, `source`, `provider` and `binding_health` are those of the pane whose type ranks highest in `priority`. `subagents` is the sum of every pane's count, and `subagents_uncertain` and `review` are true when they are true for any pane of the tab.

## Configure

All options are optional — defaults work out of the box. An unknown option, a value of the wrong type, and a `priority` or `auto_clear` that is not a list of attention types are each named once in the WezTerm log, and the default is used instead. An unknown name inside `colors` or `indicators` is named and ignored. A `title_formatter` given with `renderer = "manual"`, which draws no titles, is named and not used, and a second `apply_to_config` call is named and changes nothing:

```lua
attention.apply_to_config(config, {
  -- Render mode: "tab" | "manual". Any other value logs and means "tab".
  renderer = "tab",

  -- The state directory. The default is the first that applies:
  -- $WEZTERM_ATTENTION_DIR when set, non-empty and absolute; else
  -- $XDG_STATE_HOME/wezterm-attention when XDG_STATE_HOME is set, non-empty and
  -- absolute; else ~/.local/state/wezterm-attention. The attention command and
  -- the Pi extension resolve it the same way.
  dir = nil,

  -- Where the attention command was built, when it is not the checkout
  -- WezTerm loaded the plugin from. See Install.
  integration_root = nil,

  -- Custom base title (tab mode only; plugin adds indicators + colors around it)
  title_formatter = nil,  -- function(tab, ctx) -> string of plain text

  -- Base-title sources; see Custom tab titles.
  show_directory = true,
  settled_title_fallback = true,

  -- Append " · Claude", " · Codex", " · Cursor" or " · Pi" when the tab's indicator comes
  -- from a pane with a provider binding.
  show_provider = false,

  -- Called after each poll with what changed in a window's pane views.
  -- See docs/consumer-guide.md, "GUI view callback".
  on_view_change = nil,  -- function(change)

  -- Tab background tints per attention type
  colors = {
    thinking = "#1c1730",  -- violet tint
    stop     = "#12271c",  -- mint tint
    quiet    = "#122033",  -- blue tint
    notify   = "#240f16",  -- rose tint
    review   = "#1a1a0c",  -- gold tint
  },

  -- Tab text indicators
  indicators = {
    thinking_frames = { "◌ ", "◔ ", "◑ ", "◕ " },
    stop   = "✓ ",
    held   = "⏾ ",
    quiet  = "↪ ",
    notify = "! ",
    review = "◆ ",
  },

  -- Priority order (last = highest)
  priority = { "thinking", "review", "stop", "notify" },

  -- Visually acknowledge these types when focusing their pane
  auto_clear = { "stop", "notify" },

  -- Review toggle keybind (false to disable). Added to config.keys, so assign
  -- config.keys before calling apply_to_config.
  review_key = { key = "b", mods = "ALT" },

  -- Register the plugin's own update-status poller. See
  -- "Existing update-status handler?" below.
  auto_poll = true,

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false only if your own code already repaints titles every tick
  -- (renderer = "manual" with a title drawn from your update-status handler).
  -- The plugin then never performs ActivateTabRelative(0), saving that
  -- tab-activation pass; nothing else about polling or acknowledgement changes.
  request_redraw = true,

})
```

## Recording attention from your own tools

Write records through the `attention` command; never construct their JSON yourself. Use `attention hooks event PROVIDER EVENT` for provider callbacks, and `attention mark STATE --source NAME` for anything else, where `STATE` is `thinking`, `stop`, `notify`, `review` or `clear`. Both write into the pane's current launch claim. On macOS an agent's registered hooks claim the pane for their agent themselves (see [Claude Code hooks](#claude-code-hooks)). `attention mark`, and every producer on Linux, needs the launch id of a claiming shell, so run it from one: in bash, set `WEZTERM_ATTENTION_COMMANDS` to the space-separated command names to claim, which replaces the built-in `claude codex cursor-agent pi`, so list those too if you still want them claimed; in zsh, start it as `wezterm_attention_claim && <command>`. See [Mux setup](docs/mux-setup.md).

`--source` defaults to `manual`. `--frame`, `--label` and `--ttl-ms` are stored on the activity that `thinking`, `stop` and `notify` write: a fixed spinner frame for `thinking`, a label that `attention inspect` returns, and a time after which the activity expires; `attention mark --help` says what each does. `review` and `clear` write no activity and refuse them. `attention mark clear --source NAME` removes that source's review flag and, when the activity the tab currently shows was published by that source, clears that activity too; it reports `applied` when it did either and `skipped` otherwise. The source name `user` belongs to the plugin's review key, and every `mark` state refuses it.

### Publishing the pane id

The plugin has to match a pane's records to a pane on screen. Inside a pane,
`$WEZTERM_PANE` is the number the records are filed under. From the config
side, `pane:pane_id()` usually returns that same number — but not always.

A GUI window attached to a mux server through a unix domain numbers the panes it
displays itself. A process inside one of those panes still reads the *server's*
id from `$WEZTERM_PANE`, so the id the config sees and the id the records are
filed under are two different numbers.

So the pane publishes who it is: the `attention` command emits the
`WEZTERM_PANE` user variable and, once a launch has claimed the pane, the
`WEZTERM_ATTENTION` identity, and `pane:get_user_vars()` reads them back for
local and mux-client panes alike. The shell integration republishes them at
every prompt. The plugin also requests publication when a mux pane has no
identity, including after a reconnect (see [How it works](#how-it-works)).

**A mux-attached pane that has published nothing shows nothing.** The plugin
reads and acknowledges nothing for it, and it contributes no indicator to its
tab. Guessing from the local id would be worse than doing nothing, because that
number names some other pane.

Panes in the GUI's own domains need no published id: the `local` domain, every
exec domain, every serial port and every WSL domain. There `pane:pane_id()` and
`$WEZTERM_PANE` are the same number. Any program that prints to the terminal can set a
user variable, so on these panes the pane's own id wins: a published `WEZTERM_PANE`
that disagrees with it is ignored, and a `WEZTERM_ATTENTION` identity naming another
pane makes the pane invalid (logged once as `record_invalid`) rather than borrowing
that pane's state.

### Subagent activity

Claude Code and Codex hooks record each subagent of a pane from its
`SubagentStart`, or from its first tool call or permission request when no
start was seen, until it ends. A subagent stays counted however long it is
quiet, as while it runs one long command. It stops counting at its own
`SubagentStop`, at a Codex parent's `Stop` that comes after its last event, at
a Claude lead's `Stop` whose `background_tasks` no longer lists it, or when its
session ends. The count is independent of the pane's own activity: a
pane can carry running subagents with no activity at all — the parent agent
stopped and you acknowledged its ✓ — or alongside activity of any type.

The tab renders the count as `+N`, inside the space the indicator already
occupies:

| Activity | Subagents | Tab shows |
|----------|-----------|-----------|
| `stop` | 0 | `✓ ` |
| `stop` | 2 | `✓+2 ` |
| `stop` | 1, waiting for permission | `!+1 ` |
| `thinking` | 3 | `◑+3 ` |
| `notify` | 1 | `!+1 ` |
| none | 2 | `+2 ` with default tab colors |
| `stop` | unknown | `✓+? ` |
| none | 2 in one pane, unknown in another | `+2? ` |

`+?` means the pane's record of its subagents, or of its session's end, is
invalid, was written by a newer `attention`, or could not be read and was never
read before, so the count is unknown rather than zero.
In a tab with several panes, `+2?` means two subagents are counted and another
pane's count is unknown.

The count is what the hooks reported, not a process list. A subagent whose end
was never reported, because its `SubagentStop` hook failed or was not
registered, stays counted until the lead's next `Stop` stops listing it, or
until its session ends. See
[accepted limitations](docs/accepted-limitations.md#a-sub-agent-whose-end-is-never-reported-stays-counted-until-its-session-ends).

The count never decides which state wins the tab — priority is settled by the
states alone. But a change in the count alone is a visible change, so it
repaints the tab bar on the next poll like any other.

### The review flag

`Alt+B` flags the focused pane for your own attention. The flag is a review
record owned by `user`, which the plugin writes through the `attention` command
under the same locks as every other review writer. A press on a tab that carries
your flag on any of its panes clears it from all of them; a press on a tab that
does not flags the focused pane. The pane needs a launch claim, which the shell
integration provides; a pane no launch has claimed cannot be flagged.

The flag is a record of its own, so it coexists with whatever the pane's agent
records:

| Activity | Your flag | Tab shows |
|----------|-----------|-----------|
| none | set | `◆ ` |
| `thinking` | set | `◆ ` (the flag outranks `thinking`) |
| `stop` unacknowledged | set | `✓ ` (the activity outranks the flag) |
| `stop` acknowledged | set | `◆ ` |
| `notify` | set | `! ` |

Which one wins is the configured `priority` order, with the flag standing in for
`review`: by default `notify > stop > review > thinking`. The flag is never
acknowledged — `auto_clear` does not include `review` — so a flagged pane
comes back to `◆` once its `stop` or `notify` has been seen, and stays there
until you clear it, while that pane remains available.

A review is withdrawn only by its owner. A review another source published —
`attention mark review --source NAME`, or Pi's bus as `pi-bus` — also shows
`◆`, and `Alt+B` leaves it; `attention mark clear --source NAME` withdraws it.


For an agent session (Cursor Agent never carries it: it binds a resumed session at its first prompt, with source `clear`),
Attention carries your flag when the session registers with `resume` in a new pane after its old socket is removed or replaced, or
after its GUI is proven gone. Closing a pane on a live server does not carry
it. Only your `user` flag moves; other review owners and activity stay behind.
The newest prior binding must still be selected by its old pane. A newer retained,
unflagged binding prevents an older flag from returning.

The first eligible resume consumes the old flag before setting the new one.
Another resume or a repeated registration does not duplicate it or restore it
after you clear it. If Attention stops between those two writes, the flag can
be lost. If `attention sweep --apply` already removed the old records, there
is nothing to carry. Flags on plain claimed commands remain tied to their
pane. A provider that delays resume registration carries its flag only when
that registration arrives.
## Existing update-status handler?

By default, the plugin registers its own `update-status` handler to poll pane records. If you already have one (e.g., for a git status bar), use manual polling instead:

```lua
attention.apply_to_config(config, { auto_poll = false })

-- Then in your existing update-status handler:
wezterm.on('update-status', function(window, pane)
  attention.poll(window, { active_pane = pane })  -- redraw transport when no current pane is available
  -- ... your git status bar, battery, etc.
end)
```

## Public API

The plugin exposes functions for use in your own WezTerm Lua code:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

-- The id a pane's records are filed under: in one of the GUI's own domains
-- (local, exec, serial, WSL) its pane id; elsewhere the server pane id it
-- published (WEZTERM_ATTENTION, else WEZTERM_PANE); nil when it published neither.
local marker_id = attention.pane_marker_id(pane)

-- Read cached attention state: returns (type, frame) or nil.
-- state is the effective type: "review" when the pane's review flag outranks
-- its activity, else the activity's own type; nil when neither is in effect, as
-- when the activity was acknowledged or only the pane's subagents are running.
-- frame is the activity's frame; the `frame` row of docs/consumer-guide.md says
-- where it comes from. The call returns a single nil when the id is seen at more
-- than one full pane address; get_attention_view tells them apart.
local state, frame = attention.get_attention(marker_id)

-- Read seventeen cached base fields plus independent lifecycle evidence, without
-- I/O: type and frame as above, the activity's source, the subagent count and
-- whether it could be read, the review flag, the provider binding and more; see
-- docs/consumer-guide.md. Nested returned values do not share mutable state with
-- the plugin cache.
local view = attention.get_attention_view(pane)

-- Poll manually (for auto_poll = false)
attention.poll(window, { active_pane = pane })

-- Wrap a title function with attention decoration (for renderer = "manual")
wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
  -- ctx is the same table title_formatter receives; see Custom tab titles
  return ctx.default_title
end))
```

## Pi extension

Install this repository as a Pi package:

```bash
pi install git:github.com/pro-vi/wezterm-attention
```

The extension registers no commands and leaves print mode alone. It forwards `session_start`, `input`, `agent_start`, `tool_execution_start`, `tool_execution_end`, `message_end`, `session_before_compact`, `session_compact`, `agent_settled`, `session_shutdown` and the `wezterm-attention:mark` bus through one serialized writer queue. `agent_end` is deliberately not treated as the end of a turn, because Pi can retry or continue after it. The writer processes use Node's built-in child-process API, as Pi's Node runtime does.

Other extensions may emit `thinking`, `stop`, `notify`, `review`, or `clear` on the bus. Review uses the `pi-bus` owner. Clear writes an ordered activity-clear record and clears that owner without hiding newer activity.

Pi writes through the `attention` command that `WEZTERM_ATTENTION_ROOT` names. Where it is unset in a WezTerm pane (the command is not built, or the pane was opened before it was), Pi records nothing and shows one warning. Outside WezTerm, where `WEZTERM_PANE` is unset, it records nothing and says nothing. Its records need a launch claim. On macOS, Pi's session start claims the pane for the Pi process itself, so `pi` needs no claim step; the extension tells the writer its own pid for that. On Linux, or with self-claim switched off, a pane where `pi` was started without a claim refuses every event, Pi shows one warning, and the tab shows nothing for Pi; start Pi from a shell that claims for it, see [Mux setup](docs/mux-setup.md).

For cached lifecycle observations, question-publication evidence and consumer-owned presentation, see the [consumer guide](docs/consumer-guide.md).

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

Merge these into any hooks you already have, and keep them synchronous: do not add `async: true` or `asyncRewake: true`, either of which lets one subagent's events arrive out of order, so that a subagent can stay counted after it stopped. `SubagentStart` counts a subagent from its start, and again when it resumes, which Claude Code 2.1.283 reports as a new start under the same id. `SubagentStop` removes that same subagent. A root `Stop` removes only the subagents its `background_tasks` no longer lists, because background subagents can outlive it: Claude Code 2.1.283 sent the lead's `Stop` while a subagent was still running, and 2.1.284 lists it there. A `StopFailure` (the turn ended on an API error) shows `notify`.

A lead Stop can optionally run an application-owned check before its finish activity is recorded:

```sh
WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Stop \
  --hold-check jev=/absolute/application/stop-hold
```

Only a non-empty native `background_tasks` array with available reply text runs the check. Its input contains the checked scope, reply and unchanged array; neither content is stored by Attention. A validated hold shows fixed `⏾` with thinking color and is not automatically acknowledged. While a sub-agent waits for permission, no check runs, the turn end is recorded unheld, and the tab shows `notify` until the wait ends. Timeout or failure supplies no hold. The checks share a 2000 ms outer budget; ordinary consumer and native-work time are additional. See [the exact input, output and record contract](docs/consumer-guide.md#pre-write-hold-checks) and [ADR 0005](docs/adr/0005-application-hold-notes-on-recorded-turn-ends.md). Update matching readers and writers before activation; this repository does not register or install the check for you.

`--quiet-check NAME=/absolute/executable` runs the same way on a lead Claude or Codex `Stop`, for an application that can prove nobody needs to be called, such as a controller that submitted the prompt itself. A validated quiet note shows `↪` with a blue tint and records `turn_end.held` false; a waiting permission or a held turn end takes precedence, and a failure, timeout or missing lifecycle history leaves the ordinary turn end. See [quiet checks in the consumer guide](docs/consumer-guide.md#pre-write-hold-checks).

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

`SubagentStart` counts a subagent from its start. Child attribution needs matching native `agent_id` values; see [contact evidence](docs/reviews/lifecycle-contact-results.md) for the paths that were exercised. A root `Stop` writes the lead stop and stops counting every subagent whose last event came before it. Codex (source at commit `985cf47a4`) does not make a parent wait for its subagents, but in the sessions recorded with Codex 0.157.1 none worked after its parent's `Stop`; one that does is counted again at its next event (see [accepted limitations](docs/accepted-limitations.md#the-sub-agent-count-depends-on-how-claude-code-and-codex-send-hooks)). An `Interrupt` clears the tab's activity for that session, because Codex runs no `Stop` after one. A child's `PermissionRequest` shows `notify` on the tab, since Codex has no `Notification` hook.

## Cursor Agent hooks

Cursor Agent (`cursor-agent`) reads command hooks from `~/.cursor/hooks.json`, and from `.cursor/hooks.json` in a project. `attention hooks describe --provider cursor --json` lists the rows; the same command form applies as for Claude Code, for the same reasons, and so does the advice to register the link on your PATH. Everything below about what Cursor sends was checked with `cursor-agent` 2026.10.01-e373342 on macOS.

```json
{
  "version": 1,
  "hooks": {
    "sessionStart":       [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor sessionStart" }],
    "sessionEnd":         [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor sessionEnd" }],
    "beforeSubmitPrompt": [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor beforeSubmitPrompt" }],
    "preToolUse":         [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor preToolUse" }],
    "postToolUse":        [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor postToolUse" }],
    "postToolUseFailure": [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor postToolUseFailure" }],
    "preCompact":         [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor preCompact" }],
    "stop":               [{ "command": "WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event cursor stop" }]
  }
}
```

Register these eight and no others. A command registered for `afterAgentThought` made `cursor-agent` 2026.10.01-e373342 print `Connection lost, reconnecting`; the other hooks send nothing Attention reads.

| Cursor hook | What the pane shows |
|---|---|
| `sessionStart` | Binds the pane, and on macOS claims it. Cursor sends no start source, so every start counts as `startup` |
| `beforeSubmitPrompt`, `preToolUse` | `thinking` |
| `stop` with status `completed` | `stop` |
| `stop` with status `aborted` or `error` | Clears the pane's activity |
| `sessionEnd` | Ends the binding |
| `postToolUse`, `postToolUseFailure`, `preCompact` | Lifecycle observations only |

Cursor sends less than Claude Code and Codex do, so:

- **A pane waiting for you shows `thinking`, never `notify`.** Cursor runs no hook for a command-approval prompt or for the `AskQuestion` tool.
- **Sub-agents are not counted.** A `Task` sub-agent sends no start or stop hook, and its tool hooks carry a `session_id` of their own, which Attention ignores (`claim_stale`).
- **Esc and a failed turn both clear the pane.** Cursor sends two `stop` hooks for one Esc, `error` and `aborted`, in no fixed order, and a turn that ends on a real error looks the same as an Esc.
- **After `/new`, and for a resumed session, the pane follows the conversation.** Cursor sends no `sessionStart` for either (for a resumed one, `--resume`, `--continue`, `resume` and `ls`, from cursor-agent's code and not from a run), and no `sessionEnd` for the old conversation after `/new`. The conversation's first prompt binds it, ends the conversation it replaced, and claims the pane as a session start would: on macOS, a pane nobody claimed or one whose earlier agent is gone, never a live agent's or a shell's.

A headless `cursor-agent -p` run sends no `beforeSubmitPrompt` and no `stop`: its pane shows `thinking` from the first tool call, and ending the run does not clear it ([details](docs/accepted-limitations.md#cursor-agent-sends-fewer-hooks-than-claude-code-and-codex)). The review flag is not carried across a resumed session: its first prompt binds it with source `clear`, not `resume`. `--hold-check`, `--quiet-check`, `--include-reply` and `--include-prompt` do not apply to Cursor. Evidence and the rest: [accepted limitations](docs/accepted-limitations.md#cursor-agent-sends-fewer-hooks-than-claude-code-and-codex).

In bash, if you start it as `agent`, set `WEZTERM_ATTENTION_COMMANDS='claude codex cursor-agent agent pi'`: the variable replaces the built-in list.

## Other use cases

- **Build systems** — write `notify` on failure, `stop` on success
- **Test runners** — animated `thinking` while running, `stop` or `notify` on completion
- **Long-running scripts** — any background job that wants your attention when done
- **Manual triage** — `Alt+B` to flag tabs for review during code review sessions

## Built on Attention

These are built outside this repository from its public facts; each names what it reads.

- **A prompt-cache countdown in the status bar.** It dates the pane's last request from the newest `written_at_unix_ns` among the `lifecycle.observations` of `get_attention_view(pane)` whose `actor.kind` is `lead`, and keys the countdown by `binding_id`, so a new session starts a new one. The timestamp is a decimal string of nanoseconds, too large for a Lua number.
- **A pane jump picker that names the agent in every pane.** It reads `provider` from `get_attention_view(pane)`, and trusts it only when `binding_phase` is `active` and `reader_confidence` is `confirmed`.
- **An unanswered-question highlight.** It is driven by `on_view_change` and looks for entries in `lifecycle.requests` whose `kind` is `question`. [`examples/follow-up.lua`](examples/follow-up.lua) is the starting point.
- **An exact reply relay between panes.** An executable registered on the `Stop` hook with `--consumer … --include-reply` receives the agent's final message as `reply.text`, exactly as the provider sent it, together with the scope of the pane it came from.

## The drawn tab order

A WezTerm window attached to a mux server mirrors the server's tabs under numbers of its own, and those are the numbers the tab bar prints. They are not the order of `wezterm cli list`. Nothing outside the GUI process can see the drawn order, so the tab bar publishes it — one file per window, under the state directory, named by the incarnation of the GUI's own mux socket and the window id:

```text
<state directory>/tabs/<incarnation id>-<window id>.json
```

```json
{ "schema": 2,
  "window_id": 0,
  "published_at_ms": 1789884000123,
  "source": { "socket_path": "/…/gui-sock-4946", "realm_id": "…", "incarnation_id": "…" },
  "tabs": [ { "number": 11, "text": " 11: ✓ braid ", "marker_ids": ["v2:…:…:16"] } ] }
```

`source` names the GUI that drew the window, because a window id means something only inside one GUI process. The plugin learns that identity by asking the `attention` command shortly after startup. A window's first order is held until that answer arrives and then written under its source; when the command is not built, or no answer can come, it is written as a schema-1 file at `tabs/<window id>.json` with no `source`. A window keeps the one file it was first written to for as long as it is open, with one exception: after a config reload that finds the command built, a window first written without a `source` is written under its source, and the plugin removes the schema-1 file it wrote for that window unless another GUI has rewritten it since.

`number` is the number the bar printed, `text` is the whole string it drew (escape sequences and control characters removed, cut to 256 bytes, and a spinner always shown at its first frame so the file does not change every second), and `marker_ids` are the IDs the plugin already uses for those panes — already translated out of the window's local numbering, because only the window could translate them. A pane that a launch has claimed is `v2:<realm_id>:<incarnation_id>:<pane_id>`; a pane no launch has claimed is its canonical decimal pane id. A pane appears once a poll has identified it. The file is written when a window's composed list changes and at no other time, so `published_at_ms` says when the bar last drew something different.

Read it with `attention tabs`, which returns every window in the same JSON envelope as `bindings`. **It is honest about when it was written, not guaranteed current**: nothing refreshes it while the bar is idle, and no consumer should act on a number it has not checked. Use it to describe tabs and to resolve "the second `api` tab"; to act on one, ask the GUI, where `mux_window:tabs_with_info()` returns the drawn order live.

The publisher is the handler the plugin registers, so `renderer = "manual"` — where your own formatter draws the tabs and the plugin registers nothing — publishes nothing. `wrap_title_formatter` does not publish either: with both handlers registered, the same window would draw two different texts and each repaint would rewrite the file twice.

Every setup publishes, including a plain local WezTerm where the drawn number equals the derived one. A consumer cannot tell a simple setup from a publisher that is not running — the file is absent in both — and deriving is right in one case and wrong in the other.

## How it works

The plugin uses a **poller/renderer split** to avoid blocking WezTerm's GUI thread:

1. **Poller** (`update-status` event) — runs on WezTerm's `config.status_update_interval` (default 1000ms). Reads each pane's records, then updates an in-memory cache. It acknowledges the focused window's current active pane, by running the `attention` command once for each new activity shown there, and again after a backoff wait when that run could not get the pane's locks or did not answer, and asks WezTerm to rebuild the tab bar when a pane's effective attention changes. It drops the cache entry of any pane that was in the window on the previous tick and is gone now — unless every pane of that pane's domain went at once, which is a domain detach rather than a close, and those panes are still alive on the server. It deletes nothing.
2. **Renderer** (`format-tab-title` event) — fires on every tab repaint (mouse hover, key press, redraws). Reads only from the cache — no file reads, instant returns. It writes one file, and only when a window's whole bar draws something different from the last time it drew: the [drawn tab order](#the-drawn-tab-order), which no other process can see. A repaint that draws the same thing composes the list, compares it, and touches nothing.

WezTerm rebuilds tab titles when something it knows about changes, and a record appearing on disk is not one of those things. When the poller sees a pane's effective attention change, it performs `ActivateTabRelative(0)` on the focused window. That re-activates the already-selected tab and makes WezTerm recompute every tab title. The plugin does not write either status string, the window title, or any user title.

Set `request_redraw = false` to switch that request off, for a host whose own `update-status` handler already redraws the titles it owns. The redraw action can pass through WezTerm's normal tab-activation path, including terminal focus reporting. It therefore runs only when the window has keyboard focus and a valid active pane. Generated spinner frames use one-second wall-clock buckets, so polls induced by the action see the same frame and terminate. If an action fails, that window logs once and stops requesting redraws.

For mux domains, a pane without a published identity starts a publication
schedule. After two polls see a stable pane count in a window, its first attempt
excludes the server pane IDs currently published by live panes on that socket.
The exclusion set names the socket incarnation; if that incarnation changed
before the command started, it publishes all panes. Duplicate IDs, unreadable
panes and claimed identities from another socket scope do not exclude anything.
With no usable exclusions, the first attempt also publishes all panes.

Published variables can be copied from another terminal, so a selective attempt
is only an optimization. While identities remain missing, later attempts publish
the whole realm after waits of 2, 5, 10, then 30 seconds repeatedly. Pane-count
changes do not grant another selective attempt until the old schedule retires.
Publication writes terminal output, never pane input, and retains the socket and
tty checks around those writes. The child PATH includes `wezterm.executable_dir`.

For CLI callers, `hooks publish --socket PATH --incarnation-id ID --except-pane N`
excludes a canonical server pane ID; repeat `--except-pane` for more IDs. Both
`--socket` and `--incarnation-id` are required for exclusions. The JSON report's
`attempted` counts selected panes and `skipped` counts failed publications;
excluded panes are neither attempted nor failures.

The Lua implementation is split by responsibility under `plugin/`: protocol validation, record reading, runtime polling, tab-order publication and error reporting, title sampling, and formatting. `plugin/init.lua` owns configuration, composition, callback registration, and the public API.

## Troubleshooting

**Indicators not showing?**
- Run `attention doctor`: it checks the state directory's permissions and records, the mux socket, the pane's environment, agent processes and the command's version, and whether a Codex shared server is running for hooks that call Attention.
- If the window is attached to a mux server (`wezterm connect`, a unix domain), check the pane publishes its identity: `wezterm cli list --format json` shows the server-side pane id, and the pane must emit it as the `WEZTERM_PANE` user var. See [Publishing the pane id](#publishing-the-pane-id). Without it the plugin deliberately does nothing for that pane.
- Check the plugin found the command: without it the WezTerm log says `libexec/attention-rs is missing`, and nothing is recorded.
- Ensure your hooks write to the same state directory as the plugin's `dir` setting (see [Configure](#configure) for the order).
- A `+N` with no glyph beside it is the [subagent count](#subagent-activity) for a pane whose own activity is gone or already acknowledged. A `+?` is a pane whose subagent count could not be read.
- A ◆ that `Alt+B` does not clear is a review another source published; `attention mark clear --source NAME` withdraws it.
- `status_update_interval` defaults to 1000ms; indicators update on this interval. Lower it if indicators feel slow — the redraw request rides on the same tick.
- A Codex tab that shows nothing, or shows another pane's session: Codex 0.157 and later runs sessions in a shared background server that is in no pane, and Attention refuses their events with `session_detached` (or `claim_stale`, after a refused session start in a pane no shell claimed). Start Codex with `--no-daemon`; see [Codex hooks](#codex-hooks). `attention doctor` names such a server as `session_detached`, with the `codex_home` it serves. The refusal goes to the hook's stderr, and Codex 0.157.1 does not show the stderr of a hook that exits 0, which a refusal does, so the reason is neither on screen nor in Codex's logs. To read it, append `2>>/tmp/attention-hook.log` to the hook's command for a while; the command stays one `exec`.
- A resumed Codex session's tab shows nothing until you send its first message. Codex 0.157.1 runs `SessionStart` when the first turn starts, not when the session resumes, so this is not a refusal.

**Indicators appear only when you switch tabs?**
- In `renderer = "manual"` mode, pass the event pane: `attention.poll(window, { active_pane = pane })`. The plugin resolves `window:active_pane()` at use time; the event pane is used only when the current pane is unavailable.
- Check the WezTerm error log. A failed redraw action is logged once for that window; polling and acknowledgement continue.

**Tab titles look wrong?**
- WezTerm only runs the **first** registered `format-tab-title` handler. If you have your own handler, set `renderer = "manual"` and use `wrap_title_formatter()` or the plugin API. Two handlers cannot coexist.
- Use `title_formatter` to customize the base title while keeping the plugin's indicators.

**Alt+B not working?**
- Check for keybind conflicts. Move the binding by setting `review_key = { key = "...", mods = "..." }`. The default, `Alt+B`, also moves back one word in bash 3.2 and in zsh 5.9's emacs mode, so choose another key if you use that.
- It works on a pane an agent is running in: the flag is a record of its own. If the tab still shows `✓` or `!` after a press, that activity outranks the flag; the ◆ appears once you have seen it.
- On a pane no launch has claimed, such as a shell without the integration, a press that would flag it is refused and logged once, because no reader would show the flag. A press there on a tab that carries your flag still clears it. A mux-attached pane that has not published its identity refuses every press, since its tab cannot be found.
- A press runs the `attention` command. When it refuses, the reason is logged once for each kind of refusal on that pane (`attention plugin set-review failed for pane N: ...`) and the tab does not change: `claim_stale` means the pane's launch claim is not the launch the pane last published, which the next prompt republishes. `probe_unavailable` means a hook held the pane's records at that moment; press again. A command built before the plugin was updated answers nothing: the focused tab shows `⚠ rebuild attention` and the log says the command may predate the plugin. Run `scripts/install-cli.sh` again; the warning goes once a command answers.

**Typing lag or high GUI CPU with many tabs?**
- WezTerm rebuilds the whole tab bar whenever any pane sets its title, and each rebuild runs the plugin's `format-tab-title` once for every tab (upstream WezTerm `cab251610`). Agent titles rarely reach the screen, because a tab title or the pane's directory is drawn first (see [Custom tab titles](#custom-tab-titles)), so turning them off removes rebuilds you never see the result of.
- Claude Code (checked on 2.1.291): set `"env": { "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1" }` in `~/.claude/settings.json`. The cost: a tab with no tab title and no known directory then shows the shell's title instead of the session topic.
- Codex (checked against its source at `985cf47a4`): in `$CODEX_HOME/config.toml`, give `[tui]` a `terminal_title` list without `"activity"`, for example `terminal_title = ["project-name"]`. The `"activity"` item is a spinner, so it rewrites the title while Codex works.

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).

## License

MIT
