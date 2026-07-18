# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes via simple marker files, and WezTerm reflects them as colored tab indicators.

## What it looks like

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review |

Tabs light up when a background process writes a marker—even when another pane in that tab is currently focused. Focusing a pane auto-clears only that pane's `stop` and `notify`; markers from unfocused sibling panes remain visible until you visit them. `thinking` and `review` persist until explicitly removed.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

## Install

Add one line to your `wezterm.lua`:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

By default, the plugin owns tab title formatting (`dir / title` + attention indicators). It also registers pane cleanup, a marker poller, and an `Alt+B` keybind to toggle review mode. `Alt+B` operates on the whole active tab: it flags the active pane, and clears the flag from every pane in the tab when any are already flagged (so split tabs can always be cleared with one press). It keys off whether `review` is set anywhere in the tab, independent of which indicator is currently rendered — a higher-priority `stop`/`notify` can mask the ◆.

> **Important:** WezTerm only runs the **first** registered `format-tab-title` handler. If another plugin (e.g. tabline.wez) registers one before this plugin, all attention features — indicators, colors, and auto-clear — are disabled. Make sure `apply_to_config` runs before any other plugin that touches tab titles, or use `renderer = "manual"` to integrate via the API instead.

## Render modes

The plugin supports three render modes:

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

  -- Auto-clear these types when focusing their pane
  auto_clear = { "stop", "notify" },

  -- Stale marker cleanup by type, in milliseconds.
  -- Prevents zombie busy tabs if a process exits without clearing.
  -- Set to false to disable all stale cleanup.
  stale_after_ms = { thinking = 30 * 60 * 1000 },

  -- Review toggle keybind (false to disable)
  review_key = { key = "b", mods = "ALT" },

})
```

## The protocol

Any process running inside WezTerm can write a marker. The contract is:

1. **Write** a JSON file to `~/.local/state/wezterm-attention/<WEZTERM_PANE>`
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position. If omitted for `thinking`, the plugin animates it during polling.
4. **Optional:** `updated_at` or `updated_at_ms` records when the marker was refreshed. Seconds and milliseconds are both accepted.
5. **Optional:** `ttl_ms` overrides stale cleanup for that marker. By default, stale `thinking` markers clear after 30 minutes.
6. **Cleanup** is automatic — markers are removed when panes close, their panes become focused, or stale TTL expires

The `WEZTERM_PANE` environment variable is injected by WezTerm into every shell it spawns. That's the pane's unique ID.

**Atomic writes recommended:** To avoid partial reads, write to a `.tmp` file then rename:

### Shell (one-liner)

```bash
MARKER_DIR="$HOME/.local/state/wezterm-attention"
mkdir -p "$MARKER_DIR"
printf '{"type":"stop","updated_at":%s}\n' "$(date +%s)" > "$MARKER_DIR/$WEZTERM_PANE.tmp" && mv "$MARKER_DIR/$WEZTERM_PANE.tmp" "$MARKER_DIR/$WEZTERM_PANE"
```

### TypeScript / Bun

```typescript
import { mkdir, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";

const dir = join(process.env.HOME!, ".local", "state", "wezterm-attention");
await mkdir(dir, { recursive: true });

const file = join(dir, process.env.WEZTERM_PANE!);
await writeFile(file + ".tmp", JSON.stringify({ type: "stop", updated_at: Date.now() }));
await rename(file + ".tmp", file);
```

### Node.js

```javascript
const fs = require("fs");
const path = require("path");

const dir = path.join(process.env.HOME, ".local", "state", "wezterm-attention");
fs.mkdirSync(dir, { recursive: true });

const file = path.join(dir, process.env.WEZTERM_PANE);
fs.writeFileSync(file + ".tmp", JSON.stringify({ type: "stop", updated_at: Date.now() }));
fs.renameSync(file + ".tmp", file);
```

## Existing update-status handler?

By default, the plugin registers its own `update-status` handler to poll marker files. If you already have one (e.g., for a git status bar), use manual polling instead:

```lua
attention.apply_to_config(config, { auto_poll = false })

-- Then in your existing update-status handler:
wezterm.on('update-status', function(window, pane)
  attention.poll(window)  -- reads markers, updates cache
  -- ... your git status bar, battery, etc.
end)
```

## Public API

The plugin exposes functions for use in your own WezTerm Lua code:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

-- Read cached attention state: returns (type, frame) or nil
local state, frame = attention.get_attention(pane:pane_id())

-- Clear a marker programmatically
attention.remove_marker(pane:pane_id())

-- Poll markers manually (for auto_poll = false)
attention.poll(window)

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

Once installed, it writes markers automatically as Pi works. Outside WezTerm (`WEZTERM_PANE` unset) it's a silent no-op:

| Pi event | Marker | What happens |
|----------|--------|--------------|
| `agent_start` | `thinking` | Tab spins violet while Pi runs |
| `tool_execution_start` | `thinking` | Spinner continues while Pi uses a tool |
| `agent_end` | `stop` | Tab turns mint with ✓ when Pi finishes |

`thinking` markers carry `ttl_ms`, so a Pi process that exits unexpectedly won't leave a stuck spinner.

### Commands

Control the marker for the current pane manually:

```text
/attention status              show the current marker
/attention busy    [label]     mark thinking
/attention ready   [label]     mark stop
/attention pending [label]     mark notify (waiting on you)
/attention blocked [label]     mark notify
/attention review  [label]     flag for review
/attention clear               remove the marker
```

Aliases: `busy → thinking`, `ready → stop`, `pending`/`blocked → notify`. With `WEZTERM_PANE` unset, every command reports that markers are disabled rather than silently doing nothing.

### For other Pi extensions

Any Pi extension can request a marker for the current pane by emitting the shared `wezterm-attention:mark` event — e.g. an ask-user extension flagging `notify` while it waits for you:

```ts
pi.events.emit("wezterm-attention:mark", { type: "notify" });
// { type: "clear" } removes it; a bare string like "busy" also works
```

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
if (process.env.WEZTERM_PANE) {
  const { mkdirSync, writeFileSync, readFileSync, renameSync } = require('fs');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;

  let frame = 0;
  try {
    const data = JSON.parse(readFileSync(markerFile, 'utf8'));
    if (data.type === 'thinking') frame = ((data.frame || 0) + 1) % 4;
  } catch {}

  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'thinking', frame, updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Stop** — agent finished:
```typescript
if (process.env.WEZTERM_PANE) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'stop', updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Notification / PermissionRequest** — needs attention:
```typescript
if (process.env.WEZTERM_PANE) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const markerDir = `${process.env.HOME}/.local/state/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'notify', updated_at: Date.now() }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**SessionEnd** — cleanup:
```typescript
if (process.env.WEZTERM_PANE) {
  const { unlinkSync } = require('fs');
  try {
    unlinkSync(`${process.env.HOME}/.local/state/wezterm-attention/${process.env.WEZTERM_PANE}`);
  } catch {}
}
```

> **Tip:** Add `` execSync(`wezterm cli set-window-title --pane-id ${process.env.WEZTERM_PANE} " "`) `` after writing a marker to force an immediate tab redraw instead of waiting for the next poll cycle.

## Codex hooks

Wire Codex through its **lifecycle hooks** (`~/.codex/hooks.json`). Avoid the older
`~/.codex/config.toml` `[hooks] notify` field: it is finish-only, and the desktop Codex "Computer
Use" app silently rewrites it on launch (repointing it at a temp path that later disappears), so
markers quietly stop firing. Lifecycle hooks aren't touched by that. They require a one-time trust
step — run `/hooks` in Codex and approve them once per machine before they fire.

The marker writer is transport-agnostic — it writes the same JSON the plugin reads, tagged
`source:"codex"`. Map each lifecycle event to the matching state:

| Codex lifecycle event | Marker |
|---|---|
| `PreToolUse` | `{"type":"thinking","source":"codex","frame":0-3,"updated_at_ms":…}` (frame cycles 0→3) |
| `PermissionRequest` | `{"type":"notify","source":"codex","updated_at_ms":…}` |
| `Stop` | `{"type":"stop","source":"codex","updated_at_ms":…}` |
| `SessionStart` | clears the marker (skip `compact`, so a mid-turn compaction keeps its spinner) |

```typescript
async function writeWezTermMarker(marker: Record<string, unknown>): Promise<void> {
  const paneId = process.env.WEZTERM_PANE;
  const home = process.env.HOME;
  if (!paneId || !home) return;

  const { mkdir, writeFile, rename } = require("node:fs/promises");
  const { join } = require("node:path");

  const dir = join(home, ".local", "state", "wezterm-attention");
  await mkdir(dir, { recursive: true });
  const file = join(dir, paneId);
  await writeFile(file + ".tmp", JSON.stringify({ source: "codex", updated_at_ms: Date.now(), ...marker }));
  await rename(file + ".tmp", file); // atomic
}
```

See Codex's hooks documentation for the `hooks.json` structure that binds each lifecycle event to a
command; call `writeWezTermMarker` from each with the state above. (`updated_at_ms` is accepted by
the plugin alongside `updated_at`.)

**Coverage caveat.** `PermissionRequest` fires only for command / patch / network approvals — *not*
for Codex's other human-input waits (`request_user_input`, `request_permissions`, and MCP
elicitation). A turn blocked on one of those shows the `thinking` spinner, not `!`. Routing those to
`notify` means detecting the tool in `PreToolUse`; it isn't wired here yet.

## Other use cases

- **Build systems** — write `notify` on failure, `stop` on success
- **Test runners** — animated `thinking` while running, `stop` or `notify` on completion
- **Long-running scripts** — any background job that wants your attention when done
- **Manual triage** — `Alt+B` to flag tabs for review during code review sessions

## How it works

The plugin uses a **poller/renderer split** to avoid blocking WezTerm's GUI thread:

1. **Poller** (`update-status` event) — runs on WezTerm's `config.status_update_interval` (default 1000ms). Reads marker files from disk and updates an in-memory cache.
2. **Renderer** (`format-tab-title` event) — fires on every tab repaint (mouse hover, key press, redraws). Reads only from the cache — zero I/O, instant returns.

No background threads, no FFI, no external dependencies — just filesystem reads in Lua on a configurable interval.

## Troubleshooting

**Markers not showing?**
- Check the directory exists: `ls ~/.local/state/wezterm-attention/` (or your configured `dir`)
- Verify `WEZTERM_PANE` is set: `echo $WEZTERM_PANE` (should print a number inside WezTerm)
- Check file contents: `cat ~/.local/state/wezterm-attention/$WEZTERM_PANE` (should be valid JSON)
- Ensure your hooks write to the same path as the plugin's `dir` setting
- `status_update_interval` defaults to 1000ms; markers update on this interval

**Tab titles look wrong?**
- WezTerm only runs the **first** registered `format-tab-title` handler. If you have your own handler, set `renderer = "manual"` and use `wrap_title_formatter()` or the plugin API. Two handlers cannot coexist.
- Use `title_formatter` to customize the base title while keeping the plugin's indicators.

**Alt+B not working?**
- Check for keybind conflicts. Set `review_key = false` and bind manually if needed.

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).

## License

MIT
