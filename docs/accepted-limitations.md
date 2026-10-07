# Accepted limitations

What this project knows is imperfect and has decided to ship anyway, with the
reason in each case. It is short on purpose: something leaves this file by being
fixed or by being reclassified as intended behaviour, not by being forgotten.

Most working notes — plans, review write-ups, triage records — stay on the
author's machine and are not in this repository. A few are tracked because
something still points at them: the README and the consumer guide link to
`docs/reviews/lifecycle-contact-results.md`, for one. This
file is the maintained public account, and it is the one to read first.

## `expect` calls that rest on another function

`clippy::expect_used` is denied in shipped code, so every `expect` in `src/` outside
`#[cfg(test)]` sits under an `#[expect(clippy::expect_used, reason = "...")]` that
says why it cannot fail, and the gate fails when one stops firing. Most reasons are
visible on the line. The ones that name another function rest on a guarantee it
gives, so a change there turns the `expect` into a panic:

- the consumer timeout in `run_hooks_event`, which `validate_consumers` returns
  whenever there is a consumer;
- the parent of a binding path in `sweep`, which `collect_binding_files` lists below
  the state root;
- the publication ids in `request_evidence`, which exist because snapshots are
  validated before they are assembled and `classify_tool` pairs a nonblocking
  result only with a question.

A panic from an `expect` in a hook is not a clean abort. It interrupts processing
wherever it lands, so the state a consumer then reads can be missing, stale, or
updated in one file and not another. Replacing one with `.unwrap_or(default)` does not
count as handling it: that turns a loud failure into a silent wrong answer. A failure
that nothing handles should return an `AttentionError`.

## One timestamp field carries four roles

`observed_mono_ns` orders activity writes, fences activity clears, supplies the
cutoff for a Codex parent's child clear, and controls how long a child's
permission request holds `notify` against the lead's tool calls.

A semantically repeated activity keeps its earlier timestamp. Advancing it
would let a repeated Codex Stop clear children that started after the original
Stop. Repeated activity also keeps its event ID and write time.

A lead turn end now checks the recorded lifecycle events under the writer
locks as well. A newer prompt therefore supersedes an older Stop even when
both prompts produced the same `thinking` activity. The tests
`a_recorded_prompt_fences_stop_even_when_activity_is_deduplicated` and
`duplicate_codex_stop_keeps_a_child_newer_than_the_surviving_activity` pin both
requirements in `tests/rust/lifecycle_spec.rs`.

This additional check needs readable lifecycle history. If that history is
unavailable, native activity retains its existing timestamp-based ordering.
Ordinary non-terminal activity writes still use that ordering alone. Splitting
all four roles remains a record-contract change.
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
testing a single flag that any consumer which did not complete will raise, except
one not dispatched for an event skipped on purpose -- and
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
- **A half-removed binding directory.** A crash while a binding directory was
  being removed can leave part of it behind. While the pane is live, retention
  does not touch its tree, so the remainder stays. Sweep finds a binding by its
  `binding.json`, so a directory left without one also stays after the pane
  closes: no command lists it, and `sweep --apply` reports a complete run.
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
  Codex. Both let a command hook opt out with `async: true`, and Claude Code
  also with `asyncRewake: true`, which its hooks reference, read on
  2026-10-01 with Claude Code 2.1.287 installed, says "runs in the
  background"; the README's blocks set neither. Under an asynchronous hook, a sub-agent's older tool
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
  [One timestamp field carries four roles](#one-timestamp-field-carries-four-roles)). If Codex starts sending either report, it can
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
- **Claude Code lists the tasks it still runs when the lead stops.** In two
  headless sessions of Claude Code 2.1.284 on 2026-09-29, all 7 lead `Stop`s
  and all 4 `SubagentStop`s carried `background_tasks`, and its changelog
  adds the field in 2.1.145. A lead `Stop` named each running
  sub-agent under the id its `SubagentStart` carried, and named none once they
  had ended. The list on a `SubagentStop` still names the sub-agent that is
  stopping, so only the lead's `Stop` is read. A sub-agent given a model id that
  does not exist sent `StopFailure` with its own `agent_id` and no
  `SubagentStop`, and neither of the lead's next two `Stop`s listed it. Attention
  ends a counted sub-agent that a lead `Stop` no longer lists, unless it had an
  event after the `Stop` began. One that starts in the few milliseconds between
  Claude taking the list and the `Stop` hook starting is ended too, and counted
  again at its next typed event. An end-to-end run on 2026-09-29, with a real
  Claude Code 2.1.284 session and the README's hooks in a pane of a disposable
  mux server and a temporary state directory, counted a sub-agent that failed
  at its start when it started and ended it at the lead's next `Stop` after the
  failure; the build before this rule (`a199dfb`) left it counted. A sub-agent
  running one quiet `sleep 20` stayed counted through a lead `Stop` that listed
  it, and left at its own `SubagentStop`. Not observed: a sub-agent the API ended mid-run, as
  the two behind the `+2` below were. If Claude Code keeps listing such a
  sub-agent, it stays counted. If the field goes, or `Stop` stops carrying it,
  the count is too high after an unreported end again. If the field stays but
  its ids stop matching, each lead `Stop` ends every counted sub-agent and its
  next typed event counts it again, so the tab reads low in between.
  `tests/python/claude_contact_check.py` repeats the observations above against
  the installed Claude Code; the gate runs it when `ATTENTION_CLAUDE_CONTACT` is set.
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
  Claude Code's hooks reference, read on 2026-10-01 with Claude Code 2.1.287
  installed, says an internal agent's `SubagentStop` carries as `agent_type`
  the agent name the session runs as, set with `--agent` or the `agent`
  setting, and an empty one without it. No such session was captured. If an
  internal agent's tool call carries that name too, one that sends a tool call
  and no stop is counted until its session ends.

## A sub-agent whose end is never reported stays counted until its session ends

Nothing removes a sub-agent for being quiet, so one whose end never reaches the
writer stays counted, as `+1` on its tab, until its session's `SessionEnd`, or
until the pane moves on to another session. A Codex sub-agent also goes at a
later parent `Stop` that is published after its last event; a repeated `Stop`
the writer skips keeps the earlier cutoff. A Claude sub-agent also goes at the
next lead `Stop` whose `background_tasks` no longer lists it, so what stays
counted to the session's end is one that Claude Code still lists, or one in a
Claude Code session that sends no list.
The end goes missing when:

- the `SubagentStop` hook failed, timed out, or was not registered;
- the user pressed Esc on a foreground sub-agent, if Claude Code sends no
  `SubagentStop` then. This is untested: Claude Code 2.1.283 launched every
  sub-agent in the background (2 of 2, also when asked for the foreground), and
  its changelog for 2.1.198 says "Subagents now run in the background by
  default". Esc on the lead while a background
  sub-agent ran fired no hook, and the sub-agent sent its `SubagentStop` when it
  finished its command;
- the API ended the sub-agent. In Claude Code 2.1.283, two sub-agents stopped
  by an API error mid-run sent no `SubagentStop`, while every sub-agent that
  finished in the same session sent one. In Claude Code 2.1.284, one whose
  model id does not exist failed at its start the same way, with a
  `StopFailure` for its `agent_id`, and no lead `Stop` listed it afterwards;
- a Codex sub-agent was interrupted, or a hook ran asynchronously, as the
  previous section describes.

Earlier builds hid a sub-agent ten minutes after its last event, which also hid
sub-agents still running one long command. A count that can stay too high was
chosen over one that drops a sub-agent that is still working. There is no
command to remove such a sub-agent by hand.

## An invalid child set or lifecycle file is started again, and loses what it held

A hook that finds `children.json`, `lifecycle.json` or `children-lifecycle.json`
invalid, or naming another provider than its binding, renames it to
`.<file name>.invalid.<uuid>`, applies its event to a new file, and reports
`record_invalid`. The rename moves the file as it stands: a link to a file that
is not valid is moved aside as a link, and sweep then keeps that binding, as it
keeps any that holds a link; a link to a valid file, or to nothing, is replaced
by a regular file at the next write. The renamed files are write leftovers:
they go with their binding, and sweep collects none on its own (see
[What sweep leaves behind](#what-sweep-leaves-behind)).

What starting again loses differs by file:

- **The child set.** The new set is written even when the event changes
  nothing else. While the set is invalid, until the next event that writes it
  (a child's start, stop, tool call or permission request, a Codex parent's
  `Stop`, or a Claude lead's `Stop` that lists tasks), the tab shows `+?`. The
  sub-agents the
  invalid set held are counted again only at their next event, so one in
  the middle of a long command stays uncounted until it calls another tool or
  stops.
- **A lifecycle file.** The new file holds only the observation the hook was
  adding, and both its pools carry a retention floor just below that
  observation, so readers see the earlier evidence as evicted rather than as
  never recorded. A hook that was already running when the file started
  again, and stamped its observation earlier, falls at or below that floor:
  its observation is refused, as one below an eviction floor is, and its
  consumer is not run. A hook only reads a lifecycle file when it has an
  observation to add, so an invalid one stays in place, and readers show it
  as invalid, until the next such hook.

The rename happens while the hook plans its writes, so if a write in the same
hook fails before the new file is in place, or the lifecycle observation is
itself refused, no file is left. Without a `children.json` the tab shows no
count, not `+?`, until the next event that writes the set. Without a lifecycle
file readers show no evidence from it, and the next observation starts a file
with no floor. A failure after the new file is in place, such as a failed sync
of its directory, leaves the new file, as
[A record write can be reported failed after readers already see it](#a-record-write-can-be-reported-failed-after-readers-already-see-it)
describes.

Counting what an invalid set held, or showing what an invalid lifecycle file
held, would mean trusting a file that failed validation. The moved-aside file
keeps that evidence on disk until its binding goes.

## A child set or lifecycle file you cannot read, or a newer version wrote, refuses every change

A hook never overwrites `children.json`, `lifecycle.json` or
`children-lifecycle.json` when this user cannot read it or a newer version
wrote it. The hook refuses its change to that file with a diagnostic, and the
consumer the hook would have run is not run. Every later change to the file is
refused the same way until you delete it: sweep keeps a binding that holds
such a file, and resuming the same session in the same launch reuses its
directory.

Overwriting a file a newer version wrote would lose what that version
recorded, and a file that cannot be read cannot be shown to be invalid.

## A plugin and a command from either side of the child set show no sub-agents

The plugin reads `children.json`; builds of the command before it wrote one file
per sub-agent under `agents/` instead. A plugin that reads `children.json`
beside a command that does not write it, or an older plugin beside a command
that writes only `children.json`, shows no sub-agents: each finds none of the
files it reads, and the tab shows neither a count nor `+?`.
Updating the plugin does not rebuild the command, so run
`scripts/install-cli.sh` after `wezterm.plugin.update_all()`, as
[Install](../README.md#install) says.

## A record write can be reported failed after readers already see it

Every record is written the same way, by `atomic_replace_bytes` in
`src/records.rs`: the bytes go to a temporary file, which is synced and renamed
over the record, and then the directory is synced so that the rename survives a
crash. If that last sync fails, the write is reported failed
(`state_permissions`, "state directory could not be made durable") although the
new record is already in place and every reader sees it. The hook then reports
its event as failed, and the records its plan would have written after that one
are not written. A failed write of any record written before the lifecycle
observation is reported alone: what the plan itself would have reported, such as
the restart of an invalid child set or lifecycle file, is not in the report. Only the lifecycle
observation, which is written last, keeps the plan's diagnostic under `replaced`
in its write error. A directory that cannot be opened for the sync is not synced,
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
the GUI's own mux. Until that answer arrives, the realm is compared with a digest
of the GUI's `WEZTERM_UNIX_SOCKET` as the plugin sees it: the incarnation cannot
be checked, and the digest equals the writer's realm only when that path is
already canonical. While the plugin is still asking, through the first run and
its retries 2, 5, 10 and 30 seconds apart, a mismatch is held back. Once it has
stopped waiting, a mismatch is refused and the comparison stays the socket-path
digest. It stops waiting when it cannot ask at all, as when no writer is
installed, and then runs no retry; or when none of those runs answered, and then
later retries every thirty seconds can still bring the answer. A GUI whose
`tab-source` never answers checks a local pane's realm by socket path, and never
its incarnation, for as long as it runs.

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

A launch claim can cover more than one agent: in zsh, `claude; claude` on the
line after `wezterm_attention_claim`; in bash with bash-preexec, a line such as
`claude -c; claude`, claimed once from its first word; or a wrapper that
restarts the agent. Bash without bash-preexec claims each agent command
separately.
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

## Cursor Agent sends fewer hooks than Claude Code and Codex

Everything in this section was checked on `cursor-agent` 2026.10.01-e373342
unless a paragraph names another version, and what rests on its code and not on
a run says so.

A Cursor pane that waits for you shows `thinking`, not `notify`. Checked on
2026-10-06, interactive, with a logging hook registered for every hook the CLI
documents:

- A shell command outside the allowlist stopped at "Run this command?". The
  hooks sent by then were `sessionStart`, `beforeSubmitPrompt`, `preToolUse` and
  `beforeShellExecution`; the prompt itself sent none. `preToolUse` and
  `beforeShellExecution` run before the prompt appears, so they cannot say it
  is there.
- The `AskQuestion` tool showed its question dialog and sent no hook, not even
  `preToolUse`.
- The terminal output at the approval prompt held no bell and no notification
  escape sequence, only terminal title changes.

A `Task` sub-agent sent no `subagentStart` or `subagentStop`, on two models
(Grok 4.7 and Claude Sonnet 5), and the `Task` call itself got no `postToolUse`.
The sub-agent's own tool hooks arrived with a `session_id` that is not the
lead's. The lifecycle ignores them as not the current binding: a `preToolUse`
answers `ignored` and a `postToolUse` answers `partial`, each with `claim_stale`
in the hook's own output, and nothing is written. So Cursor has no sub-agent
count, and the lead's `thinking` stays until its `stop`.

Cursor sends `stop` twice for one Esc, with status `error` and `aborted`.
Cursor ran the two at the same time: with a hook that waited a second before
logging, both were logged 30 ms apart, in the opposite order to an earlier run.
Both statuses clear the pane's activity, because mapping `error` to `notify`
would leave a stale `notify` after an Esc whenever the `error` hook happened to
read its clock after the `aborted` one: each hook stamps its observation when
it starts, and the two start together. The cost: a turn that ends on a real
error shows nothing. No genuine mid-turn error was observed. From cursor-agent's
code, not from a run: a failed turn sends one `error` stop and no
`aborted`, and the last `turn_ended` line of the transcript file named in
`transcript_path` says `error`, where an Esc's says `aborted`. Attention does
not read that file.

In print mode (`cursor-agent -p`), cursor-agent 2026.10.01-e373342 sent
`sessionStart`, the tool hooks and `sessionEnd`, and no `beforeSubmitPrompt` and
no `stop` (one run, 2026-10-06, with an extra `afterAgentThought` hook
registered). The pane shows `thinking` from the first tool call, and nothing
clears it when the run ends: the next prompt of a shell with the integration
does, and with none it stays until the next agent claims the pane.

After `/new` the new conversation's hooks carry another `session_id`,
`sessionStart` is not sent again, and the `sessionEnd` at exit names the first
(without `/new`, every hook of the lead carried the same one; a sub-agent's tool
hooks carry their own, as above).

With a prompt on the command line, `cursor-agent "…"`, Cursor sent the first
`beforeSubmitPrompt` 0.07 s before `sessionStart`, and did not wait for a
`sessionStart` hook that took 3 s (one run, 2026-10-06). The prompt binds the
session first, and claims the pane under the proof a start uses, so the binding's start
source reads `clear`, not `startup`.

A resumed session sends no `sessionStart`. A run with `--continue` on 2026-10-07
sent `beforeSubmitPrompt`, `stop` and `sessionEnd` and no `sessionStart`. In the
code the hook runs only while no session id is being resumed, and a session id
is set by `--resume`, `--continue`, `resume` and `ls`; the other three were read
in the bundle, not run. `sessionEnd` is still sent. The first prompt of
such a session binds it with source `clear` and claims the pane under the proof a
start uses: on macOS, a pane nobody claimed or one whose earlier agent is gone,
never a live agent's or a shell's, and not at all on Linux or with self-claim off.
A prompt of any other provider cannot claim.

Cursor builds a hook's environment partly from somewhere other than the pane:
in one run the hook process saw `WEZTERM_ATTENTION_DIR` and
`WEZTERM_ATTENTION_ROOT` values the pane did not have, while `WEZTERM_PANE` and
`WEZTERM_UNIX_SOCKET` came through (2026-10-06; the
mechanism was not read). The launch id a bash claim exports did reach every hook
of a run (2026-10-07, macOS, bash with the integration loaded). If your pane's
state directory differs from the one
your shell startup files give Cursor, a Cursor hook writes to the second and
the plugin reads the first: set `WEZTERM_ATTENTION_DIR` in the hook command.

A `sessionEnd` hook that is slow to reach `attention` is lost. On 2026-10-07 the
hook shell's parent, the process named `cursor-agent`, was alive 1 s after the hook
started and gone 2 s after, while the hook shell itself ran on, and the shell
prompt came back about 4 s after the second Ctrl-C whatever the hook took. A hook
that reached `attention` after 2, 4, 6 or 8 s was ignored as
`self_claim_parent_unverified`. The binding stayed `active`, and the shell's next
prompt cleared the activity. The command in the README reaches `attention` in
milliseconds and was applied in every other run, so this matters only to a hook
command that does other work first.

Three more bounds, from reading the code and not from runs:
- A second `cursor-agent` that inherits the launch and sends a prompt takes
  the launch's binding and ends the lead's when its session differs; when it
  resumed the lead's own session, the prompt keeps that binding, or binds it
  again if the first run's `sessionEnd` ended it, and ends nothing. [A second
  agent started after the first died](#a-second-agent-in-one-launch-after-the-first-died-without-ending)
  is refused. A `-p` run sends no prompt hook. In
  the one spawn path of Cursor's shell tool that was read, commands run over
  pipes, which would leave an interactive nested agent without a terminal.
- The prompt binds the new conversation, ends the one it replaced, and writes
  the activity as three separate writes. If one write fails, the ones after it
  do not happen: a failed end leaves the replaced conversation active and the
  prompt without `thinking` until its first tool hook. Only Cursor's own bindings are ended this way; one
  of another provider in the same launch is left as it was.
- The `sessionEnd` after a `/new` ends two bindings in two writes. The hook
  reports the more complete of the two ends (`applied`, then `skipped`, then
  `ignored`); a conflict on the named session stays reported. A consumer executable
  (`--consumer`) is told of neither, and Cursor registers none.

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

## A sub-agent whose permission was granted reads as waiting until its next tool call

A sub-agent is marked waiting when it asks for permission, running again at
its next tool call (`PreToolUse`), and removed when it stops. No event
Attention records marks the moment the user grants the permission, and
`PostToolUse` does not change the mark. So
while the granted tool runs, a long build for example, the sub-agent still
reads as waiting, and a lead `Stop` in that time is recorded unheld, so a
sound reader plays it as finished although the lead will wake itself. This errs
toward a call the user did not need, never toward a missed one. Claude Code
2.1.289 lists the sub-agent as running in `background_tasks` either way, so
the Stop cannot tell the two apart.

## A Codex turn that ends on an API error leaves `thinking` on the tab

Codex runs no hook when a turn ends on an error. Its turn loop runs `Stop` only
when the model finished without needing a follow-up, and `Interrupt` only when
the user interrupted the turn. An error from the model request reports the
error to the client and leaves the loop, so neither hook runs. The `thinking`
the prompt wrote stays on the tab until the next turn ends, or until Codex
quits and prompt return clears it, as with an Esc in Claude Code. The writer
has no signal to act on. Claude Code sends `StopFailure` for the same case, and
is not affected.

Read in the Codex source at commit `985cf47a4` (a development commit, not the
`rust-v0.157.1` tag): `run_turn_stop_hooks` in `core/src/session/turn.rs` and
`run_turn_interrupt_hooks` in `core/src/tasks/mod.rs`, and the `Err` arms of the
turn loop that `break` without calling either. The path was read, not traced.
[openai/codex#22774](https://github.com/openai/codex/issues/22774) asks for a
hook on this case; it was open, with no pull request, on 2026-09-29.

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

Twelve of its fourteen modules are public, because the `attention` binary and
the integration tests under `tests/rust` link the library, and those tests
drive storage mechanics -- locking, atomic replacement, path construction,
durable deletion -- in `records` directly. Several test files mix such white-box storage tests with
CLI subprocess tests in one module, so they cannot simply move inward.

Documenting the boundary does not prevent an external program from compiling
against the crate; only privacy does that. `publish = false` in `Cargo.toml`
keeps the crate off crates.io, so such a program has to build from a checkout,
and it takes whatever the next commit changes. Once modules start moving to
`pub(crate)`, rustc's `unreachable_pub` lint becomes a useful one to turn on.

## Validated record fields are read as if they could be missing

`src/protocol.rs` proves a record's required fields are present and well formed,
then hands callers a `serde_json::Value` that remembers none of it. Later reads
re-derive each field with a fallback, most often
`record["observed_mono_ns"].as_str().unwrap_or("")`. `src/lifecycle.rs` and
`src/maintenance.rs` hold about a dozen of these each.

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
