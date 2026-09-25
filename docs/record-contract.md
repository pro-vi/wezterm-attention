# Record contract

This document describes the v2 records the `attention` command writes, and how they relate to the older v1 flat markers. "v1" and "v2" name those two formats, not releases of this project. `protocol/v2.json` is the machine-readable authority. Writers must call `bin/attention`; examples
and provider hooks must not construct v2 record JSON themselves.

This is an implementation contract, not an activation claim. `bin/attention` selects the Rust writer, but a hook registered before it was installed keeps doing what it did: a helper that writes flat files keeps writing them, and a hook registered without `WEZTERM_ATTENTION_HOST_PID=$PPID exec` never claims a pane for its agent.

## Identity and paths

A v2 pane address contains a realm digest, socket-incarnation digest, and canonical server pane ID.
GUI-local pane IDs never name v2 files. Launch IDs, provider bindings, child identities, and review
owners remain separate.

The state root is `WEZTERM_ATTENTION_DIR` when it is set and non-empty, else
`$XDG_STATE_HOME/wezterm-attention` when `XDG_STATE_HOME` is set, non-empty and absolute, else
`$HOME/.local/state/wezterm-attention`. The Rust writer, the plugin and the Pi extension use this
one order. A relative `WEZTERM_ATTENTION_DIR`, or one longer than 4096 bytes, holding a control
character or not UTF-8, is an error to the writer; the plugin, and Pi without a configured writer,
ignore it with a warning and fall through to the next rule, and Pi with a configured writer refuses a
value that is not UTF-8 where it decides the root and does not start the writer. An `XDG_STATE_HOME` that is relative, too long or holds a control character is skipped by all three. An absolute one
that is not UTF-8 is an error to the writer when it decides the root, even when it is also too long or
holds a control character, as such a `WEZTERM_ATTENTION_DIR` is, because the writer cannot name that directory; the plugin skips it with a warning, and Pi
skips it with a warning or, with a configured writer, refuses it, so the failure is reported on
every side.

A text field is safe only when it contains no control character: nothing in U+0000–U+001F, U+007F
or U+0080–U+009F (Rust's `char::is_control`). The Rust writer, `plugin/protocol.lua` and the
fixture checker apply the same rule to every record text field, so a record holding a C1 character
is refused on write and invalid on read.

```text
v2/realms/<realm>/
  realm.json
  incarnations/<incarnation>/
    incarnation.json
    panes/<pane>/
      claim.json
      absence-probe.json
      reviews/<owner-key>.json
      launches/<launch>/
        activity.json
        ack.json
        current-binding.json
        bindings/<binding>/
          binding.json
          activity.json
          activity-clear.json
          end.json
          ack.json
          lifecycle.json
          agents-clear.json
          agents-floor.json
          agents/<agent-key>.json
v2/sessions/
  complete.json
  <session-key>/<entry-key>.json
```

`v2/sessions/` is the session index: one `session_binding` record per binding, under the
`session_key_input` digest of its provider and provider session id and named by the
`session_entry_key_input` digest of the binding record's path. It holds the binding's address,
launch id and binding id, so `bindings --socket` and `inspect` find the other bindings of a
session by reading one directory rather than every binding. It is derived state, never the only
copy of anything: a binding is written with its entry in the same commit, and a reader trusts the
index only while the `session_index` record `complete.json` is present. A claim that starts a new
store writes that record; a store with bindings from before the index gets it from the first
`sweep --apply` without `--realm` that reads every binding and gives each one its entry. A binding
written into a store marked complete by an `attention` binary older than the index has no entry,
so it is not found as a rival until the next `sweep --apply` without `--realm`. Without that
record, readers walk every binding as before. Retention removes an entry with the binding or pane
tree it names; the session's directory in the index stays, empty, once its last entry is gone.

State that is not addressed by a pane lives outside that tree and outside this manifest. The tab bar publishes the order it draws at `tabs/<incarnation id>-<window id>.json`, one file per identified GUI source and window; it names no pane address, carries no pane execution fence and no TTL, so it carries its own `schema` (currently 2) and is versioned separately from `record_schema`. That is the rule for any published fact with no address to validate against: a local schema field, not a manifest entry, because a record-tree change must not refuse a file that has nothing to do with it. `attention tabs` reads them. The process that wrote a file withdraws it when its window closes, and only its own files; a file whose writer has exited is collected by `attention sweep` when every pane it names is verified absent, or when it names no tab at all. See the [consumer guide](consumer-guide.md) for what the order does and does not promise.

The binding record is durable before its pointer. Rust provider/CLI transitions use the appropriate lock scopes and atomic per-file replacement. Lua acknowledgement and review operations validate their targets but use per-file atomic replacement or removal without those Rust locks; a read/check/write sequence is not a cross-writer transaction. Raw child and review IDs never become filenames.


Schema 2 carries `source` with canonical `socket_path`, `realm_id` and `incarnation_id`, derived from the publishing GUI socket. The filename must match its incarnation and window ID. Schema-1 files at `tabs/<window id>.json` remain readable with no known source. Equal window numbers do not associate legacy files with new sources. Cleanup uses the validated file path rather than reconstructing one from a window number.


Window checks are derived per query and never stored in publication files. Their statuses are `present`, `not_listed` and `unavailable`; source identity failure cannot produce `not_listed`. The recorded source namespace is separate from a pane realm, so realm-filtered sweep still leaves tab publications alone.

## Claims and how an event finds its launch

A pane's `claim.json` names the launch its records currently go to. A claim is one of two kinds,
told apart by its owner fields:

- A **shell claim** has none of them. A shell writes it for the commands it starts, through
  `attention hooks claim`, and those commands inherit its launch id as
  `WEZTERM_ATTENTION_LAUNCH_ID`. Every claim written before owner fields existed is one.
- A **self-owned claim** has all four: `owner_pid`, `owner_started_sec` and `owner_started_usec`
  (the process start time, as exact decimal integers) and `owner_boot_session_id` (the boot it
  started in, as a lowercase UUID). An agent's own hook writes it, for that agent's process, and
  nothing inherits its launch id.

A claim with some but not all owner fields is invalid, never a shell claim. The record schema stays
3: the fields are optional and additive, every earlier claim reads as before, and a reader that
does not know them refuses a self-owned claim as invalid rather than misreading it. Every reader
refuses a claim that names some but not all owner fields as `record_invalid`: the writer and the
queries through the Rust record layer, the plugin reader, and `tests/fixtures/v2/check.py`.

A provider event finds its launch in this order, and stops at the first rule that applies:

1. An inherited `WEZTERM_ATTENTION_LAUNCH_ID` decides alone. It must match the pane's claim;
   a malformed or different one refuses the event (`record_invalid`, `claim_stale`), whatever
   else is true. Nothing below is asked of an event that carries one.
2. Without one, the event is refused when self-claim is switched off
   (`WEZTERM_ATTENTION_ENABLE_SELF_CLAIM` set to anything but `1`) or the platform is not macOS
   (`claim_stale`); when `WEZTERM_ATTENTION_HOST_PID` is missing or is not a positive decimal pid
   (`self_claim_parent_unverified`); and when the pane holds a shell claim, for every event
   (`claim_stale`).
3. Otherwise the writer proves its host. Its direct parent, as the kernel reports it, must be the
   process `WEZTERM_ATTENTION_HOST_PID` names, alive, this user's and not replaced while it is
   read, and the writer must not be traced (`self_claim_parent_unverified`). The writer's own
   controlling terminal, or, only when the kernel says it has none, the parent's, must be the
   device of the terminal the mux lists for `WEZTERM_PANE` on the current socket; the listing must
   succeed and name the pane exactly once. A terminal the kernel cannot report refuses
   (`probe_unavailable`); a different terminal refuses (`unsafe_tty`).
4. A session start then claims, under the pane's claim lock and no other lock, after reading the
   same host again. No claim: a new self-owned claim with a new launch id. The same process's own
   claim: kept as it is, not rewritten. Another process's claim: replaced with a new launch id only
   when that process is proven gone, meaning the boot session differs, no process has its pid, or
   the process there started at another time; a live owner keeps the pane (`claim_stale`), and a
   zombie or an owner that cannot be read keeps it too (`probe_unavailable`). Creating or replacing
   needs the parent to be in the terminal's foreground process group; it need not lead that
   group, as a native agent started by a launcher process does not. Keeping does not need it.
   The claim is published to the proven terminal before the lock is released.
5. Any other event resolves only against a self-owned claim whose owner is the process it proved.

Lifecycle observations are kept only for an event with an inherited launch id; an event resolved
through its own agent's claim writes the rest of its records and reports the lifecycle as not
persisted.

Every writer of a launch's records takes the launch lock, then the pane's claim lock, then a
review owner's lock where it also writes a review; `attention mark review`, which writes only the
review, takes the claim lock and then the owner's. Under them it reads the claim again and writes
only if the claim is exactly the one the event was resolved against and, for an event without a
launch id, the same host still proves itself; otherwise it refuses and writes nothing, in either
launch. A claim writer takes the claim lock alone. Publication to a terminal holds the pane's claim lock
from reading the claim through the whole write. A pane with no state directory has no claim and no
lock: its publication names only the pane, which leaves any launch a claim published in place.

## Ordering and wall age

`observed_mono_ns` orders competing writes and supplies activity-clear, child-clear,
retention-floor, and absence fences. `written_at_unix_ns` is required on activity, child presence,
binding, and binding-end records. It supplies TTL and 30-day retention age.

A binding-end record ends the binding event its `binding_event_id` names, and any binding it was
observed at or after. The name is what orders an end across a reboot: the monotonic clock restarts
at boot, so an end sweep writes after one carries a smaller stamp than a binding recorded before
it. Both writers name the event; an end that names none is ordered by its stamp alone. Any other
end belongs to an earlier binding of the same id, which a resume replaced, and ends nothing.

Exact TTL equality remains eligible. The first ineligible instant is one nanosecond later. Missing,
malformed, unavailable, or negative wall age fails closed: TTL-bearing state is omitted, retention
does not prune it, and diagnostics report `record_invalid`, `probe_unavailable`, or `clock_skew`.

A lead `UserPromptSubmit` starts the turn's `thinking` activity for Claude and Codex, the way
Pi's `agent_start` does, so the pane is tinted from the prompt rather than from the turn's first
tool call and a turn that calls no tool still shows activity. The first `PreToolUse` of that turn
repeats the same `thinking` and is skipped. A child actor cannot write lead state, so its prompt
stays observation-only. The one exception is a child's `PermissionRequest`: a child blocked on a
permission prompt waits for the user as the lead would, so it publishes lead `notify` activity and
records the child's presence as waiting (`source: "permission"`); its observation still names the
child as the actor. While that child waits, the lead's next `thinking` does not replace the visible
`notify`. The wait has no time limit: it ends at the child's next tool call, its `SubagentStop`, a
parent clear, or the retention floor, and never at the presence TTL, because a child blocked on a
prompt sends nothing that would refresh its presence. A user prompt or any lead activity other than
`thinking` replaces the `notify` at once. A child presence that cannot be read does not stop the
`notify`; the event reports `partial` with that record's diagnostic.

A turn that ends without `Stop` still ends the activity. A lead Claude `StopFailure` (an API error
ended the turn) publishes `notify`, because the user must act. A Codex `Interrupt` writes an
activity clear for that session, because the user stopped the turn and there is nothing to report;
it does not touch a Pi review. Both keep their lifecycle observation. A child's `StopFailure` stays
observation-only.

`SessionStart` with source `fork` binds the forked session for Claude and Codex, replacing the
active binding as `resume` and `clear` do.

A provider event with a malformed optional field keeps its action and loses only that field. The
fields are `agent_type`, `transcript_path` or `session_file`, `cwd`, `CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, `PI_CODING_AGENT_DIR`, `model`, and the Pi bus `label`. One `record_invalid`
diagnostic names them, with message `optional fields were dropped: …` and the list in
`context.dropped_fields`. A malformed `session_id` or `WEZTERM_ATTENTION_EXPECTED_SESSION_ID` still
ignores the event, because those are identity, not metadata. A native enum value this version does
not know keeps the observation: an unknown `error_category` becomes `unknown`, and an unknown
`input_source` or compaction trigger is omitted.

An activity-clear watermark hides activity at or below its monotonic observation. A strictly newer
activity reappears. Child presence behaves the same way across active, stopped, parent-clear, and
retention-floor records. A stopped snapshot is retained because deleting it would discard the
ordering fence.

Prompt return is `hooks publish` from a bound pane. It republishes the pane identity and writes an activity-clear watermark for the current lead activity only. It never clears child presence and never writes `end.json`. A shell that inherited a launch id clears that launch. A shell without one, in a pane an agent claimed for itself, clears the claim's launch only once the claim's owner is proven gone by the test a replacing claim uses: another boot session, no process at the pid, or a process there with another start time. An owner that still runs, or whose state cannot be read, keeps its activity, and so does a shell claim. The watermark is written under the launch lock and then the claim lock, only while the claim is the one that was read.

## Compatibility and precedence

Attention's writers do not maintain v1 flat marker or `.agents` projections.
The flat format remains permanently supported input: third-party writers and
Pi's fallback may still create `<root>/<pane_id>` and `<pane_id>.agents`, and
the Lua reader keeps accepting them. Writer-owned leftovers from development builds that
projected v2 records into those names are collected with `attention sweep --json` to preview,
then `attention sweep --apply`. Each apply makes up a fresh operation id and
reports it in `result.operation_id`. `--operation-id` (a canonical lowercase UUID)
exists to retry an interrupted run: a run under an id already used is treated as a
replay of that run, so it ends no binding and advances no retention floor, because
the absence rule needs two observations under different ids. Collection follows a unique
v2 claim for that scalar pane id; it does not ask whether a live writer of v1 flat markers currently
occupies the same number, so preview the stems before applying. `.review` is user
state and is never collected that way.

A valid v2 claim selects v2 records. An invalid or future-schema v2 record is reported and never downgraded to a plausible v1 flat marker.
v1 flat markers are read only when no v2 claim exists, so in a pane with a published claim a flat marker written under the same pane id is not shown. The public Lua query remains six values:
`type, frame, source, reserved, subagents, review`. Without an explicit legacy directory, `get_attention(id)` returns unavailable (`nil`) when the scalar ID is observed at multiple full pane addresses. `get_attention_view(pane)` selects the exact pane instead. The fourth return is reserved and always false; controller ownership is not an Attention fact.

For a pane with v2 records, focusing the active pane writes an exact acknowledgement for the displayed activity
event. `Alt+B` writes the `user` owner claim under that pane's full address. Its clear-all action
removes every valid review claim in the active tab through each pane's full address and leaves
activity records unchanged. Panes on v1 flat markers keep their shipped `.ack` and `.review` behavior.

Acknowledgement records are Lua-owned. Rust validates them during reads and never creates or
removes one on a read. An acknowledgement is removed only with the directory that holds it, when
`attention sweep --apply` retention prunes its binding or its whole pane tree.

An acknowledged activity is no longer displayed, so it is not treated as visible when the next
activity is committed: repeating the same semantic activity after its acknowledgement publishes a
new `event_id` instead of reporting the acknowledged one unchanged. Without that, a turn whose only
activity is a `stop` the human already dismissed would never light the tab again. An unreadable
acknowledgement counts as none, so it can only leave the earlier behaviour in place.

## Consumer boundary

`get_attention_view(pane)` exposes fifteen copied base fields plus an independent cached `lifecycle` facet. See the [consumer guide](consumer-guide.md) for exact availability, request/publication relations, acknowledgement meaning, and display ownership. No `answered`, `currently_waiting`, or complete pending-count claim is made.

Lifecycle evidence stays outside `activity.json` because adding request IDs to activity would change `semantic_activity` equality and could redisplay an acknowledged badge. `append_observation` and the request/focus/result tests enforce that separation. Revisit it only if badge identity is deliberately redesigned, not to simplify one consumer.

`lifecycle.json` has schema 3 and kind `lifecycle_snapshot`. Its full address, launch, binding and provider scope a closed fourteen-kind observation union. Each of its required request/general pools has a separate 64-entry/122,880-byte budget and optional monotonic retention floor. One observation is at most 2,048 compact UTF-8 bytes; the file read is bounded at 262,144 bytes plus one overflow-detection byte before decoding, with at most eight container levels. Both pools and floors are validated and replaced together. The lifecycle file has no TTL.

Unknown fields, nulls, object-shaped arrays, invalid nested child digests, wrong pool membership, duplicate identities, below-floor members, and incompatible provider/tool/question-mode tuples are rejected. Native elicitation correlation includes the MCP server namespace. A local receipt UUID cannot stand in for a native request identifier.

Existing native mutations run before lifecycle replacement. There is no multi-file atomicity promise. An independently valid native effect can survive rich-evidence rejection or a failed sidecar write; partial work is diagnostic and strict hook mode fails. A post-rename failure requires reading actual state before retry.

`attention bindings --json` returns validated facts and four independent axes: binding phase, pane
presence, reader confidence, and binding health. It never returns a resume command. Consumers build
their own argv from the closed provider and session ID fields.

`conflicted` health and the `binding_conflict` diagnostic mean two live claims on one provider
session at different pane addresses. A binding that has ended, whose pane is verified absent, or
whose mux server may be gone (its socket path no longer exists, or holds a different socket)
is history and is left out of that comparison: resuming a session in a new pane, or after the mux
restarts, leaves one behind every time, and marking the live row conflicted would hide the pane
the session now runs in. A row of a server that may be gone still reports `pane_presence`
`unavailable`.

JSON responses contain `schema`, `command`, `status`, `complete`, `result`, and `diagnostics`.
`bindings` also reports where its time went, in `result.timing_ms`: `pane_list` (inside `wezterm cli list`), `process_list` (inside the process probe) and `records` (the rest: finding and reading the records). It is on every answer, without a flag or threshold, so a slow call names its phase.
Default output is bounded. Use `--all` or `--all-details` only when complete detail is required.
Sweep leftover `projection_collection` and `tab_order_collection` rows are listed in full even when other sweep details are truncated.

## Trust boundary

Trust means schema-valid, internally addressed, correctly fenced cooperative state. It is not
authentication against another process running as the same user. State directories and files are
private, but a same-UID process can still forge cooperative records. The plugin creates the state
root and `tabs/` with mode 0700 and tightens a directory you chose with `dir`.

Terminal output is also on the untrusted side. Any program that prints to a pane, including `cat`
of a file or the far end of an ssh session, can set that pane's `WEZTERM_PANE` and
`WEZTERM_ATTENTION` user variables. In the GUI's own domains (local, exec, serial, WSL) the plugin
therefore trusts the pane's own id over a published one, and a published identity naming another
pane is invalid. `WEZTERM_ATTENTION` values over 4096 bytes, and pane ids wider than 20 digits,
are refused. Until the GUI's tab-source answer arrives, the realm is checked by socket path only;
see [accepted limitations](accepted-limitations.md#before-the-gui-knows-its-own-mux-a-local-panes-realm-is-checked-by-socket-path-only).
On a mux-attached pane the published value is the only identity there is.

A current binding is selected by the pane's current claim and then that launch's pointer. A pointer
inside a historical launch cannot make its binding current or confirmed. Doctor validates v2
records in its file and version scope even when a pane has no binding.

Destructive absence needs two sightings of absence under different operation IDs at least 60
monotonic seconds apart. A sighting is a pane-list negative plus an identity-scoped process
negative for the full socket path and pane ID. There the mux answered and does not list the pane,
which is what shows it gone; the process probe is asked only whether a process still carries the
pane, so a process it could not read does not stop the sighting. Socket paths are compared after
resolving their directory, because the realm record keeps the path resolved and a process keeps
it as WezTerm was configured to spell it, through `/tmp` on macOS or a symlinked home; the socket
file itself need not exist.

Every reader and sweep classify the server behind a recorded incarnation the same way, by
whether the socket at the realm record's path still carries that incarnation (the digest of its
resolved path, device, inode and change time) and what else can be shown:

- **Live.** The socket still carries the incarnation and its pane listing answers. Presence is
  decided per pane as above.
- **Exited.** Either the socket file is named `gui-sock-<pid>`, as a WezTerm GUI names its own,
  and no process with that pid exists, since a GUI's local panes end with it (a GUI that quit
  usually leaves that file behind; when the file still carries the incarnation, this is asked
  only after its pane listing failed); or the process probe read every process of this user and
  none carries the socket and pane id. A pane of an exited server reads `verified_absent`, and
  sweep counts it as one sighting under the rule above. Nothing about it is a diagnostic.
- **Gone, not proven.** The socket file no longer exists, the path holds a different socket (a
  new server bound it, or a `chmod` or `touch` changed its metadata, which the incarnation
  includes), or the socket still carries the incarnation, its pane listing failed and it refuses
  a connection; and neither proof above holds. A refusing socket is read exactly as a missing
  one is, in every command, the process probe included. A refusal is not an exit: macOS refuses a
  connection to a live listener whose accept queue is full, and WezTerm stops accepting on its
  first accept error while the server and its panes run on. The server may still run, so every
  record is kept: sweep neither ends the binding nor removes the pane tree, and lists no detail
  for it. Readers report the pane `unavailable` with a `socket_gone` diagnostic ("mux socket no
  longer exists"), an `incarnation_changed` one ("realm socket identity changed") or a
  `socket_refused` one ("mux socket refuses connections"). This is recorded history, not a probe
  that did not answer, so it never makes `doctor` or `sweep` incomplete; they report it once per
  code for the run, in one diagnostic whose `context.incarnations` lists each such incarnation
  with its `realm_id`, `incarnation_id`, `path` (the incarnation's directory relative to the
  state root) and `pane_count`, however many panes it holds.
- **Did not answer.** The socket still carries the incarnation, does not refuse, and its pane
  listing fails or times out. That is an unavailable probe in `doctor` and `sweep` alike: the
  diagnostic is `realm_unavailable` with the listing's own message, and the report is incomplete.
  A tab-order file naming such a pane is kept, and leaves sweep incomplete the same way. So does
  one naming any pane whose absence sweep could not decide for its binding: a pane listing that
  failed another way, or an unlisted pane the process probe did not answer for. A tab-order file
  naming a pane whose realm or incarnation is not recorded is kept too, with the reason
  `not_recorded`; there is no socket to ask, so it leaves sweep complete.

A probe recorded at a monotonic time later than the current clock, as after a reboot, restarts
the count; that can only delay an end. Every other diagnostic of sweep's absence and retention
steps carries the `realm_id`, `incarnation_id`, `pane_id` and `binding_id` it is about, and
appears once per pane per run. Process-probe failure is unavailable evidence, not absence. One failed process listing answers
every pane of that query as unavailable; it is not retried pane by pane, so a query waits on at
most one pane listing per mux socket and one process listing. A realm-wide `bindings` asks its
sockets in parallel, so it waits about as long as the slowest one, bounded by the per-listing
deadline. A sweep preview and `doctor` do the same. A sweep apply looks again for each pane it
decides on, and probes it before taking that pane's locks, so hooks are not held up behind a slow
mux. A pane listing that failed during an apply is not asked again: every later pane of that
socket in the same run reads the same failure, so a mux that never answers costs one listing
deadline, not one per pane.

On macOS and Linux the process probe reads environments, never command-line arguments. On macOS it reads each of
this user's processes' environment with `KERN_PROCARGS2`; on Linux it reads `/proc/<pid>/environ`
for processes this user owns; elsewhere it runs `ps axeww -o uid=,command=` from `/bin` or
`/usr/bin` and keeps this user's lines. If it cannot read its own process's environment, the whole
listing counts as failed, so a permission problem never reads as every pane absent. A process
it lists and cannot read makes the listing incomplete: macOS hides the environment of its own
system binaries, such as `/bin/zsh` and `/bin/bash`, from both `ps` and `sysctl`, `KERN_PROCARGS2`
can refuse a running process, and Linux can refuse `/proc/<pid>/environ`. An incomplete listing
still shows a pane present by a process it read; it never shows a pane absent where it is the
only evidence. On macOS a listing is in practice always incomplete, so there it never shows the
panes of a mux server whose socket is gone absent; see
[accepted limitations](accepted-limitations.md). The `ps` listing used elsewhere cannot tell
which processes it did not read, and counts as complete. Process environments are never printed
or persisted.

Once a pane's current binding ended more than 30 days ago, `sweep --apply` removes the pane's
whole tree, but only after two new sightings of absence under different operation ids at least 60
seconds apart; sightings from before that binding ended do not count. A tree holding any file sweep does not recognise is kept. Each step appears as a `pane_retention` detail, with action
`first_absence`, `too_soon`, `replay_first`, `clear_absence`, `present`, `unavailable`, `prune` or
`keep`; the preview says `keep` wherever apply would keep. Temporary files left by an interrupted
write, a review that Alt+B had moved aside to `<review>.json.<session>.clear` when it was
interrupted, and the lock `reviews/.<owner key>.lock` that `mark review`, `mark clear` and Pi's
review events leave beside the reviews, do not hold a binding or pane tree back from retention; a
lock-like file of any other name or place does. Every removal sweep makes stays inside the state
root: a target reached through a symlinked directory below the root is kept, with a
`record_invalid` diagnostic, and so are subagent records below a symlinked directory. An absence
probe kept that way shows as action `keep` in its `absence` or `pane_retention` detail. Every
other removal a writer makes follows the same rule without a diagnostic: a new claim's removal of
the pane's absence probe, and a clear's removal of a review or activity record, keep the target
below a symlinked directory, and the claim or the clear is still written.

A retention floor advances only across complete monotonic-timestamp groups that were already
ineligible under the prior floor. An eligible member blocks the whole equal-timestamp group.
Binding-history caps are calculated separately for each realm.
