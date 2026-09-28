# Accepted limitations

What this project knows is imperfect and has decided to ship anyway, with the
reason in each case. It is short on purpose: something leaves this file by being
fixed or by being reclassified as intended behaviour, not by being forgotten.

Most working notes — plans, review write-ups, triage records — stay on the
author's machine and are not in this repository. A few are tracked because
something still points at them: `hooks describe` names
`docs/reviews/lifecycle-contact-results.md` in its evidence output, for one. This
file is the maintained public account, and it is the one to read first.

## Panic paths are not denied by lint

`Cargo.toml` denies `dbg_macro`, `todo`, `unimplemented`, `unsafe_op_in_unsafe_fn`
and `unused_must_use`. It does not deny `clippy::unwrap_used` or
`clippy::expect_used`, which still fire at sites in the library.

Denying them today would mean adding an allow attribute at each site, which announces an
intention while changing nothing. The sites need reading individually: some are
genuinely infallible and want a comment, some want an error path. Until that pass
happens, the table locks in the lints the crate already satisfies and says
nothing about the ones it does not.

To list the sites, run `cargo clippy --lib -- -W clippy::unwrap_used
-W clippy::expect_used`. The command also lints `build.rs`, and reports its one
`expect` (cargo sets `CARGO_MANIFEST_DIR`), which is not a library site. Each
site is one of three things, and they
need separating before the lint can go on:

1. A genuine invariant. It keeps the call and gains an `#[allow]` with a
   one-line reason.
2. A read of a field the validator has already proved present. These belong to
   "Validated record fields are read as if they could be missing" below, and
   disappear when that view type exists.
3. A failure nothing handles, which should return an `AttentionError`. This is
   the set worth finding.

Replacing `.unwrap()` with `.unwrap_or(default)` on a required field does not
count as handling it. It turns a loud failure into a silent wrong answer.

A panic in a hook is not a clean abort. It interrupts processing wherever it
lands, so the state a consumer then reads can be missing, stale, or updated in
one file and not another. The record contract already declines to promise that
independently written files form one atomic snapshot; nothing rolls back on
failure. That is why this is a real gap rather than a stylistic preference.

## One timestamp field carries three roles

`observed_mono_ns` decides which of two competing writes publishes, serves as the
watermark an activity-clear compares against, and is the cutoff at which a Codex
parent `Stop` removes its sub-agents. `apply_activity` passes the surviving
activity's `observed_mono_ns` as the order of the parent clear it applies to
`children.json`, so the coupling is semantic rather than a shared field name.

One consequence is known and characterised: when an incoming activity is
semantically equal to the published one, the write is skipped, the stored order
keeps its older value, and an event carrying a timestamp between the two can
still publish over it if it commits later. The interleaving is narrow and the
wrong tint clears on the next distinct activity. The same skip keeps a Codex
parent clear at the earlier stop's order, so a sub-agent whose last event falls
between that stop and a repeat of it stays counted until a distinct `Stop`.
`a_deduplicated_activity_does_not_advance_the_ordering_fence` in
`tests/rust/lifecycle_spec.rs` pins the current behaviour so a change to it is
deliberate.

Separating the roles is a record-contract change: a new field the Lua reader must
tolerate on records written before and after, schema validation, pruning in
`maintenance.rs`, reporting in `query.rs`, and a decision about which of the two
meanings the parent-clear cutoff actually wants. That is not a change to
make on the way out of the door.

The sequence, with one current binding, no acknowledgement and no
activity-clear:

| step | order | result |
|---|---|---|
| `PreToolUse` publishes `thinking` | 300 | applied |
| `UserPromptSubmit` repeats `thinking` | 500 | skipped, stored order stays 300 |
| `Stop` commits late | 400 | applied, publishes `stop` |

The tab reads `stop` while the agent works on the prompt submitted at 500. If
that turn calls no tool, nothing republishes `thinking`. Commit order differs
from timestamp order because the `hooks event` command in `src/main.rs` takes
`monotonic_ns20()` before its blocking `read_to_end` of stdin, and the commit
then waits up to two seconds on the launch lock.

The obvious fix was tried and does not work. Advancing `observed_mono_ns` on a
duplicate breaks
`duplicate_codex_stop_keeps_a_child_newer_than_the_surviving_activity`, because
a repeated Codex `Stop` would then clear children that started after the first
one. It also breaks `same_claim_event_reaches_snapshot` ("facts must not refresh
an equal badge") and the record contract's promise that a duplicate event does
not refresh timestamps.

A fix is done when the table above ends with the `Stop` at 400 `ignored` and
`thinking` surviving, the duplicate-Codex-stop test passes unchanged, and a
duplicate still leaves `event_id` and `written_at_unix_ns` untouched.

## Consumer options belong to the hook invocation, not to each consumer

`--consumer` repeats, so one hook can deliver to several executables. The options
beside it do not repeat. `--consumer-timeout-ms` is one value applied to each
consumer in turn, and `--include-reply` and `--include-prompt` select content for
the single envelope every consumer receives.

Three things follow, and a consumer author should know all three before
registering a second executable:

- Consumers run in the order given, one after another, so the deadline multiplied
  by the number of consumers bounds the dispatch loop. It does not bound the hook:
  before the first consumer starts, the hook has already read its payload, applied
  the provider event and taken the record locks. That product is a floor for the
  hook's worst case, not the whole of it. All of it is spent inside a synchronous
  hook, with the agent waiting.
- A fast consumer and a slow one cannot be given different deadlines.
- Adding a consumer to a hook that already passes `--include-reply` or
  `--include-prompt` hands that text to the new executable as well. Content
  cannot be selected per consumer.

A failing consumer never stops the ones after it, and that part is genuinely per
consumer. `--strict` is not. It is one decision about the hook's exit code,
testing a single flag that any consumer which did not complete will raise -- and
that a native outcome of ignored, conflict or partial raises just the same. One
consumer cannot be marked as allowed to fail while another is not.

## The three validators disagree on how a version may be spelled

`schema` on an ordinary record and `wire` on a published identity carry the same
number in all three implementations, but not the same rule about its encoding.
`src/protocol.rs` requires an unsigned integer: `value.as_u64() == Some(...)`.
`plugin/protocol.lua` compares the decoded number, and the Python checker in
`tests/fixtures/v2/check.py` does the same. So `"schema": 3.0` is rejected by
Rust and accepted by the other two, and the disagreement is lexical -- it exists
only in the JSON text, and disappears once the value is decoded.

The project already draws this distinction where it decided it mattered: the Lua
side scans lifecycle snapshots for canonical integers before decoding, and the
Python checker asserts the lifecycle schema's integer type. Ordinary records and
wire identity did not get the same treatment.

The Rust writer serialises through serde, which writes an integer, so it never
emits the disputed spelling.

The plugin writes no records. The one file it writes, a tab order, spells its
integers with `string.format("%d")` and its schema as a literal, and encodes the
`source` object with `wezterm.json_encode`, so it never emits `3.0`.

That leaves the divergence real but harmless: two readers accept a spelling the
third rejects, and nothing this project ships produces it.

Tightening the Lua and Python readers to match Rust is a reasonable change
rather than a risky one, and it is deferred for sequencing rather than danger:
it wants the canonical rule stated and raw-JSON fixtures for the integral-float
and exponent spellings, since a decoded fixture cannot express the difference.

## What sweep leaves behind

`attention sweep --apply` removes only ended bindings and closed panes' whole
trees under the retention rules in the
[record contract](record-contract.md#trust-boundary), with their session index
entries; and exited GUIs' tab-order files the record contract lists as collected.
Those rules keep sweep from removing state it cannot prove abandoned, and they
mean four kinds of leftover stay on disk:

- **An exited GUI's tab-order files that name mux panes.** A file is removed
  only when it names no tab, or when every pane it names is verified absent.
  The GUI's own local panes are, once it has exited (see the next section), so
  a file naming only those goes. A window attached to a mux server names that
  server's panes, which are usually still running, or gone together with their
  socket with nothing to show the server gone, which a reader reports as
  unavailable rather than absent. So that file stays until you remove it. So
  does a file naming a pane whose realm or incarnation records are gone, as
  after you remove a server's records by hand; with no socket to ask, it
  leaves sweep complete. It is safe to delete
  `tabs/<incarnation id>-<window id>.json` by hand once no GUI with that
  window is running.
- **Temporary files from an interrupted write, outside a tree being removed.**
  So is a review that an earlier plugin build had moved aside to
  `<review>.json.<session>.<ms>.clear` while clearing it, and never finished with. They do not stop a binding or pane tree from being
  pruned, and they go with that tree when it is, but sweep collects none on its
  own.
- **A half-removed binding directory in a live pane.** A crash while a binding
  directory was being removed can leave part of it behind. While the pane is
  live, retention does not touch its tree, so the remainder stays.
- **Per-child sub-agent files an earlier build wrote.** Builds before
  `children.json` wrote `agents/`, `agents-clear.json` and `agents-floor.json`
  into each binding. Nothing reads them now. Sweep removes them only with their
  binding, under the same retention rules, so a binding that is still current
  keeps them.

Collecting any of these would be a new deletion, and would need the same
evidence rule the others have.

## The sub-agent count depends on how Claude Code and Codex send hooks

Attention counts a sub-agent from the hooks Claude Code and Codex run, and stops
counting it only on a hook that says it ended (see the
[record contract](record-contract.md#child-presence)). The count is exact only
while the behaviours below hold. Each belongs to another
program, so each names what it was checked against and what the tab shows if it
stops holding.

- **Each hook finishes before the agent goes on.** One sub-agent's events then
  reach the writer in the order they happened, which is why `children.json`
  keeps no record of sub-agents that stopped. Checked in Claude Code's hooks
  documentation, read on 2026-09-27 with Claude Code 2.1.283 installed ("By
  default, hooks block Claude's execution until they complete"), and in Codex
  source at commit `985cf47a4`, which awaits each hook it runs; not run live on
  Codex. Both let a command hook opt out with `async: true`, and the README's
  blocks do not set it. Under an asynchronous hook, a sub-agent's older tool
  call can arrive after its `SubagentStop` and add it again, and it then stays
  counted until its session ends.
- **A Codex parent stops only after its sub-agents have.** A Codex parent
  `Stop` removes every sub-agent whose last event came before it, because Codex
  sends no stop for an interrupted sub-agent (next item). The Codex runtime
  does not enforce the order: in its source at `985cf47a4` no step of a turn
  waits for running sub-agents, and the model waits by calling `wait_agent`. In
  the sessions recorded with Codex CLI 0.157.1, none of the 11 with sub-agent
  events showed sub-agent work after the parent's `Stop`. If a parent does stop
  first, its sub-agent is not counted from that `Stop` until its next event,
  which counts it again, and that event's hook prints
  `attention: child_active_after_parent_clear: …` on stderr; its `SubagentStop`
  after the parent's `Stop` prints the same. A sub-agent that sends nothing more
  before it ends is not counted again at all, and one that stays quiet past its
  parent's next `Stop` is counted again without the report, since the set keeps
  only the latest parent stop's removals.
- **Codex reports no interrupted sub-agent.** In Codex source at `985cf47a4`,
  an interrupt runs no hook for a sub-agent's session, and a parent's `Stop`
  carries no list of running sub-agents. An interrupted Codex sub-agent
  therefore stays counted until a later parent `Stop` is published after its
  last event, or its session ends; a `Stop` that repeats the stop the tab
  already shows is skipped and keeps the earlier cutoff (see
  [One timestamp field carries three roles](#one-timestamp-field-carries-three-roles)). If Codex starts sending either report, it can
  replace the parent-stop rule above.
- **Codex sends `SubagentStart` for the sub-agents it spawns.** Codex source at
  `985cf47a4` dispatches it, with the sub-agent's `agent_id` and `agent_type`,
  for a spawned sub-agent whether it starts fresh or forks its parent's
  history, and skips it for internal and system sub-agents. No live Codex
  session has shown it yet. Without it, a Codex sub-agent is counted from its
  first tool call, which carries its `agent_type` in the same source.
- **Claude Code starts a resumed sub-agent again under the same id.** Claude
  Code 2.1.283 sent `SubagentStart` with the same `agent_id` when a stopped
  sub-agent was woken by its own finished background shell, then its tool
  calls and a second `SubagentStop`. Claude Code's sub-agents documentation,
  read on 2026-09-27, says "Resuming starts a new run of the agent under the
  same ID". If a resume stops sending
  `SubagentStart`, a resumed sub-agent is counted from its first tool call
  instead.
- **Claude Code's own agents carry no type.** The agents Claude Code runs for
  itself are told apart only by an empty or
  missing `agent_type`, so a tool call is counted only when it names a type. In
  one session of Claude Code 2.1.283, four such agents sent only a
  `SubagentStop` with `agent_type: ""`, and one sent a single Bash `PreToolUse`
  with no `agent_type` and nothing else, which would otherwise have been
  counted until its session ended. Every tool call of a sub-agent launched
  through the Agent tool carried its `agent_type` in that session. The session
  ran with transcript saving turned off, which may change what these agents
  send.
  Sessions started with `--agent` were not captured: if their internal agents'
  events carry the session's agent name as `agent_type`, one that sends a tool
  call and no stop is counted until its session ends.

## A sub-agent whose end is never reported stays counted until its session ends

Nothing removes a sub-agent for being quiet, so one whose end never reaches the
writer stays counted, as `+1` on its tab, until its session's `SessionEnd`, or
until the pane moves on to another session. A Codex sub-agent also goes at a
later parent `Stop` that is published after its last event; a repeated `Stop`
the writer skips keeps the earlier cutoff.
The end goes missing when:

- the `SubagentStop` hook failed, timed out, or was not registered;
- the user pressed Esc on a foreground sub-agent, if Claude Code sends no
  `SubagentStop` then. This is untested: Claude Code 2.1.283 launched every
  sub-agent in the background (2 of 2, also when asked for the foreground), and
  its hooks documentation, read on 2026-09-27, says sub-agents run in the
  background by default since 2.1.198. Esc on the lead while a background
  sub-agent ran fired no hook, and the sub-agent sent its `SubagentStop` when it
  finished its command;
- a Codex sub-agent was interrupted, or a hook ran asynchronously, as the
  previous section describes.

Earlier builds hid a sub-agent ten minutes after its last event, which also hid
sub-agents still running one long command. A count that can stay too high was
chosen over one that drops a sub-agent that is still working. There is no
command to remove such a sub-agent by hand.

## An invalid child set is started again, and its sub-agents return at their next event

A writer that finds `children.json` invalid renames it to
`.children.json.invalid.<uuid>`, applies its event to a new, empty set, and
writes that set even when the event changes nothing else; its hook reports
`record_invalid`. A `children.json` that something else replaced with a link to a
file that is not a valid set is moved aside as a link, and sweep then keeps that
binding, as it keeps any that holds a link; a link to a valid set, or to
nothing, is replaced by a regular file at the next write. Until that next child event, the tab shows `+?`. The
sub-agents the invalid set held are counted again only at their next event, so
one in the middle of a long command stays uncounted until it calls another tool
or stops. The renamed files are write leftovers: they go with their binding,
and sweep collects none on its own (see
[What sweep leaves behind](#what-sweep-leaves-behind)).

Counting what an invalid file held would mean trusting a file that failed
validation. Starting again loses only what each sub-agent's next event restores.

## A plugin and a command from either side of the child set show no sub-agents

The plugin reads `children.json`; builds of the command before it wrote one file
per sub-agent under `agents/` instead. A plugin that reads `children.json`
beside a command that does not write it, or an older plugin beside a command
that writes only `children.json`, shows no sub-agents: each finds none of the
files it reads, and the tab shows neither a count nor `+?`.
Updating the plugin does not rebuild the command, so run
`scripts/install-cli.sh` after `wezterm.plugin.update_all()`, as
[Install](../README.md#install) says.

## The six-value query cannot say a sub-agent count is unknown

`get_attention` keeps its six values, as the record contract promises, and its
fifth, `subagents`, is 0 both when no sub-agent runs and when the pane's count
could not be read. Only `get_attention_view(pane).subagents_uncertain`, and
`ctx.attention.subagents_uncertain` in a title formatter, tell the two apart. A
manual renderer that draws from `get_attention` shows nothing where the bundled
renderer shows `+?`, until it reads that field.

## A record write can be reported failed after readers already see it

Every record is written the same way, by `atomic_replace_bytes` in
`src/records.rs`: the bytes go to a temporary file, which is synced and renamed
over the record, and then the directory is synced so that the rename survives a
crash. If that last sync fails, the write is reported failed
(`state_permissions`, "state directory could not be made durable") although the
new record is already in place and every reader sees it. The hook then reports
its event as failed, and the records its plan would have written after that one
are not written. A directory that cannot be opened for the sync is not synced,
and the write is reported as made, though a crash can still undo it.

This holds for every record kind, `children.json` included: a `SubagentStop`
can be reported failed after the sub-agent has left the set, and the lifecycle
observation that event would have written after it is then missing. Later
writes are not misled, because every writer reads the records as they stand,
under its locks, before it decides, never what an earlier hook reported; the
[record contract](record-contract.md#consumer-boundary) says a failure after the
rename requires reading the actual state before a retry. Telling a visible write
from a durable one would need a state that every reader understands, and no
record kind has one.

## A mux server whose socket was removed, replaced or refuses keeps its records

Sweep reclaims the panes of a server it can show has exited. A GUI that quit is
one: its socket is its own `gui-sock-<pid>`, and no process with that pid exists,
whether its socket file was left behind or not. So the records of GUI-local
panes are reclaimed by the two-observation rule, and after the retention age
their pane trees go too.

A server whose socket file no longer exists, whose path now holds a different
socket, or whose socket refuses connections, is another matter. It may still
run with its socket file deleted, or with another server bound over the same
path (WezTerm removes a socket file in its way before binding), and a `chmod`
or `touch` on a live socket changes its identity too. A refusal does not show
it gone either: macOS refuses a connection to a live listener whose accept
queue is full, which a server that stopped accepting fills with each listing
that timed out, and WezTerm stops accepting after its first accept error while
the server and its panes run on. Only one thing outside the server tells a
stopped one from a running one: the process listing, when it read every process
of this user and none carries that socket and pane id. When it cannot say that,
the records are kept: sweep neither ends those bindings nor removes those pane
trees, readers report the panes `unavailable` with a `socket_gone`,
`incarnation_changed` or `socket_refused` diagnostic, and `doctor` and `sweep`
report the whole kept history once per code, listing each incarnation with its
`path` and `pane_count`. That is a finding, not an unanswered probe, so `sweep`
and `doctor` still give a complete answer and exit 0 however much of it there
is.

The listing rarely says it. macOS hides the environment of its own system
binaries, Apple's `/bin/zsh` and `/bin/bash` among them, and every Mac runs some
of them as the user, so there the listing never reads every process. On Linux,
one process of the user that has made itself non-dumpable is enough to leave
the listing incomplete, and ssh-agent does that by default, so Linux usually
behaves the same.

Once you know such a server is gone, remove its records yourself: the `path`
each incarnation carries in that diagnostic is relative to the state root, as
in `<state root>/v2/realms/<realm id>/incarnations/<incarnation id>`.

## Empty session directories stay in the index

The session index keeps one directory per provider session, `v2/sessions/<session key>/`.
Retention removes a binding's entry but never the directory, even once it is empty: a bind of
the same session in another pane may be creating its entry there at that moment, and removing
the directory under it would refuse that bind. So one empty directory stays for every provider
session ever bound. Each is an empty directory; remove the empty ones by hand if their number
matters to you.

An entry whose binding is gone stays too. Retention finds an entry only through the binding it
names, so the entry left by a bind that stopped after writing it and before writing its binding,
or by removing an incarnation's records by hand, is never removed. Readers skip such an entry, as
a walk would find no binding there. `doctor` walks every directory the index keeps, so its cost
grows with the number of provider sessions ever started, not with the live store.

## After a reboot, pane retention can wait without bound

Absence sightings are ordered by the monotonic clock, which restarts at boot. A
sighting recorded before a reboot reads as later than now and restarts the
count, which costs a minute. An end sweep writes after a reboot names the
binding event it ends, so it ends that binding whatever its stamp, and the
pane's retention counts from it as usual. A binding end recorded before a reboot
is worse: no sighting from the new boot can be shown to follow it until the new
uptime passes the uptime at which the end was recorded, so retention of that
pane's tree waits until then. A machine rebooted more often than that never
removes the tree. This errs toward keeping records: nothing is removed early.

## The session index trusts every writer to keep it

Once `v2/sessions/complete.json` exists, `bindings --socket` and `inspect` look
for the other bindings of a session only where the session index names them.
Every binding this version writes gets its entry in the same commit. A binding
written without one -- by an older `attention` still running hooks after the
store was marked, or by hand -- is not found as a rival, so a conflict with it
is not reported, until an entry is written for it. `attention sweep --apply`
without `--realm` writes a missing entry for every binding it reads; with
`--realm` it writes none. Realm-wide `bindings` walks
every binding and is not affected. Nothing is removed on the index's word.

## Removing a pane tree removes the lock files it holds

Pane retention removes the pane's whole directory while it holds the pane's
claim lock and the launch lock, and those lock files are inside that directory. A
writer that opened the old lock file and is waiting on it takes the lock on the
removed file once sweep lets go, and a later writer creates a new lock file at
the same path and takes that one, so both can hold "the" lock at once. This can
only happen on a pane whose binding ended more than 30 days ago and that sweep
has since seen absent twice, where no writer is expected.

## Before the GUI knows its own mux, a local pane's realm is checked by socket path only

Any program that prints to a pane can set that pane's `WEZTERM_ATTENTION` user
variable. On a pane in the GUI's own domains, the plugin refuses an identity
whose pane id differs from the pane's own, and once the `attention tab-source`
answer has arrived it also refuses an identity whose realm or incarnation is not
the GUI's own mux. Until that answer arrives, shortly after startup, the realm is
compared with a digest of the GUI's `WEZTERM_UNIX_SOCKET` as the plugin sees it:
the incarnation cannot be checked yet, and the digest equals the writer's realm
only when that path is already canonical. A mismatch in that window is held back,
not refused, until the answer arrives.

## bash-preexec loaded after the first prompt

The bash integration decides at the first prompt whether bash-preexec is
present. When bash-preexec is loaded later in the same shell, the integration's
DEBUG trap is wrapped by bash-preexec's own and no command in that shell is
claimed again. Load bash-preexec before this file, or start a new shell after
loading it.

## Ctrl-C during a claim under bash-preexec on bash 3.2

When bash-preexec is loaded, the claim runs inside its DEBUG trap. On bash 3.2,
the macOS `/bin/bash`, a Ctrl-C there leaves the interrupted functions' names
behind, and current bash-preexec checks that list to decide whether a command
line was typed at the top level. From then on it runs no preexec function in
that shell: not the claim, and not any other tool's hook. Without bash-preexec,
and in zsh, the integration recovers. Start a new shell after interrupting a
claim there. Catching the interrupt instead would let the interrupted command
line run, unclaimed, which is not what Ctrl-C asks for.

## Two GUIs without a writer closing same-numbered windows

A GUI with no working `attention` writer, or whose `tab-source` did not answer
through its whole retry backoff, publishes tab orders under the shared name
`tabs/<window id>.json`, and window ids restart with every mux. When a window
closes, the plugin removes that file only if it still holds the bytes this GUI
wrote, but the check and the removal are two steps. If another such GUI writes
the same name between them, its file is removed, and it is written again only
when that GUI's bar changes. Only GUIs without a source answer share a name.

## A second agent in one launch after the first died without ending

A launch claim covers one command line, so two agents can start under one
claim: `claude; claude` on one line, or a wrapper that restarts the agent.
When the first agent dies without its session-end hook running (killed, or
crashed), its binding is never ended. The second agent's fresh session start is
then refused (`binding_conflict`, "provider start cannot replace the active
binding"), and its later events are ignored (`claim_stale`), so its tab shows no
indicator for that session. The writer cannot tell a dead first agent from one
still running beside the second, as in `claude & claude`, and replacing a live
agent's binding would take over its tab. Nothing is lost: the next command line
gets a fresh claim and binds normally.

## On Linux, only a shell claims a pane

An agent claims its own pane only on macOS in 1.0. The proof reads a process's
controlling terminal, parent and start time from the kernel through `sysctl`,
and 1.0 has that reader for macOS only, so elsewhere the writer refuses
before reading anything. On Linux an agent's events need a
launch id inherited from a claiming shell, as before; an agent started without
one records nothing. The hook command's `WEZTERM_ATTENTION_HOST_PID=$PPID exec`
prefix is harmless there, and for the same reason an event with an inherited
launch id is not checked against its agent's terminal: an agent in tmux inside
the pane, or a session run by a background server, writes to the pane its
environment names.

## An agent's claim belongs to its process, not to one run of it

A self-owned claim names a process: its pid, its start time and the boot it
started in. Everything that process does is one launch. Several sessions in one
agent process, as after `/clear` or a resume, are successive bindings of that
launch, and a process that `exec`s another program keeps its pid and start time,
so the claim goes with it. A claim passes to a new agent only when its owner is
proven gone. Telling one invocation from the next inside a process would need
evidence the kernel does not keep, and a guess would let one run take over
another's pane.

## An agent that claimed its own pane can leave its last activity behind

In a pane an agent claimed for itself, the shell integration's prompt hook
clears the agent's standing activity once the agent's process is proven gone.
Without the shell integration nothing runs at the prompt, so an agent that ends
without a hook of its own ending that activity, because it was killed, crashed
or cut its turn short, leaves its last activity on the tab until the next agent
claims the pane or the pane closes.

## Pressing Esc in Claude Code leaves `thinking` on the tab

Claude Code runs no hook when the user presses Esc to stop a turn: `Stop` does
not run, it has no interrupt event, and the `idle_prompt` notification that
follows a finished turn after a minute without input is not sent either. The
`thinking` the prompt wrote stays on the tab until the next turn ends with
`Stop`, or until Claude Code quits and the shell integration's prompt return
clears it. Focusing the pane does not clear it, since acknowledgement covers
only `stop` and `notify`, and a hook's `thinking` carries no TTL. The writer
has no signal to act on. The only trace is a `[Request interrupted by user]`
line in the session transcript, and an Esc before any output leaves not even
that: Claude Code undoes the turn and puts the prompt back in the input box.
Codex sends `Interrupt`, and is not affected.

Checked against Claude Code 2.1.283. In one session, an Esc before any output
and an Esc after a tool call had run each ran no hook within 90 seconds, while
a normal turn ran `Stop` and then `idle_prompt` 63 seconds later.

## A pane a shell has claimed refuses agents started without its launch id

Once a shell has claimed a pane, whether the bash integration for a listed
command or zsh's `wezterm_attention_claim`, an agent started in that pane
without the claim's launch id is refused on every event (`claim_stale`) and
shows nothing. The shell claim belongs to the commands its shell starts; letting
an agent it did not start write into it is how one agent's work was reported as
another's. Such an agent does not claim the pane for itself either, since that
would take the pane from the shell's commands. Open a new pane to go back to
agents claiming for themselves, or start the agent through the shell's claim.

## A hook run through a relay or a compound command is refused

The writer accepts an agent's own claim only when its direct parent is the
process `WEZTERM_ATTENTION_HOST_PID` names, which the registered hook command
sets from `$PPID` and then replaces itself with `attention` through `exec`. A
hook command that keeps its shell, such as `...; true`, `a && b`, or a wrapper
script that runs `attention` without `exec`, leaves that shell as the parent. A
relay that receives the callback and starts `attention` itself, forwarding the
value or not, leaves itself as the parent or drops the value. Each of these is
refused with `self_claim_parent_unverified` and records nothing, where accepting
it would record the relay or the shell as the agent. This holds against
programs that cooperate with the hook entry; a same-user program that sets the
value to its own pid can still claim a pane for itself, as it can already
forge any record (see the [record contract](record-contract.md#trust-boundary)).

## An inherited launch id is checked only where the hook names its agent

An event that inherited a shell claim's launch id is written only when the
process `WEZTERM_ATTENTION_HOST_PID` names runs on the terminal that shell
claimed from. A hook registered without the variable is not checked, because
nothing else names the process that runs the session. The hook's parent can be
a shell that stayed behind, which has no terminal even under an agent that does
run in the pane; the first ancestor that has a terminal can be the agent that
started a background server from another pane, on that pane's terminal. So
such a hook's events go to whichever pane its inherited environment names, as
the events of a session Codex runs in its shared server do. On Linux nothing is
checked, for the reason in "On Linux, only a shell claims a pane".

## A mark stamped before an unbound clear

`attention mark clear --source NAME` in a claimed launch that no provider
session has bound yet removes the launch's activity, but leaves no record of
when it did. `attention mark` reads its clock before taking the launch lock, so
a mark stamped just before the clear and written just after it shows the
activity again. A bound pane keeps a clear record and ignores such a mark.

## The library is public, and none of it is supported

`src/lib.rs` says the supported interface is the `attention` command and the
plugin's Lua API, and that no Rust item the crate exports is supported for use
outside this repository. This section explains why that is a declaration rather
than an enforced boundary.

The modules are public because the `attention` binary and the integration tests
under `tests/rust` link the library, and those tests drive storage mechanics --
locking, atomic replacement, path construction, durable deletion -- in
`records` directly. Several test files mix such white-box storage tests with
CLI subprocess tests in one module, so they cannot simply move inward.

Documenting the boundary does not prevent an external program from compiling
against the crate; only privacy does that. `publish = false` in `Cargo.toml`
keeps the crate off crates.io, so such a program has to build from a checkout,
and it takes whatever the next commit changes. Once modules start moving to
`pub(crate)`, `clippy::unreachable_pub` becomes a useful lint to turn on.

## Validated record fields are read as if they could be missing

`src/protocol.rs` proves a record's required fields are present and well formed,
then hands callers a `serde_json::Value` that remembers none of it. Later reads
re-derive each field with a fallback, most often
`record["observed_mono_ns"].as_str().unwrap_or("")`. `src/lifecycle.rs` and
`src/maintenance.rs` hold about fifteen of these each.

The fallback is unreachable today. `protocol/v2.json` declares `observed_mono_ns`
required on `activity` and `activity_clear`, `validate_shape` rejects a missing
required field and any undeclared field, the `monotonic_ns20` type requires
exactly 20 ASCII digits, and every read of an ordering record passes its declared
kind. The one read that passes no kind is the tab-order publication in
`src/query.rs`, which is not a record and carries no fence.

It is still a gap because the guarantee lives only in the validator. A future
read that skips validation would compile, and the fallback is asymmetric: `""`
loses as the left operand of the comparison and wins as the right, so a malformed
clear timestamp would not suppress an activity. It would silently stop the clear
from working.

The fix is a borrowed view over the retained `Value`, built after validation,
whose required accessors are total: `observed_at()` returns a fence, not an
`Option<&str>`. Optional fields return `Result<Option<T>>` so absent and
malformed stay distinguishable. The document itself stays whole, because the
record contract is a published surface and lossy typed parsing would break it.
Start with the three reads that carry ordering: activity, activity-clear, and the
binding end whose order fences a binding's sub-agents (`EndMark::of` in
`src/children.rs`). Do not rewrite every site mechanically; some read fields
that really are optional.

A fix is done when a record with a missing or mistyped `observed_mono_ns` is
rejected before any visibility decision runs.

## Record validity is decided in two layers inside one function

`protocol::parse_record_value` checks a record's shape against the manifest and
then does kind-specific semantic validation in the same function. For
`lifecycle_snapshot` it calls up into
`crate::observations::LifecycleSnapshot::validate_semantics`. That call is the
only edge keeping `protocol`, `observations` and `identity` in a module cycle:
`identity -> protocol -> observations -> identity`. There is no runtime
consequence; the cost is that `protocol` cannot be read or tested without the
observation model.

Three ways out were weighed and each costs more than the edge. Moving
`LifecycleSnapshot` into `protocol` drags the whole observation model with it,
since `validate_semantics` touches `Actor`, `ObservationBody`, the elicitation
types, `ResultSurface`, `PostHook`, `PostToolUse`, `ToolResult`, `Lead` and
`Child`. Moving the semantic check down to `records::decode_record` changes what
`parse_record_value` means, from "is this record valid" to "is its shape valid",
while cases in `tests/fixtures/lifecycle/observations.json` expect
`record_invalid` on semantic grounds and the lifecycle and claim-publish specs
treat that function as the authority. A registry that `protocol` calls into is
machinery for one call site.

What resolves it is a decision on whether shape validity and semantic validity
are one verdict or two. If two, `Verdict` gains a variant or the semantic pass
becomes a separate function the reader calls, the fixture corpus gains a column
for which layer rejected each case, and the cycle disappears as a side effect.
That is worth doing when the record set next changes shape, not as its own
errand.
