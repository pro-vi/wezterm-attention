# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes, and WezTerm reflects them as colored tab indicators.

Two things write those signals. A small Rust command, `attention`, runs as a hook from Claude Code, Codex or Pi and records what a pane's agent is doing against a pane identity that survives detach, reattach and multiple mux sockets. A Lua reader in WezTerm polls those records and renders the tab. Programs other than WezTerm can read the same records: `attention bindings --json`, `attention tabs` and `attention inspect` return validated facts, so a script does not have to scrape a terminal to find out which pane an agent is in.

The records the `attention` command writes are called **v2 records** in these docs. The older format, one small JSON file per pane id, is called **v1 flat markers**. Flat markers still work as input: they were the whole protocol before, several tools still write them, and the reader keeps accepting them. Attention's own writers no longer produce them, and in a pane that has v2 records they are not shown — see [Producer paths](#producer-paths-v2-records-and-v1-flat-markers). These names are for the two record formats, not for versions of this project.

Known compromises are listed in [docs/accepted-limitations.md](docs/accepted-limitations.md).

## What it looks like

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review (`Alt+B`) |

Tabs light up when a background process writes a marker—even when another pane in that tab is currently focused, and even when the tab itself is not the one you are on. Focusing a pane acknowledges only that pane's `stop` and `notify`; markers from unfocused sibling panes remain visible until you visit them. `thinking` persists until its writer removes it or its TTL expires; a `review` flag persists until you press `Alt+B` again.

Only the active pane of the focused window is acknowledged. The writer-owned marker stays in place; the plugin records the exact displayed identity in a `.ack` sidecar, so unseen notifications remain visible.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

The `review` flag is yours, not a writer's: it lives in its own `.review` file beside the marker, so you can flag a pane that is mid-`thinking` or showing a `stop` without touching either. See [The review flag](#the-review-flag-the-review-sidecar).

A pane can also report how many subagents are still working inside it. The tab appends that count to whatever indicator it is already showing — `✓+2` — or shows `+2` on its own when the pane has no marker left. See [Subagent activity](#subagent-activity-the-agents-sidecar).

## Install

There are two parts. The Lua plugin draws the tab bar and reads what writers record. The `attention` command is a small Rust program that agent hooks and scripts call to record what a pane's agent is doing. The plugin works on its own with v1 flat markers; v2 records need the command.

### 1. Load the plugin

This needs WezTerm `20230320-124340-559cb7b0` or newer, the first release with `wezterm.plugin.require`. Add the plugin before any other `format-tab-title` handler:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

`apply_to_config` adds its `Alt+B` key to `config.keys`. Assign your own `config.keys = { ... }` before this call: an assignment after it replaces the list and drops `Alt+B`.

### 2. Build the `attention` command

`wezterm.plugin.require` clones this repository into WezTerm's own plugin directory, and by default the plugin looks for the command only in that copy. Build it there. This needs a POSIX system and a Rust toolchain with `cargo` (the minimum version is `rust-version` in `Cargo.toml`). WezTerm creates the directory the first time it loads a config that requires the plugin, so start WezTerm once first.

```sh
case "$(uname)" in
  Darwin) plugins="$HOME/Library/Application Support/wezterm/plugins" ;;
  *) plugins="${XDG_DATA_HOME:-$HOME/.local/share}/wezterm/plugins" ;;
esac
checkout="$plugins/httpssCssZssZsgithubsDscomsZspro-visZswezterm-attention"
sh "$checkout/scripts/install-cli.sh"
```

If the plugin cannot find the command, it logs once per config load, naming the path it checked: `.../libexec/attention-rs is missing, so panes get no WEZTERM_ATTENTION_ROOT ...`. Open the WezTerm debug overlay (`Ctrl+Shift+L`) to read it.

Reload the config afterwards. New panes then get `WEZTERM_ATTENTION_ROOT`, the checkout path. The plugin exports it only once the command is built, because a producer that sees it writes through the command instead of writing a v1 flat marker. It always exports `WEZTERM_ATTENTION_DIR`, the state directory.

`"$checkout/bin/attention" --version` prints the commit the command was built from, with `-dirty` if the tree had uncommitted changes. Compare it with `git -C "$checkout" rev-parse --short=12 HEAD` to see whether the build is current.

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

Continue with [Mux setup](docs/mux-setup.md) for Bash or zsh launch claims, then register the [Claude Code](#claude-code-hooks) and [Codex](#codex-hooks) hooks. See [Record contract](docs/record-contract.md) for precedence and [Mux pane moves](docs/mux-pane-moves.md) before moving the final pane out of a server tab.

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

Text from the first two sources, and the title from the last, has control characters removed and is cut to 256 bytes on a character boundary; a title with a control character in it is never used as the settled title.

In `tab` mode, pass a `title_formatter` to replace the base title without losing indicators:

```lua
attention.apply_to_config(config, {
  title_formatter = function(tab, ctx)
    -- ctx.default_title: the base title from the rule above
    -- ctx.server_title, ctx.directory, ctx.settled_title: its sources, nil when empty
    -- ctx.attention: { indicator, type, color, subagents, source, provider, review, binding_health }
    local pane = tab.active_pane
    return pane.title  -- just the pane title, no directory
  end,
})
```

`ctx.attention[1]`, `[2]` and `[3]` are the indicator, type and color, for formatters written against the positional form.

## Configure

All options are optional — defaults work out of the box. An unknown option, or a value of the wrong type, is named once in the WezTerm log and the default is used instead:

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
  title_formatter = nil,  -- function(tab, ctx) -> string

  -- Base-title sources; see Custom tab titles.
  show_directory = true,
  settled_title_fallback = true,

  -- Append " · Claude", " · Codex" or " · Pi" when the tab's indicator comes
  -- from a pane with a provider binding.
  show_provider = false,

  -- Called after each poll with what changed in a window's pane views.
  -- See docs/consumer-guide.md, "GUI view callback".
  on_view_change = nil,  -- function(change)

  -- Tab background tints per attention type
  colors = {
    thinking = "#1c1730",  -- violet tint
    stop     = "#12271c",  -- mint tint
    notify   = "#240f16",  -- rose tint
    review   = "#1a1a0c",  -- gold tint
  },

  -- Tab text indicators
  indicators = {
    thinking_frames = { "◌ ", "◔ ", "◑ ", "◕ " },
    stop   = "✓ ",
    notify = "! ",
    review = "◆ ",
  },

  -- Priority order (last = highest)
  priority = { "thinking", "review", "stop", "notify" },

  -- Visually acknowledge these types when focusing their pane
  auto_clear = { "stop", "notify" },

  -- Stale marker cleanup by type, in milliseconds.
  -- Prevents zombie busy tabs if a process exits without clearing.
  -- Set to false to disable the config-driven sweep. Note: markers that carry
  -- their own ttl_ms (e.g. the Pi extension's `thinking` markers) still expire
  -- on that embedded TTL — false only turns off this type-based cleanup.
  stale_after_ms = { thinking = 30 * 60 * 1000 },

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

## Producer paths: v2 records and v1 flat markers

Write v2 records through the `attention` command; never construct their JSON yourself. Use `attention hooks event PROVIDER EVENT` for provider callbacks, and `attention mark STATE --source NAME` for anything else, where `STATE` is `thinking`, `stop`, `notify`, `review` or `clear`. Both need the pane's current launch claim, so run the producer from a claiming shell: in bash, add its command name to `WEZTERM_ATTENTION_COMMANDS`; in zsh, start it as `wezterm_attention_claim && <command>`. See [Mux setup](docs/mux-setup.md).

`--source` defaults to `manual`. `attention mark clear --source NAME` removes that source's review flag and, when the activity the tab currently shows was published by that source, clears that activity too; it reports `applied` when it did either and `skipped` otherwise. The source name `user` belongs to `Alt+B` and every `mark` state refuses it.

**In a pane with v2 records, v1 flat markers are not shown.** Once a launch claim has been published in a pane (the shell integration republishes it at every prompt), the reader takes that pane's state only from v2 records and ignores a flat marker written under the same pane id. A script that writes flat markers into a pane where you also run claimed agents should switch to `attention mark`.

The flat-file format below remains supported for other writers. Attention's writers do not emit it. Installing the `attention` command does not change a hook you registered earlier that writes flat files; re-register it through `attention hooks event` (see [Claude Code hooks](#claude-code-hooks)).

### v1 flat-marker protocol

Any process running inside WezTerm can write a v1 flat marker. The compatibility contract is:

1. **Write** a JSON file to `$WEZTERM_ATTENTION_DIR/<WEZTERM_PANE>`. The plugin exports `WEZTERM_ATTENTION_DIR` to every pane; outside one, use the state directory from [Configure](#configure).
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position. If omitted for `thinking`, the plugin animates it during polling.
4. **Recommended:** `publication_id` is a new non-empty string for every publication. It lets an identical `stop` or `notify` payload become visible again after the previous publication was acknowledged. Without it, the plugin uses the exact JSON bytes as the legacy identity.
5. **Optional:** `updated_at` or `updated_at_ms` records when the marker was refreshed. Seconds and milliseconds are both accepted.
6. **Optional:** `ttl_ms` overrides stale cleanup for that marker. By default, stale `thinking` markers clear after 30 minutes.
7. **Optional:** a `<WEZTERM_PANE>.agents` sidecar reports how many subagents are working in the pane — see [Subagent activity](#subagent-activity-the-agents-sidecar) below.
8. **Plugin-owned:** `<WEZTERM_PANE>.ack` records the marker publication you have already been shown, and `<WEZTERM_PANE>.review` is the `Alt+B` flag — see [The review flag](#the-review-flag-the-review-sidecar). Writers never touch either one.
9. **Cleanup** is automatic for v1 flat markers the poller still owns. The poller removes a marker whose pane it saw on the previous tick and does not see now (WezTerm emits no pane-close event, so a vanished pane is how a closed pane is detected), and removes a marker whose stale TTL has expired. A closed pane loses everything — marker, `.ack`, `.agents` and `.review`. An expired marker loses only itself and its `.ack`: the subagent sidecar and your review flag keep their own lifetimes and neither of them aged out because a spinner did. Focusing a pane writes an acknowledgement sidecar instead of removing writer-owned state. Flat files that development builds of Attention wrote beside v2 records are collected when exactly one valid v2 claim names that pane id: preview with `attention sweep --json`, then `attention sweep --apply --operation-id "$(uuidgen | tr A-Z a-z)"`. The operation id must be a canonical lowercase UUID, and a new one for every run. Unique claim is not a provenance check and does not ask whether a live writer occupies the number. That command never removes `.review`.

The `WEZTERM_PANE` environment variable is injected by WezTerm into every shell it spawns. That's the pane's unique ID — always a non-negative integer. Validate it (`/^\d+$/`) before building a path from it: a stray `../…` value would otherwise write to, or delete, a file outside the marker directory. Every example and fragment below enforces this.

### Publishing the pane id

The plugin has to match a marker file to a pane on screen. Inside a pane,
`$WEZTERM_PANE` is the number the marker is named after. From the config side,
`pane:pane_id()` usually returns that same number — but not always.

A GUI window attached to a mux server through a unix domain numbers the panes it
displays itself. A process inside one of those panes still reads the *server's*
id from `$WEZTERM_PANE` and writes its marker under that name, so the id the
config sees and the id the file is named after are two different numbers. Every
read, write, acknowledgement and removal the plugin performs for that pane would
land on the wrong file.

The fix is for the pane to publish its own id as the `WEZTERM_PANE` user
variable, which `pane:get_user_vars()` reads back for local and mux-client panes
alike. In zsh, publish it on every prompt so it survives a reattach:

```zsh
__wezterm_publish_pane() {
  [[ -n "$WEZTERM_PANE" ]] || return
  printf '\e]1337;SetUserVar=WEZTERM_PANE=%s\a' "$(printf %s "$WEZTERM_PANE" | base64)"
}
precmd_functions+=(__wezterm_publish_pane)
```

The value is base64-encoded because that is what the OSC 1337 `SetUserVar`
sequence expects. A hook or agent that writes markers can emit the same sequence
to `/dev/tty`, which covers panes whose shell has not been reloaded yet.

**Without this, a mux-attached GUI cannot address markers at all.** The plugin
treats a remote pane that has published nothing as having no marker id: it reads,
writes, acknowledges and removes nothing for that pane, and the pane contributes
no indicator to its tab. Guessing from the local id would be worse than doing
nothing, because that number names some other pane's marker file.

Panes in the GUI's own domains need none of this: the `local` domain, every exec
domain, every serial port and every WSL domain. There `pane:pane_id()` and
`$WEZTERM_PANE` are the same number. Any program that prints to the terminal can set a
user variable, so on these panes the pane's own id wins: a published `WEZTERM_PANE`
that disagrees with it is ignored, and a `WEZTERM_ATTENTION` identity naming another
pane makes the pane invalid (logged once as `record_invalid`) rather than borrowing
that pane's state.

### Subagent activity: the `.agents` sidecar

A pane can have more than one thing running in it. Beside the marker file, the
writer maintains `~/.local/state/wezterm-attention/<WEZTERM_PANE>.agents`:

```json
{
  "agents": {
    "agent-4f2a": { "type": "general-purpose", "last_ms": 1756890000000 },
    "agent-91bd": { "type": "Explore",         "last_ms": 1756890042000 }
  }
}
```

One entry per subagent that has run a tool call from that pane, keyed by
whatever id the writer uses for it. `last_ms` is when that subagent last ran a
tool call, in epoch milliseconds. `type` is the writer's own label for the
subagent; the plugin carries it in the file but does not interpret it.

**The writer owns this file.** The plugin only reads it, and deletes it when the
pane closes or `remove_marker` is called. A marker that expires by TTL leaves it
alone — the subagents are still running.

An entry is **live for ten minutes** after its `last_ms`. Older entries are
ignored, so a writer that never cleans up still stops reporting a subagent that
has gone quiet. An absent, empty, or unparseable file counts as zero live
subagents and never disturbs the marker beside it.

The file is **independent of the marker.** A pane can carry live subagents with
no marker at all — the parent agent stopped and you acknowledged its ✓ — or
alongside a marker of any type.

The tab renders the live count as `+N`, inside the space the indicator already
occupies:

| Marker | Live subagents | Tab shows |
|--------|----------------|-----------|
| `stop` | 0 | `✓ ` |
| `stop` | 2 | `✓+2 ` |
| `thinking` | 3 | `◑+3 ` |
| `notify` | 1 | `!+1 ` |
| none | 2 | `+2 ` with default tab colors |

The count never decides which marker wins the tab — priority is settled by the
markers alone. But a change in the count alone is a visible change, so it
repaints the tab bar on the next poll like any other.

### The review flag: the `.review` sidecar

`Alt+B` flags a pane for your own attention. The flag is a file of its own:

```
~/.local/state/wezterm-attention/<WEZTERM_PANE>.review
{"publication_id":"1756890000000-a1b2c3d4-7"}
```

Its **presence is the flag**; nothing reads the body, so a truncated write still
counts as flagged rather than silently losing a flag you set by hand. The plugin
writes it the same way it writes an acknowledgement — to a per-process temp name,
then renamed into place — and removes it when you press `Alt+B` again or when the
pane closes.

It is a separate file because it is a separate claim. The marker file belongs to
whatever process runs in the pane, and on a pane you actually want to flag there
is almost always one there: an agent's `thinking`, or the `stop` it left behind.
The flag used to be written into that file as `{"type":"review"}`, guarded so it
would never overwrite a process marker — which meant `Alt+B` silently did nothing
on exactly those panes. As a sidecar it coexists:

| Marker file | `.review` | Tab shows |
|-------------|-----------|-----------|
| none | present | `◆ ` |
| `thinking` | present | `◆ ` (the flag outranks `thinking`) |
| `stop` unacknowledged | present | `✓ ` (the marker outranks the flag) |
| `stop` acknowledged | present | `◆ ` |
| `notify` | present | `! ` |

Which one wins is the configured `priority` order, with the flag standing in for
`review`: by default `notify > stop > review > thinking`. The flag is never
acknowledged — `auto_clear` does not include `review` — so a flagged pane
comes back to `◆` once its `stop` or `notify` has been seen, and stays there
until you clear it.

A marker file whose own type is `review` — written by an older version of this
plugin, or by a writer that publishes `review` directly — is still read as the
same flag, and `Alt+B` still clears it.

**Atomic writes recommended:** To avoid partial reads, write to a `.tmp` file then rename:

### Shell (one-liner)

```bash
case "$WEZTERM_PANE" in '' | *[!0-9]*) exit 0 ;; esac  # numeric pane id only
MARKER_DIR="${WEZTERM_ATTENTION_DIR:-$HOME/.local/state/wezterm-attention}"
mkdir -p "$MARKER_DIR"
if command -v uuidgen >/dev/null 2>&1; then PUBLICATION_ID="$(uuidgen)"; else PUBLICATION_ID="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"; fi
printf '{"type":"stop","publication_id":"%s","updated_at":%s}\n' "$PUBLICATION_ID" "$(date +%s)" > "$MARKER_DIR/$WEZTERM_PANE.tmp" && mv "$MARKER_DIR/$WEZTERM_PANE.tmp" "$MARKER_DIR/$WEZTERM_PANE"
```

### TypeScript / Bun

```typescript
import { mkdir, writeFile, rename } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";

const pane = process.env.WEZTERM_PANE;
if (!pane || !/^\d+$/.test(pane)) process.exit(0); // numeric pane id only

const dir = process.env.WEZTERM_ATTENTION_DIR || join(process.env.HOME!, ".local", "state", "wezterm-attention");
await mkdir(dir, { recursive: true });

const file = join(dir, pane);
await writeFile(file + ".tmp", JSON.stringify({ type: "stop", publication_id: randomUUID(), updated_at: Date.now() }));
await rename(file + ".tmp", file);
```

### Node.js

```javascript
const fs = require("fs");
const path = require("path");
const { randomUUID } = require("crypto");

const pane = process.env.WEZTERM_PANE;
if (!pane || !/^\d+$/.test(pane)) process.exit(0); // numeric pane id only

const dir = process.env.WEZTERM_ATTENTION_DIR || path.join(process.env.HOME, ".local", "state", "wezterm-attention");
fs.mkdirSync(dir, { recursive: true });

const file = path.join(dir, pane);
fs.writeFileSync(file + ".tmp", JSON.stringify({ type: "stop", publication_id: randomUUID(), updated_at: Date.now() }));
fs.renameSync(file + ".tmp", file);
```

## Existing update-status handler?

By default, the plugin registers its own `update-status` handler to poll marker files. If you already have one (e.g., for a git status bar), use manual polling instead:

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

-- The id a pane's markers are named after: in one of the GUI's own domains
-- (local, exec, serial, WSL) its pane id; elsewhere the server pane id it
-- published (WEZTERM_ATTENTION, else WEZTERM_PANE); nil when it published neither.
local marker_id = attention.pane_marker_id(pane)

-- Read cached attention state:
-- returns (type, frame, source, reserved, subagents, review) or nil.
-- source is the marker's JSON "source" string (nil when it carried none);
-- reserved is always false; it preserves the positions of later tuple values;
-- subagents is how many of the pane's subagents ran a tool call in the last
-- ten minutes, 0 when none. A pane with live subagents and no marker returns
-- (nil, nil, nil, false, n).
-- review is true when the pane carries the Alt+B flag. state is the effective
-- type: "review" when the flag outranks the marker file, the marker's own type
-- when that outranks the flag -- and then review is still true.
local state, frame, source, reserved, subagents, review = attention.get_attention(marker_id)

-- Read fifteen cached base fields plus independent lifecycle evidence, without
-- I/O. Nested returned values do not share mutable state with the plugin cache.
local view = attention.get_attention_view(pane)

-- Clear a v1 flat marker programmatically. This removes the flat marker file and
-- the sidecars beside it; it does not clear a v2 activity record or acknowledge
-- a v2 record's event, even when the id resolves to a pane with v2 records. An
-- id that is not a pane id does nothing.
attention.remove_marker(marker_id)

-- Check the GUI-side identity publication of every pane in a window. Returns a
-- list of diagnostics, empty when every pane is identified. File, socket,
-- process, permission and version checks belong to `attention doctor`.
local diagnostics = attention.doctor(window)

-- Poll markers manually (for auto_poll = false)
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

Which format Pi writes depends on the pane. Where `WEZTERM_ATTENTION_ROOT` is unset (the `attention` command is not built), it writes v1 flat markers. Where it is set, it writes v2 records through the command, and those need a launch claim: in a pane where `pi` was started without one, the writer refuses every event, Pi shows one warning, and the tab shows nothing for Pi. Start Pi from a shell that claims for it; see [Mux setup](docs/mux-setup.md).

For cached lifecycle observations, question-publication evidence and consumer-owned presentation, see the [consumer guide](docs/consumer-guide.md).

## Claude Code hooks

This repository does not edit Claude Code's settings; registration is yours. `attention hooks describe --provider claude --json` lists every native event with `registration` set to `register` or `ignored`. For each `register` row, run `attention` with that row's `arguments`; Claude Code passes the callback JSON on stdin, and the command reads it unchanged.

Register the `attention` link on your PATH, not `$WEZTERM_ATTENTION_ROOT/bin/attention`. Hooks are global: they also run in editors, other terminals, ssh sessions and cron, where that variable is unset, and on macOS the plugin directory's path contains a space, which splits an unquoted command. A hook that has nothing to record there, such as one outside WezTerm or in a pane with no claim, exits 0 and does not interrupt the agent. If the agent's PATH does not include `~/.local/bin`, put the link's absolute path in each `command` instead.

In `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart":       [{ "hooks": [{ "type": "command", "command": "attention hooks event claude SessionStart" }] }],
    "UserPromptSubmit":   [{ "hooks": [{ "type": "command", "command": "attention hooks event claude UserPromptSubmit" }] }],
    "PreToolUse":         [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PreToolUse" }] }],
    "PostToolUse":        [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PostToolUse" }] }],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PostToolUseFailure" }] }],
    "PermissionRequest":  [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PermissionRequest" }] }],
    "PermissionDenied":   [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PermissionDenied" }] }],
    "Notification":       [{ "hooks": [{ "type": "command", "command": "attention hooks event claude Notification" }] }],
    "Elicitation":        [{ "hooks": [{ "type": "command", "command": "attention hooks event claude Elicitation" }] }],
    "ElicitationResult":  [{ "hooks": [{ "type": "command", "command": "attention hooks event claude ElicitationResult" }] }],
    "PreCompact":         [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PreCompact" }] }],
    "PostCompact":        [{ "hooks": [{ "type": "command", "command": "attention hooks event claude PostCompact" }] }],
    "Stop":               [{ "hooks": [{ "type": "command", "command": "attention hooks event claude Stop" }] }],
    "StopFailure":        [{ "hooks": [{ "type": "command", "command": "attention hooks event claude StopFailure" }] }],
    "SubagentStop":       [{ "hooks": [{ "type": "command", "command": "attention hooks event claude SubagentStop" }] }],
    "SessionEnd":         [{ "hooks": [{ "type": "command", "command": "attention hooks event claude SessionEnd" }] }]
  }
}
```

Merge these into any hooks you already have. Do not register `SubagentStart`: a child becomes visible only after its first tool call. `SubagentStop` records that the same child stopped. A root `Stop` does not clear the children, because background children can outlive it. A `StopFailure` (the turn ended on an API error) shows `notify`.

## Codex hooks

Codex reads lifecycle hooks from `~/.codex/hooks.json`, and asks you to approve each new or edited hook once (`/hooks` in Codex). `attention hooks describe --provider codex --json` lists the rows; the same rule applies as for Claude Code, and so does the advice to register the link on your PATH.

```json
{
  "hooks": {
    "SessionStart":      [{ "hooks": [{ "type": "command", "command": "attention hooks event codex SessionStart" }] }],
    "UserPromptSubmit":  [{ "hooks": [{ "type": "command", "command": "attention hooks event codex UserPromptSubmit" }] }],
    "PreToolUse":        [{ "hooks": [{ "type": "command", "command": "attention hooks event codex PreToolUse" }] }],
    "PostToolUse":       [{ "hooks": [{ "type": "command", "command": "attention hooks event codex PostToolUse" }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "attention hooks event codex PermissionRequest" }] }],
    "PreCompact":        [{ "hooks": [{ "type": "command", "command": "attention hooks event codex PreCompact" }] }],
    "PostCompact":       [{ "hooks": [{ "type": "command", "command": "attention hooks event codex PostCompact" }] }],
    "Stop":              [{ "hooks": [{ "type": "command", "command": "attention hooks event codex Stop" }] }],
    "Interrupt":         [{ "hooks": [{ "type": "command", "command": "attention hooks event codex Interrupt" }] }],
    "SubagentStop":      [{ "hooks": [{ "type": "command", "command": "attention hooks event codex SubagentStop" }] }],
    "SessionEnd":        [{ "hooks": [{ "type": "command", "command": "attention hooks event codex SessionEnd" }] }]
  }
}
```

Do not register `SubagentStart`. Child attribution needs matching native `agent_id` values; see [contact evidence](docs/reviews/lifecycle-contact-results.md) for the paths that were exercised. A root `Stop` writes the lead stop, then one child-clear record. An `Interrupt` clears the tab's activity for that session, because Codex runs no `Stop` after one. A child's `PermissionRequest` shows `notify` on the tab, since Codex has no `Notification` hook.

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

A WezTerm window attached to a mux server mirrors the server's tabs under numbers of its own, and those are the numbers the tab bar prints. They are not the order of `wezterm cli list`: a consumer of this project measured one 29-tab window on 2026-09-19 and found 22 of the 29 numbers differing. Nothing outside the GUI process can see the drawn order, so the tab bar publishes it — one file per window, under the state directory, named by the incarnation of the GUI's own mux socket and the window id:

```text
<state directory>/tabs/<incarnation id>-<window id>.json
```

```json
{ "schema": 2,
  "window_id": 0,
  "published_at_ms": 1789884000123,
  "source": { "socket_path": "/…/gui-sock-4946", "realm_id": "…", "incarnation_id": "…" },
  "tabs": [ { "number": 11, "text": " 11: ✓ braid ", "marker_ids": ["16"] } ] }
```

`source` names the GUI that drew the window, because a window id means something only inside one GUI process. The plugin learns that identity by asking the `attention` command shortly after startup. Until then, and always when the command is not built, it writes a schema-1 file at `tabs/<window id>.json` with no `source`, and removes it once a sourced file for the same window is written.

`number` is the number the bar printed, `text` is the whole string it drew (control characters removed, cut to 256 bytes, and a spinner always shown at its first frame so the file does not change every second), and `marker_ids` are the IDs the plugin already uses for those panes — already translated out of the window's local numbering, because only the window could translate them. A pane on v1 flat markers is a canonical decimal marker id; a pane with v2 records is `v2:<realm_id>:<incarnation_id>:<pane_id>` once a poll has identified it. The file is written when a window's composed list changes and at no other time, so `published_at_ms` says when the bar last drew something different.

Read it with `attention tabs`, which returns every window in the same JSON envelope as `bindings`. **It is honest about when it was written, not guaranteed current**: nothing refreshes it while the bar is idle, and no consumer should act on a number it has not checked. Use it to describe tabs and to resolve "the second `api` tab"; to act on one, ask the GUI, where `mux_window:tabs_with_info()` returns the drawn order live.

The publisher is the handler the plugin registers, so `renderer = "manual"` — where your own formatter draws the tabs and the plugin registers nothing — publishes nothing. `wrap_title_formatter` does not publish either: with both handlers registered, the same window would draw two different texts and each repaint would rewrite the file twice.

Every setup publishes, including a plain local WezTerm where the drawn number equals the derived one. A consumer cannot tell a simple setup from a publisher that is not running — the file is absent in both — and deriving is right in one case and wrong in the other.

## How it works

The plugin uses a **poller/renderer split** to avoid blocking WezTerm's GUI thread:

1. **Poller** (`update-status` event) — runs on WezTerm's `config.status_update_interval` (default 1000ms). Reads marker files and acknowledgement sidecars, then updates an in-memory cache. It acknowledges the focused window's current active pane and asks WezTerm to rebuild the tab bar when a pane's effective attention changes. It also removes the marker of any pane that was in the window on the previous tick and is gone now — unless every pane of that pane's domain went at once, which is a domain detach rather than a close, and those panes are still alive on the server.
2. **Renderer** (`format-tab-title` event) — fires on every tab repaint (mouse hover, key press, redraws). Reads only from the cache — no file reads, instant returns. It writes one file, and only when a window's whole bar draws something different from the last time it drew: the [drawn tab order](#the-drawn-tab-order), which no other process can see. A repaint that draws the same thing composes the list, compares it, and touches nothing.

WezTerm rebuilds tab titles when something it knows about changes, and a marker file appearing on disk is not one of those things. When the poller sees a pane's effective attention change, it performs `ActivateTabRelative(0)` on the focused window. That re-activates the already-selected tab and makes WezTerm recompute every tab title. The plugin does not write either status string, the window title, or any user title.

Set `request_redraw = false` to switch that request off, for a host whose own `update-status` handler already redraws the titles it owns. The redraw action can pass through WezTerm's normal tab-activation path, including terminal focus reporting. It therefore runs only when the window has keyboard focus and a valid active pane. Generated spinner frames use one-second wall-clock buckets, so polls induced by the action see the same frame and terminate. If an action fails, that window logs once and stops requesting redraws.

For mux domains, the poller also recovers identity after a GUI reconnect. It waits for the pane count to be stable across two polls, runs one realm publication, then retries after 2, 5, 10, and 30 seconds while any pane remains unpublished. The child PATH includes `wezterm.executable_dir`; publication writes terminal output and never pane input.

The Lua implementation is split by responsibility under `plugin/`: protocol validation, record reading, runtime polling, overlays, legacy compatibility, title sampling, and formatting. `plugin/init.lua` owns configuration, composition, callback registration, and the public API.

## Troubleshooting

**Markers not showing?**
- If the window is attached to a mux server (`wezterm connect`, a unix domain), check the pane publishes its id: `wezterm cli list --format json` shows the server-side pane id, and the pane must emit that number as the `WEZTERM_PANE` user var. See [Publishing the pane id](#publishing-the-pane-id). Without it the plugin deliberately does nothing for that pane.
- Check the directory exists: `ls ~/.local/state/wezterm-attention/`, or `$XDG_STATE_HOME/wezterm-attention` when `XDG_STATE_HOME` is set, or your configured `dir` (see [Configure](#configure) for the order)
- Verify `WEZTERM_PANE` is set: `echo $WEZTERM_PANE` (should print a number inside WezTerm)
- Check file contents: `cat ~/.local/state/wezterm-attention/$WEZTERM_PANE` (should be valid JSON)
- A `+N` with no glyph beside it is the [subagent count](#subagent-activity-the-agents-sidecar) for a pane whose own marker is gone or already acknowledged. `cat ~/.local/state/wezterm-attention/$WEZTERM_PANE.agents` shows the entries; ones older than ten minutes are not counted.
- A matching `$WEZTERM_PANE.ack` means that publication was already displayed. Removing the sidecar makes it visible again; sidecars are plugin-owned and safe to remove before rolling back to an older plugin version.
- A ◆ that no marker file explains is the `Alt+B` flag: `ls ~/.local/state/wezterm-attention/$WEZTERM_PANE.review`. Pressing `Alt+B` in that tab clears it, and so does deleting the file.
- Ensure your hooks write to the same path as the plugin's `dir` setting
- `status_update_interval` defaults to 1000ms; markers update on this interval. Lower it if indicators feel slow — the redraw request rides on the same tick.

**Indicators appear only when you switch tabs?**
- In `renderer = "manual"` mode, pass the event pane: `attention.poll(window, { active_pane = pane })`. The plugin resolves `window:active_pane()` at use time; the event pane is used only when the current pane is unavailable.
- Check the WezTerm error log. A failed redraw action is logged once for that window; polling and acknowledgement continue.

**Tab titles look wrong?**
- WezTerm only runs the **first** registered `format-tab-title` handler. If you have your own handler, set `renderer = "manual"` and use `wrap_title_formatter()` or the plugin API. Two handlers cannot coexist.
- Use `title_formatter` to customize the base title while keeping the plugin's indicators.

**Alt+B not working?**
- Check for keybind conflicts. Set `review_key = false` and bind manually if needed.
- It does work on a pane that already has a marker — the flag is the separate `$WEZTERM_PANE.review` file. If the tab still shows `✓` or `!` after a press, that marker simply outranks the flag; the ◆ appears once you have seen it.
- On a mux-attached pane that has not published its `WEZTERM_PANE` user var, the press is refused and logged once, because the plugin cannot tell which pane's files to write. See [Publishing the pane id](#publishing-the-pane-id).
- On a pane with v2 records whose current launch claim is not the launch the pane last published, the press is refused and logged (`cannot flag this pane for review: ...`), because the flag would not show. The next prompt republishes the claim.

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).

## License

MIT
