# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes via simple marker files, and WezTerm reflects them as colored tab indicators.

## What it looks like

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review |

Inactive tabs light up when a background process writes a marker. Active tabs auto-clear `stop` and `notify` (you've seen it). `thinking` and `review` persist until explicitly removed.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

## Install

Add one line to your `wezterm.lua`:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

This registers the tab title formatter, pane cleanup handler, and an `Alt+B` keybind to toggle review mode.

## Configure

All options are optional — defaults work out of the box:

```lua
attention.apply_to_config(config, {
  -- Where marker files live (one file per pane ID)
  -- Default: ~/.local/state/wezterm-attention
  dir = os.getenv("HOME") .. "/.local/state/wezterm-attention",

  -- Tab background tints per attention type.
  -- These are blended behind tab text, so keep them dark (lightness ~10-15%).
  -- A good starting point: take your accent color and mix it ~70% toward
  -- your tab bar background. The defaults assume a near-black tab bar.
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

  -- Auto-clear these types when switching to the tab
  auto_clear = { "stop", "notify" },

  -- Review toggle keybind (false to disable)
  review_key = { key = "b", mods = "ALT" },
})
```

## The protocol

Any process running inside WezTerm can write a marker. The contract is:

1. **Write** a JSON file to `~/.local/state/wezterm-attention/<WEZTERM_PANE>`
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position
4. **Cleanup** is automatic — markers are removed when panes close or tabs become active

The `WEZTERM_PANE` environment variable is injected by WezTerm into every shell it spawns. That's the pane's unique ID.

**Atomic writes recommended:** To avoid partial reads, write to a `.tmp` file then rename:

### Shell (one-liner)

```bash
MARKER_DIR="$HOME/.local/state/wezterm-attention"
mkdir -p "$MARKER_DIR"
echo '{"type":"stop"}' > "$MARKER_DIR/$WEZTERM_PANE.tmp" && mv "$MARKER_DIR/$WEZTERM_PANE.tmp" "$MARKER_DIR/$WEZTERM_PANE"
```

### TypeScript / Bun

```typescript
import { mkdir, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";

const dir = join(process.env.HOME!, ".local", "state", "wezterm-attention");
await mkdir(dir, { recursive: true });

const file = join(dir, process.env.WEZTERM_PANE!);
await writeFile(file + ".tmp", JSON.stringify({ type: "stop" }));
await rename(file + ".tmp", file);
```

### Node.js

```javascript
const fs = require("fs");
const path = require("path");

const dir = path.join(process.env.HOME, ".local", "state", "wezterm-attention");
fs.mkdirSync(dir, { recursive: true });

const file = path.join(dir, process.env.WEZTERM_PANE);
fs.writeFileSync(file + ".tmp", JSON.stringify({ type: "stop" }));
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
```

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

Claude hooks write to `~/.claude/wezterm-attention/` by convention. Point the plugin at that path:

```lua
attention.apply_to_config(config, {
  dir = os.getenv("HOME") .. "/.claude/wezterm-attention",
})
```

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
  const markerDir = `${process.env.HOME}/.claude/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;

  let frame = 0;
  try {
    const data = JSON.parse(readFileSync(markerFile, 'utf8'));
    if (data.type === 'thinking') frame = ((data.frame || 0) + 1) % 4;
  } catch {}

  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'thinking', frame }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Stop** — agent finished:
```typescript
if (process.env.WEZTERM_PANE) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const markerDir = `${process.env.HOME}/.claude/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'stop' }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**Notification / PermissionRequest** — needs attention:
```typescript
if (process.env.WEZTERM_PANE) {
  const { mkdirSync, writeFileSync, renameSync } = require('fs');
  const markerDir = `${process.env.HOME}/.claude/wezterm-attention`;
  const markerFile = `${markerDir}/${process.env.WEZTERM_PANE}`;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(markerFile + '.tmp', JSON.stringify({ type: 'notify' }));
  renameSync(markerFile + '.tmp', markerFile);
}
```

**SessionEnd** — cleanup:
```typescript
if (process.env.WEZTERM_PANE) {
  const { unlinkSync } = require('fs');
  try {
    unlinkSync(`${process.env.HOME}/.claude/wezterm-attention/${process.env.WEZTERM_PANE}`);
  } catch {}
}
```

> **Tip:** Add `` execSync(`wezterm cli set-window-title --pane-id ${process.env.WEZTERM_PANE} " "`) `` after writing a marker to force an immediate tab redraw instead of waiting for the next poll cycle.

## Codex hooks

Codex uses a single `notify` hook that fires when the agent finishes or needs attention. Like Claude, Codex hooks write to `~/.claude/wezterm-attention/` — use the same `dir` config shown above. Add this to your Codex notify handler:

```typescript
async function writeWezTermMarker(type: "stop" | "notify"): Promise<void> {
  const paneId = process.env.WEZTERM_PANE;
  const home = process.env.HOME;
  if (!paneId || !home) return;

  const { mkdir, writeFile } = require("node:fs/promises");
  const { join } = require("node:path");

  const markerDir = join(home, ".claude", "wezterm-attention");
  await mkdir(markerDir, { recursive: true });
  await writeFile(join(markerDir, paneId), JSON.stringify({ type }));
}

// In your notify handler:
// - "stop" if the agent completed work (has last-assistant-message)
// - "notify" for other notifications
const attentionType = payload["last-assistant-message"] ? "stop" : "notify";
await writeWezTermMarker(attentionType);
```

Wire it in `~/.codex/config.toml`:
```toml
[hooks]
notify = ["bun", "/path/to/your/notify.ts"]
```

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
- The plugin registers `format-tab-title` — if you have your own handler, only the first one registered wins. Remove yours or integrate the plugin's logic.

**Alt+B not working?**
- Check for keybind conflicts. Set `review_key = false` and bind manually if needed.

## License

MIT
