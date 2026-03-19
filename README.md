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
  dir = os.getenv("HOME") .. "/.claude/wezterm-attention",

  -- Tab background tints (keep these dark — they're behind text)
  colors = {
    thinking = "#1c1730",
    stop     = "#12271c",
    notify   = "#240f16",
    review   = "#1a1a0c",
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

1. **Write** a JSON file to `~/.claude/wezterm-attention/<WEZTERM_PANE>`
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position
4. **Cleanup** is automatic — markers are removed when panes close or tabs become active

The `WEZTERM_PANE` environment variable is injected by WezTerm into every shell it spawns. That's the pane's unique ID.

### Shell (one-liner)

```bash
echo '{"type":"stop"}' > ~/.claude/wezterm-attention/$WEZTERM_PANE
```

### TypeScript / Bun

```typescript
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

const dir = join(process.env.HOME!, ".claude", "wezterm-attention");
await mkdir(dir, { recursive: true });
await writeFile(join(dir, process.env.WEZTERM_PANE!), JSON.stringify({ type: "stop" }));
```

### Node.js

```javascript
const fs = require("fs");
const path = require("path");

const dir = path.join(process.env.HOME, ".claude", "wezterm-attention");
fs.mkdirSync(dir, { recursive: true });
fs.writeFileSync(path.join(dir, process.env.WEZTERM_PANE), JSON.stringify({ type: "stop" }));
```

## Public API

The plugin also exposes functions for use in your own WezTerm Lua code (e.g., custom status bars):

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")

-- Read attention state: returns (type, frame) or nil
local state, frame = attention.get_attention(pane:pane_id())

-- Clear a marker programmatically
attention.remove_marker(pane:pane_id())
```

## Use cases

- **AI coding agents** (Claude Code, Codex) — hook into stop/notification events to light up tabs
- **Build systems** — write `notify` on failure, `stop` on success
- **Test runners** — animated `thinking` while running, `stop` or `notify` on completion
- **Long-running scripts** — any background job that wants your attention when done
- **Manual triage** — `Alt+B` to flag tabs for review during code review sessions

## How it works

WezTerm polls the status bar every 5 seconds (configurable via `config.status_update_interval`). The `format-tab-title` event fires on each poll, reads marker files, and returns styled tab titles. No background threads, no FFI, no external dependencies — just filesystem reads in Lua.

## License

MIT
