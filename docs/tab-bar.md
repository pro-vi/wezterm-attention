# The tab bar

What a tab shows, which pane decides it, and when a mark goes away. [Configuration](configuration.md) has the options that change it.

## States

| State | Indicator | Tab tint | Meaning |
|-------|-----------|----------|---------|
| `thinking` | ◌ ◔ ◑ ◕ (animated) | Violet | Agent is working |
| `stop` | ✓ | Mint | Agent finished — check results |
| `notify` | ! | Rose | Something needs your attention |
| `review` | ◆ | Gold | Manually flagged for review (`Alt+B`) |
| `thinking`, turn end held | ⏾ | Violet | The turn ended, and a [hold check](setup.md#claude-code-hooks) says the agent's work goes on |
| `stop`, quiet turn end | ↪ | Blue | The turn ended, and a [quiet check](setup.md#claude-code-hooks) says nobody needs to be called |
| muted | a bell with a slash | None | You muted the tab with `Alt+M`; see [Muting a tab](#muting-a-tab) |

Tabs light up when an agent or script records a state for its pane—even when another pane in that tab is currently focused, and even when the tab itself is not the one you are on. Focusing a pane acknowledges only that pane's `stop` and `notify` (not a `notify` shown over its agent's finished turn while a sub-agent waits for permission); states of unfocused sibling panes remain visible until you visit them. `thinking` persists until its writer replaces or clears it, or until the TTL it was written with expires; a `review` flag persists until you press `Alt+B` again.

Only the active pane of the focused window is acknowledged. The writer's record stays in place; the plugin has the `attention` command record which activity you were shown, and only while that activity is still the pane's, so an unseen notification remains visible.

When multiple panes in a tab have different states, the highest-priority one wins: **notify > stop > review > thinking**.

The `review` flag is yours, not a writer's: it is a record of its own, so you can flag a pane that is mid-`thinking` or showing a `stop` without touching either. See [The review flag](#the-review-flag).

`Alt+M` mutes a tab: it shows a bell until its agent waits on you. See [Muting a tab](#muting-a-tab).

A pane can also report how many subagents are still working inside it. The tab appends that count to whatever indicator it is already showing — `✓+2` — or shows `+2` on its own when the pane has nothing else to show. See [Subagent activity](#subagent-activity).

## Subagent activity

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
[accepted limitations](accepted-limitations.md#a-sub-agent-whose-end-is-never-reported-stays-counted-until-its-session-ends).

The count never decides which state wins the tab — priority is settled by the
states alone. But a change in the count alone is a visible change, so it
repaints the tab bar on the next poll like any other.

## The review flag

`Alt+B` flags the focused pane for your own attention. The flag is a review
record owned by `user`, which the plugin writes through the `attention` command
under the same locks as every other review writer. A press on a tab that carries
your flag on any of its panes clears it from all of them; a press on a tab that
does not flags the focused pane. The pane needs a launch claim, which an agent or
a claiming shell makes when a command starts there (see
[Recording attention from your own tools](building-on-attention.md#recording-attention-from-your-own-tools));
a pane no launch has claimed cannot be flagged.

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

For an agent session, Attention carries your flag when the session registers
with `resume` in a new pane after its old socket is removed or replaced, or
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

## Muting a tab

`Alt+M` mutes the focused tab: a tab whose agent you do not need to hear from
for now, such as one running a long job or a loop. A muted tab shows a bell
with a slash and no tint, and nothing of what its agent is doing, until the
agent waits on you:

| Agent | Tab shows |
|-------|-----------|
| idle, `thinking`, held, `stop`, quiet, sub-agents only | the bell |
| `notify`, or a sub-agent waiting for permission | the bell, then `!`, in the notify tint |
| your review flag | the bell, then `◆`, in the review tint |

A press mutes every pane of the tab that a launch has claimed, so a split tab
is muted as one; a press on a tab with any muted pane unmutes them all. A pane
that is not muted, such as one split into the tab later, still lights it, after
the bell. The mute stays until you press `Alt+M` again, also when a new agent
starts in that pane.

The mute is a record of its own, `mute.json` in the pane's directory, which
only `attention plugin set-mute` and `clear-mute` write. It changes no other
record: `get_attention_view(pane).muted` and a title formatter's
`ctx.attention.muted` say a pane or tab is muted, and `type` still says what
the agent is doing. Hook deliveries carry `muted` too, so a consumer that plays
a sound when a turn ends can play nothing; see the consumer guide's "Transient
hook delivery".

## Publishing the pane id

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

## How it works

The plugin uses a **poller/renderer split** to avoid blocking WezTerm's GUI thread:

1. **Poller** (`update-status` event) — runs on WezTerm's `config.status_update_interval` (default 1000ms). Reads each pane's records, then updates an in-memory cache. It acknowledges the focused window's current active pane, by running the `attention` command once for each new activity shown there, and again after a backoff wait when that run could not get the pane's locks or did not answer, and asks WezTerm to rebuild the tab bar when a pane's effective attention changes. It drops the cache entry of any pane that was in the window on the previous tick and is gone now — unless every pane of that pane's domain went at once, which is a domain detach rather than a close, and those panes are still alive on the server. It deletes nothing.
2. **Renderer** (`format-tab-title` event) — fires on every tab repaint (mouse hover, key press, redraws). Reads only from the cache — no file reads, instant returns. It writes one file, and only when a window's whole bar draws something different from the last time it drew: the [drawn tab order](building-on-attention.md#the-drawn-tab-order), which no other process can see. A repaint that draws the same thing composes the list, compares it, and touches nothing.

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
