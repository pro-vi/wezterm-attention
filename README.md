# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes via simple marker files, and WezTerm reflects them as colored tab indicators.

## What it looks like

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review |

Tabs light up when a background process writes a marker—even when another pane in that tab is currently focused, and even when the tab itself is not the one you are on. Focusing a pane acknowledges only that pane's `stop` and `notify`; markers from unfocused sibling panes remain visible until you visit them. `thinking` persists until its writer removes it or its TTL expires; `review` persists until explicitly removed.

Only the active pane of the focused window is acknowledged. The writer-owned marker stays in place; the plugin records the exact displayed identity in a `.ack` sidecar, so unseen notifications remain visible.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

A pane can also report how many subagents are still working inside it. The tab appends that count to whatever indicator it is already showing — `✓+2` — or shows `+2` on its own when the pane has no marker left. See [Subagent activity](#subagent-activity-the-agents-sidecar).

## Install

Add one line to your `wezterm.lua`:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

Requires WezTerm `20221119-145034-49b9839f` or newer.

By default, the plugin owns tab title formatting (`dir / title` + attention indicators). It also registers a marker poller and an `Alt+B` keybind to toggle review mode. `Alt+B` operates on the whole active tab: it flags the active pane, and clears the flag from every pane in the tab when any are already flagged (so split tabs can always be cleared with one press). It keys off whether `review` is set anywhere in the tab, independent of which indicator is currently rendered — a higher-priority `stop`/`notify` can mask the ◆.

> **Important:** WezTerm only runs the **first** registered `format-tab-title` handler. If another plugin (e.g. tabline.wez) registers one first, this plugin still polls and acknowledges markers, but its indicators and colors are not rendered. Make sure `apply_to_config` runs first, or use `renderer = "manual"` to integrate via the API instead.

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

In `tab` mode, pass a `title_formatter` to control the base title without losing indicators:

```lua
attention.apply_to_config(config, {
  title_formatter = function(tab, ctx)
    -- ctx.default_title = "dir / pane_title"
    -- ctx.attention = { indicator, type, color }
    local pane = tab.active_pane
    return pane.title  -- just the pane title, no directory
  end,
})
```

## Configure

All options are optional — defaults work out of the box:

```lua
attention.apply_to_config(config, {
  -- Render mode: "tab" | "manual"
  renderer = "tab",

  -- Where marker files live (one file per pane ID)
  dir = os.getenv("HOME") .. "/.local/state/wezterm-attention",

  -- Custom base title (tab mode only; plugin adds indicators + colors around it)
  title_formatter = nil,  -- function(tab, ctx) -> string

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

  -- Review toggle keybind (false to disable)
  review_key = { key = "b", mods = "ALT" },

  -- Ask WezTerm to rebuild the tab bar when a pane's attention changes.
  -- Set false only if your own code already repaints titles every tick
  -- (renderer = "manual" with a title drawn from your update-status handler).
  -- The plugin then never performs ActivateTabRelative(0), saving that
  -- tab-activation pass; nothing else about polling or acknowledgement changes.
  request_redraw = true,

})
```

## The protocol

Any process running inside WezTerm can write a marker. The contract is:

1. **Write** a JSON file to `~/.local/state/wezterm-attention/<WEZTERM_PANE>`
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position. If omitted for `thinking`, the plugin animates it during polling.
4. **Recommended:** `publication_id` is a new non-empty string for every publication. It lets an identical `stop` or `notify` payload become visible again after the previous publication was acknowledged. Without it, the plugin uses the exact JSON bytes as the legacy identity.
5. **Optional:** `updated_at` or `updated_at_ms` records when the marker was refreshed. Seconds and milliseconds are both accepted.
6. **Optional:** `ttl_ms` overrides stale cleanup for that marker. By default, stale `thinking` markers clear after 30 minutes.
7. **Optional:** a `<WEZTERM_PANE>.agents` sidecar reports how many subagents are working in the pane — see [Subagent activity](#subagent-activity-the-agents-sidecar) below.
8. **Cleanup** is automatic. The poller removes a marker whose pane it saw on the previous tick and does not see now (WezTerm emits no pane-close event, so a vanished pane is how a closed pane is detected), and removes a marker whose stale TTL has expired. Removing a marker takes its `.agents` sidecar with it. Focusing a pane writes an acknowledgement sidecar instead of removing writer-owned state.

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

Panes in the GUI's own `local` domain need none of this — there `pane:pane_id()`
and `$WEZTERM_PANE` are the same number whether it is published or not.

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

**The writer owns this file.** The plugin only reads it, and deletes it along
with the marker when the pane closes or the marker is removed.

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
| none | 2 | `+2 ` (tinted mint, as `stop` is) |

The count never decides which marker wins the tab — priority is settled by the
markers alone. But a change in the count alone is a visible change, so it
repaints the tab bar on the next poll like any other.

**Atomic writes recommended:** To avoid partial reads, write to a `.tmp` file then rename:

### Shell (one-liner)

```bash
case "$WEZTERM_PANE" in '' | *[!0-9]*) exit 0 ;; esac  # numeric pane id only
MARKER_DIR="$HOME/.local/state/wezterm-attention"
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

const dir = join(process.env.HOME!, ".local", "state", "wezterm-attention");
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

const dir = path.join(process.env.HOME, ".local", "state", "wezterm-attention");
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

-- The id a pane's markers are named after: its published WEZTERM_PANE user
-- var, else its pane id when the pane is in the "local" domain, else nil.
local marker_id = attention.pane_marker_id(pane)

-- Read cached attention state:
-- returns (type, frame, source, puppet, subagents) or nil.
-- source is the marker's JSON "source" string (nil when it carried none);
-- puppet is true only when the marker set "puppet": true;
-- subagents is how many of the pane's subagents ran a tool call in the last
-- ten minutes, 0 when none. A pane with live subagents and no marker returns
-- (nil, nil, nil, false, n).
local state, frame, source, puppet, subagents = attention.get_attention(marker_id)

-- Clear a marker programmatically
attention.remove_marker(marker_id)

-- Poll markers manually (for auto_poll = false)
attention.poll(window, { active_pane = pane })

-- Wrap a title function with attention decoration (for renderer = "manual")
wezterm.on("format-tab-title", attention.wrap_title_formatter(function(tab, ctx)
  -- ctx.default_title is "dir / title"
  -- ctx.attention is { indicator, type, color }
  return ctx.default_title
end))
```

## Pi extension

[Pi](https://github.com/badlogic/pi-mono) is an extensible coding agent. This repo ships a Pi extension that writes attention markers for the current WezTerm pane — install it with one command:

```bash
pi install git:github.com/pro-vi/wezterm-attention
```

Once installed, it writes markers automatically from Pi's lifecycle. Outside WezTerm (`WEZTERM_PANE` unset) it's a silent no-op:

| Pi event | Marker | What happens |
|----------|--------|--------------|
| `agent_start` | `thinking` | Tab spins violet while Pi runs |
| `tool_execution_start` | `thinking` | Spinner continues while Pi uses a tool |
| `agent_settled` | `stop` | Tab turns mint with ✓ once Pi is fully done |

`stop` hangs off `agent_settled`, not `agent_end`: `agent_end` fires at the end of every low-level run — including ones Pi will auto-retry or auto-continue after compaction — so using it would flash a false ✓ mid-task. `agent_settled` fires only once Pi will not continue running automatically, so the ✓ appears exactly once, at the real end. (This needs **Pi 0.80.5+** — `agent_settled` landed in the 0.80.4 changelog but 0.80.4 was never published to npm. On an older Pi the extension loads but the ✓ never fires; the spinner clears on its TTL instead.)

`thinking` markers carry `ttl_ms`, so a Pi process that exits unexpectedly won't leave a stuck spinner. That's the whole extension — no commands, no configuration; the tab tracks Pi automatically.

### The `notify` state, for other extensions

Pi's lifecycle only produces `thinking` and `stop` — there's no lifecycle event for "blocked, waiting for a human", so the extension never raises the rose `!` on its own. Instead it listens on a shared event bus so **any other Pi extension can request a state** without knowing anything about marker files or `WEZTERM_PANE`. The typical caller is an ask-user extension flagging `notify` while it waits for you, then clearing it:

```ts
pi.events.emit("wezterm-attention:mark", { type: "notify" }); // waiting on you → rose !
pi.events.emit("wezterm-attention:mark", { type: "clear" });  // answered → remove it
```

The payload is a bare string (`"notify"`) or an object (`{ type: "notify", label }`); accepted states are `thinking` / `stop` / `notify` / `review` / `clear` (with `busy`, `ready`, `pending`, `blocked` as aliases). Extensions that would rather not depend on this event can always [write the marker file directly](#the-protocol).

### Environment

| Variable | Default | Effect |
|----------|---------|--------|
| `WEZTERM_ATTENTION_DIR` | `~/.local/state/wezterm-attention` | Override the marker directory (match the plugin's `dir`) |
| `PI_WEZTERM_ATTENTION_TTL_MS` | `1800000` (30 min) | Override the `thinking` marker TTL |

## Claude Code hooks

Claude Code has [hooks](https://docs.anthropic.com/en/docs/claude-code/hooks) that fire on lifecycle events. Add attention markers to each one:

| Hook event | Marker | What happens | Required? |
|------------|--------|--------------|-----------|
| `Stop` | `stop` | Tab turns mint with ✓ when agent finishes | **Yes** — core value |
| `PreToolUse` | `thinking` | Spinner animates while agent works | Recommended |
| `Notification` | `notify` | Tab turns rose with ! for notifications | Optional |
| `PermissionRequest` | `notify` | Tab turns rose when agent needs approval | Optional |
| `SessionEnd` | _(cleanup)_ | Marker file removed | Recommended |

**Minimum viable setup:** Just the `Stop` hook gives you the "agent finished" indicator. Add the rest as desired.

The snippets below are **fragments to paste into your hook files** — not standalone scripts. Each one guards on `WEZTERM_PANE` so it's safe to use outside WezTerm. If you don't have existing hooks, wrap the snippet in a Claude Code hook handler (see [hook docs](https://docs.anthropic.com/en/docs/claude-code/hooks)).

> **Pane-ID contract.** WezTerm sets `WEZTERM_PANE` to a non-negative integer. Every fragment below gates on `/^\d+$/` before touching the filesystem — an unvalidated `../…` value would let a write clobber, or a delete remove, a file *outside* the marker dir.

Register hooks in `~/.claude/settings.json`:
```json
{
  "hooks": {
    "Stop": [{ "matcher": "", "hooks": ["/bin/bash ~/.claude/hooks/stop.sh"] }],
    "PreToolUse": [{ "matcher": "", "hooks": ["/bin/bash ~/.claude/hooks/pre_tool_use.sh"] }],
    "SessionEnd": [{ "matcher": "", "hooks": ["/bin/bash ~/.claude/hooks/session_end.sh"] }]
  }
}
```

**PreToolUse** — animated thinking spinner:
```typescript
if (process.env.WEZTERM_PANE && /^\d+$/.test(process.env.WEZTERM_PANE)) {
  const { mkdirSync, writeFileSync, readFileSync, renameSync } = require('fs');
  const { randomUUID } = require('crypto');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;

  let frame = 0;
  try {
    const data = JSON.parse(readFileSync(markerFile, 'utf8'));
    if (data.type === 'thinking') frame = ((data.frame || 0) + 1) % 4;
  } catch {}

  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'thinking', publication_id: randomUUID(), frame, updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Stop** — agent finished:
```typescript
if (process.env.WEZTERM_PANE && /^\d+$/.test(process.env.WEZTERM_PANE)) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const { randomUUID } = require('crypto');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'stop', publication_id: randomUUID(), updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Notification / PermissionRequest** — needs attention:
```typescript
if (process.env.WEZTERM_PANE && /^\d+$/.test(process.env.WEZTERM_PANE)) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const { randomUUID } = require('crypto');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'notify', publication_id: randomUUID(), updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**SessionEnd** — cleanup:
```typescript
if (process.env.WEZTERM_PANE && /^\d+$/.test(process.env.WEZTERM_PANE)) {
  const { unlinkSync } = require('fs');
  try {
    unlinkSync(`${process.env.HOME}/.local/state/wezterm-attention/${process.env.WEZTERM_PANE}`);
  } catch {}
}
```

## Codex hooks

Wire Codex through its **lifecycle hooks** (`~/.codex/hooks.json`). Avoid the older top-level
`notify` field in `~/.codex/config.toml`: it is finish-only, and the desktop Codex "Computer Use"
app silently rewrites it on launch (repointing it at a temp path that later disappears), so markers
quietly stop firing. Lifecycle hooks aren't touched by that. They require a one-time trust approval —
run `/hooks` in Codex and approve — and because trust is keyed to a hash of the hook definition,
editing a hook re-prompts. See the [Codex hooks documentation](https://learn.chatgpt.com/docs/hooks)
for the `hooks.json` schema that binds each event to a command.

Map each lifecycle event to a marker state (all tagged `source:"codex"`):

| Codex lifecycle event | Marker state |
|---|---|
| `PreToolUse` | `thinking` (optionally cycle `frame` 0→3 for the spinner) |
| `PermissionRequest` | `notify` |
| `Stop` | `stop` |
| `SessionStart` | **remove** the marker file |

The three *write* states share one helper — a **writer-only fragment**, not a complete hook. Call it
from the `PreToolUse` / `PermissionRequest` / `Stop` hooks with the matching state:

```typescript
async function writeWezTermMarker(marker: Record<string, unknown>): Promise<void> {
  const paneId = process.env.WEZTERM_PANE;
  const home = process.env.HOME;
  // WezTerm injects WEZTERM_PANE as a non-negative integer. Validate it: a stray
  // value like "../foo" would escape the marker dir — and on the SessionStart
  // cleanup path below, the rm would then delete a file outside it.
  if (!paneId || !home || !/^\d+$/.test(paneId)) return;

  // `await import`, not `require`: this fragment is ESM-shaped, and bare
  // `require` is undefined under Node in module mode (throws). `await import`
  // works under both Node ESM and bun.
  const { mkdir, writeFile, rename } = await import("node:fs/promises");
  const { randomUUID } = await import("node:crypto");
  const { join } = await import("node:path");

  const dir = join(home, ".local", "state", "wezterm-attention");
  await mkdir(dir, { recursive: true });
  const file = join(dir, paneId);
  await writeFile(file + ".tmp", JSON.stringify({ source: "codex", updated_at_ms: Date.now(), ...marker, publication_id: randomUUID() }));
  await rename(file + ".tmp", file); // atomic
}
// PreToolUse:        writeWezTermMarker({ type: "thinking" })
// PermissionRequest: writeWezTermMarker({ type: "notify" })
// Stop:              writeWezTermMarker({ type: "stop" })
```

Two behaviours the fragment deliberately does **not** implement — wire them in your hooks if you want them:

- **`SessionStart` cleanup** *removes* the marker rather than writing one: `rm(join(dir, paneId), { force: true })`, not `writeWezTermMarker`. Apply the same `paneId`/`home` guard first (`if (!paneId || !home || !/^\d+$/.test(paneId)) return;`) — the `rm` is the one path where an unvalidated pane id could delete a file *outside* the marker dir. (Skip cleanup on the `compact` startup reason so a mid-turn compaction keeps its spinner.)
- **Spinner frame cycling** (`frame` 0→3 across repeated `PreToolUse`) needs reading the current marker and incrementing; omit it entirely and the plugin animates the spinner on its own poll. Optional.

(`updated_at_ms` is accepted by the plugin alongside `updated_at`.)

**Coverage caveat.** `PermissionRequest` fires for command / patch / network approvals *and* MCP
tool-call approvals — but *not* for Codex's other human-input waits (`request_user_input`,
`request_permissions`, and MCP *elicitation*). A turn blocked on one of those uncovered waits shows
the `thinking` spinner, not `!`. Routing those to `notify` means detecting the tool in `PreToolUse`;
it isn't wired here yet.

## Other use cases

- **Build systems** — write `notify` on failure, `stop` on success
- **Test runners** — animated `thinking` while running, `stop` or `notify` on completion
- **Long-running scripts** — any background job that wants your attention when done
- **Manual triage** — `Alt+B` to flag tabs for review during code review sessions

## How it works

The plugin uses a **poller/renderer split** to avoid blocking WezTerm's GUI thread:

1. **Poller** (`update-status` event) — runs on WezTerm's `config.status_update_interval` (default 1000ms). Reads marker files and acknowledgement sidecars, then updates an in-memory cache. It acknowledges the focused window's current active pane and asks WezTerm to rebuild the tab bar when a pane's effective attention changes. It also removes the marker of any pane that was in the window on the previous tick and is gone now — unless every pane of that pane's domain went at once, which is a domain detach rather than a close, and those panes are still alive on the server.
2. **Renderer** (`format-tab-title` event) — fires on every tab repaint (mouse hover, key press, redraws). Reads only from the cache — zero I/O, instant returns, and no writes of any kind.

WezTerm rebuilds tab titles when something it knows about changes, and a marker file appearing on disk is not one of those things. When the poller sees a pane's effective attention change, it performs `ActivateTabRelative(0)` on the focused window. That re-activates the already-selected tab and makes WezTerm recompute every tab title. The plugin does not write either status string, the window title, or any user title.

Set `request_redraw = false` to switch that request off, for a host whose own `update-status` handler already redraws the titles it owns. The redraw action can pass through WezTerm's normal tab-activation path, including terminal focus reporting. It therefore runs only when the window has keyboard focus and a valid active pane. Generated spinner frames use one-second wall-clock buckets, so polls induced by the action see the same frame and terminate. If an action fails, that window logs once and stops requesting redraws.

No background threads, no FFI, no external dependencies — just filesystem reads in Lua on a configurable interval.

## Troubleshooting

**Markers not showing?**
- If the window is attached to a mux server (`wezterm connect`, a unix domain), check the pane publishes its id: `wezterm cli list --format json` shows the server-side pane id, and the pane must emit that number as the `WEZTERM_PANE` user var. See [Publishing the pane id](#publishing-the-pane-id). Without it the plugin deliberately does nothing for that pane.
- Check the directory exists: `ls ~/.local/state/wezterm-attention/` (or your configured `dir`)
- Verify `WEZTERM_PANE` is set: `echo $WEZTERM_PANE` (should print a number inside WezTerm)
- Check file contents: `cat ~/.local/state/wezterm-attention/$WEZTERM_PANE` (should be valid JSON)
- A `+N` with no glyph beside it is the [subagent count](#subagent-activity-the-agents-sidecar) for a pane whose own marker is gone or already acknowledged. `cat ~/.local/state/wezterm-attention/$WEZTERM_PANE.agents` shows the entries; ones older than ten minutes are not counted.
- A matching `$WEZTERM_PANE.ack` means that publication was already displayed. Removing the sidecar makes it visible again; sidecars are plugin-owned and safe to remove before rolling back to an older plugin version.
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

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).

## License

MIT
