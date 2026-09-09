# wezterm-attention

A WezTerm plugin that turns your tab bar into a notification system. Any CLI tool — AI agents, build scripts, test runners — can signal state changes via simple marker files, and WezTerm reflects them as colored tab indicators.

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

Add the plugin before any other `format-tab-title` handler:

```lua
local attention = wezterm.plugin.require("https://github.com/pro-vi/wezterm-attention")
attention.apply_to_config(config)
```

The v1 reader and renderer require WezTerm `20221119-145034-49b9839f` or newer. Mux-native v2 requires POSIX and the Rust CLI built by running `scripts/install-cli.sh` in this checkout. `bin/attention` uses that installed Rust binary and names the install command if it is absent. The plugin exports its resolved checkout and state paths to new panes.

Continue with [Mux setup](docs/mux-setup.md) for Bash or zsh launch claims and provider callbacks. Zsh uses the explicit `wezterm_attention_claim && <agent>` fallback; it does not claim automatic detection. See [Record contract](docs/record-contract.md) for precedence and [Mux pane moves](docs/mux-pane-moves.md) before moving the final pane out of a server tab.

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

## Producer paths and V1 compatibility

V2 producers call the Rust writer through `bin/attention`; they do not construct V2 record JSON. Use `attention mark` for custom activity and `attention hooks event PROVIDER EVENT` for provider callbacks after the shell has established a launch claim. See [Mux setup](docs/mux-setup.md) for the supported commands and activation boundary.

The flat-file format below remains supported for existing V1 producers. The inspected bootstrap Claude/Codex helpers still use it; selecting the Rust binary does not migrate those registrations automatically.

### V1 flat-marker protocol

Any process running inside WezTerm can write a V1 marker. The compatibility contract is:

1. **Write** a JSON file to `~/.local/state/wezterm-attention/<WEZTERM_PANE>`
2. **Contents:** `{"type":"<state>"}` where state is `thinking`, `stop`, `notify`, or `review`
3. **Optional:** `{"type":"thinking","frame":0}` — `frame` (0-3) controls the spinner position. If omitted for `thinking`, the plugin animates it during polling.
4. **Recommended:** `publication_id` is a new non-empty string for every publication. It lets an identical `stop` or `notify` payload become visible again after the previous publication was acknowledged. Without it, the plugin uses the exact JSON bytes as the legacy identity.
5. **Optional:** `updated_at` or `updated_at_ms` records when the marker was refreshed. Seconds and milliseconds are both accepted.
6. **Optional:** `ttl_ms` overrides stale cleanup for that marker. By default, stale `thinking` markers clear after 30 minutes.
7. **Optional:** a `<WEZTERM_PANE>.agents` sidecar reports how many subagents are working in the pane — see [Subagent activity](#subagent-activity-the-agents-sidecar) below.
8. **Plugin-owned:** `<WEZTERM_PANE>.ack` records the marker publication you have already been shown, and `<WEZTERM_PANE>.review` is the `Alt+B` flag — see [The review flag](#the-review-flag-the-review-sidecar). Writers never touch either one.
9. **Cleanup** is automatic. The poller removes a marker whose pane it saw on the previous tick and does not see now (WezTerm emits no pane-close event, so a vanished pane is how a closed pane is detected), and removes a marker whose stale TTL has expired. A closed pane loses everything — marker, `.ack`, `.agents` and `.review`. An expired marker loses only itself and its `.ack`: the subagent sidecar and your review flag keep their own lifetimes and neither of them aged out because a spinner did. Focusing a pane writes an acknowledgement sidecar instead of removing writer-owned state.

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
acknowledged — `acknowledge_types` does not include `review` — so a flagged pane
comes back to `◆` once its `stop` or `notify` has been seen, and stays there
until you clear it.

A marker file whose own type is `review` — written by an older version of this
plugin, or by a writer that publishes `review` directly — is still read as the
same flag, and `Alt+B` still clears it.

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
-- returns (type, frame, source, puppet, subagents, review) or nil.
-- source is the marker's JSON "source" string (nil when it carried none);
-- puppet is true only when the marker set "puppet": true;
-- subagents is how many of the pane's subagents ran a tool call in the last
-- ten minutes, 0 when none. A pane with live subagents and no marker returns
-- (nil, nil, nil, false, n).
-- review is true when the pane carries the Alt+B flag. state is the effective
-- type: "review" when the flag outranks the marker file, the marker's own type
-- when that outranks the flag -- and then review is still true.
local state, frame, source, puppet, subagents, review = attention.get_attention(marker_id)

-- Read the cached full-pane v2 view without I/O. The returned table includes
-- provider, binding_id, binding_phase, type, event_id, subagents, review, and
-- reader_confidence. Mutating it does not change the plugin cache.
local view = attention.get_attention_view(pane)

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

Install this repository as a Pi package:

```bash
pi install git:github.com/pro-vi/wezterm-attention
```

The extension preserves print mode and registers no commands. It forwards `session_start`, `agent_start`, `tool_execution_start`, `agent_settled`, the `wezterm-attention:mark` bus, and `session_shutdown` through its serialized writer queue. `agent_end` is intentionally not terminal. Writer processes use Node's built-in child-process API, matching Pi's Node runtime.

Other extensions may emit `thinking`, `stop`, `notify`, `review`, or `clear`. Review uses the `pi-bus` owner. Clear writes an ordered activity-clear watermark, repairs v1 deletion, and clears that owner without suppressing newer activity. If the v2 checkout root is unavailable, shipped v1 marker behavior remains the fallback.

## Claude Code hooks

Claude hook registration remains user-owned. Pass original hook stdin to:

```text
attention hooks event claude SessionStart
attention hooks event claude PreToolUse
attention hooks event claude PermissionRequest
attention hooks event claude Stop
attention hooks event claude SubagentStop
attention hooks event claude SessionEnd
```

Use `$WEZTERM_ATTENTION_ROOT/bin/attention`. Do not register `SubagentStart`: presence begins only after child tool work. `SubagentStop` writes stopped evidence for the same child ID. Claude root Stop does not clear all children because background children may outlive it. See [Mux setup](docs/mux-setup.md).

## Codex hooks

Codex hook registration remains user-owned. Pass original hook stdin to:

```text
attention hooks event codex SessionStart
attention hooks event codex PreToolUse
attention hooks event codex PermissionRequest
attention hooks event codex Stop
attention hooks event codex SubagentStop
attention hooks event codex SessionEnd
```

Use `$WEZTERM_ATTENTION_ROOT/bin/attention`. Thread-spawned children carry the same `agent_id` through work and `SubagentStop`; internal and synthetic children emit neither callback. Root Stop writes lead Stop, then one child-clear watermark. `stop_hook_active` is not stored. Do not register `SubagentStart`. See [Mux setup](docs/mux-setup.md).

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

For mux domains, the poller also recovers identity after a GUI reconnect. It waits for the pane count to be stable across two polls, runs one realm publication, then retries after 2, 5, 10, and 30 seconds while any pane remains unpublished. The child PATH includes `wezterm.executable_dir`; publication writes terminal output and never pane input.

The Lua implementation is split by responsibility under `plugin/`: protocol validation, record reading, runtime polling, overlays, legacy compatibility, title sampling, and formatting. `plugin/init.lua` owns configuration, composition, callback registration, and the public API.

## Troubleshooting

**Markers not showing?**
- If the window is attached to a mux server (`wezterm connect`, a unix domain), check the pane publishes its id: `wezterm cli list --format json` shows the server-side pane id, and the pane must emit that number as the `WEZTERM_PANE` user var. See [Publishing the pane id](#publishing-the-pane-id). Without it the plugin deliberately does nothing for that pane.
- Check the directory exists: `ls ~/.local/state/wezterm-attention/` (or your configured `dir`)
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

## Type annotations

LuaCATS type annotations are available via [wezterm-types](https://github.com/DrKJeff16/wezterm-types) for IDE autocomplete and type checking. See [DrKJeff16/wezterm-types#145](https://github.com/DrKJeff16/wezterm-types/pull/145).

## License

MIT
