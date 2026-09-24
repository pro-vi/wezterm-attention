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
`clippy::expect_used`, which fire at 25 sites in the library.

Denying them today would mean adding 25 allow attributes, which announces an
intention while changing nothing. The sites need reading individually: some are
genuinely infallible and want a comment, some want an error path. Until that pass
happens, the table locks in the lints the crate already satisfies and says
nothing about the ones it does not.

To reproduce the count, run `cargo clippy --lib -- -W clippy::unwrap_used
-W clippy::expect_used`. Nineteen of the 25 are `expect` and six are `unwrap`;
`src/query.rs` holds eleven of them. The command also lints `build.rs`, and
reports its one `expect` (cargo sets `CARGO_MANIFEST_DIR`) as a 26th warning,
which is not a library site. Each site is one of three things, and they
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
watermark an activity-clear compares against, and is the fence a parent `Stop`
writes to clear its subagents. `apply_activity` takes `surviving_order` from the
surviving activity and reuses it in the generated `subagent_clear` record, so the
coupling is semantic rather than a shared field name.

One consequence is known and characterised: when an incoming activity is
semantically equal to the published one, the write is skipped, the stored order
keeps its older value, and an event carrying a timestamp between the two can
still publish over it if it commits later. The interleaving is narrow and the
wrong tint clears on the next distinct activity.
`a_deduplicated_activity_does_not_advance_the_ordering_fence` in
`tests/rust/lifecycle_spec.rs` pins the current behaviour so a change to it is
deliberate.

Separating the roles is a record-contract change: a new field the Lua reader must
tolerate on records written before and after, schema validation, pruning in
`maintenance.rs`, reporting in `query.rs`, and a decision about which of the two
meanings the subagent-clear watermark actually wants. That is not a change to
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

The plugin also writes records, through the local `json_value` in
`plugin/overlays.lua` rather than `wezterm.json_encode`, and that renders a
number as `tostring(value)`. Those records carry `schema`, so what the plugin
emits depends on whether it is holding an integer or a float.

It is holding an integer. WezTerm converts a JSON number in
`lua-api-crates/serde-funcs/src/lib.rs`, trying `as_i64()` first and producing
`LuaValue::Integer`, and reaching `as_f64()` and `LuaValue::Number` only when
that fails. The manifest spells `record_schema` as `3`, so the plugin holds an
integer and `tostring` gives `3` under any Lua version. The float rendering that
would produce `3.0` does not arise here.

That leaves the divergence real but close to harmless: two readers accept a
spelling the third rejects, and nothing this project ships produces it. What is
still unverified is the installed WezTerm on a given machine, since the check
above was read from source rather than run, and there is no test that writes a
record through a real WezTerm and validates the resulting bytes with the Rust
validator. That round trip is the missing evidence, and the existing smoke does
not cover it -- it exercises reading and formatting, not the writer boundary.

Tightening the Lua and Python readers to match Rust is therefore a reasonable
change rather than a risky one, and it is deferred for sequencing rather than
danger: it wants the canonical rule stated, that write-and-validate round trip in
place, and raw-JSON fixtures for the integral-float and exponent spellings, since
a decoded fixture cannot express the difference.

## Cleaning up a pane's records asks the mux, and waits when it cannot answer

A pane's v1 flat marker files are named by its pane id. Deciding that nobody writes
them any more is not a question about one window. Before unlinking, the plugin
walks the mux -- every window, every tab, every pane -- and collects the names
in use. Attention's writers do not project those names for panes with v2 records,
so the walk runs only for absent panes that used v1 flat markers.

Two consequences are deliberate.

The walk answers only when it finishes. A pane that will not say which identity
it carries could be the owner, so it makes the search inconclusive, and an
inconclusive search keeps the files and keeps the obligation to ask again. This
is the direction the whole sweep errs in: not being able to tell is never a
reason to delete.

A pane that stays unidentifiable across many polls therefore keeps a pending
deletion pending, and the walk is repeated on each of those polls. It is bounded
-- once per poll, and only when something is otherwise eligible for removal, with
one walk serving every candidate in that poll -- but it is work that a quieter
implementation would not do. Bounding it further would be a performance policy,
and any such policy has to keep the obligation rather than manufacture an answer
by giving up.

The cheaper shape, asking the mux for the pane's old local id first, helps only a
pane that moved without reconnecting: a failed lookup cannot tell a closed pane
from one that came back under a new local id, so it would still fall through to
the walk. It is worth adding after measuring a real callback, not before.

## Sweep collection attributes leftover flats by claim, not by live occupancy

`attention sweep` collects `<id>`, `<id>.agents`, and `<id>.ack` when exactly one
valid v2 claim names that scalar pane id. It does not ask the mux whether a pane
writing v1 flat markers currently occupies the same number. After pane ids reuse, a leftover claim
from an old incarnation can name a live third-party or Pi-fallback marker.
Preview (`attention sweep --json`) lists the stems; `--apply` is opt-in. A pane id shared
by several v2 addresses still refuses. `.review` is never collected.

Uniqueness is a snapshot under the selected owner's `.claim.lock`. Apply does
not hold a tree-wide lock, so a second claim at another address can land in the
same window. Occupancy-blind collection is already opt-in; this snapshot is the
same class of limitation.

The restat that refuses a replaced leftover compares device, inode, nlink, size,
and mtime. Rename (Pi fallback) changes the inode and is refused.
An in-place rewrite of equal size that restores mtime is not.

## What sweep leaves behind

`attention sweep --apply` removes only ended bindings under the retention rules,
subagent records below a retention floor, a closed pane's whole tree under the
pane retention rule in the [record contract](record-contract.md#trust-boundary),
and the leftover files listed there. That rule keeps sweep from
removing state it cannot prove abandoned, and it means four kinds of leftover
stay on disk:

- **An exited GUI's tab-order files that name mux panes.** A file is removed
  only when it names no tab, or when every pane it names is verified absent.
  The GUI's own local panes are, once it has exited (see the next section), so
  a file naming only those goes. A window attached to a mux server names that
  server's panes, which are usually still running, or gone together with their
  socket with nothing to show the server gone, which a reader reports as
  unavailable rather than absent. So that file stays until you remove it. It
  is safe to delete `tabs/<incarnation id>-<window id>.json` by hand once no
  GUI with that window is running.
- **Temporary files from an interrupted write, outside a tree being removed.**
  So is a review that Alt+B had moved aside to `<review>.json.<session>.clear`
  when it was interrupted. They do not stop a binding or pane tree from being
  pruned, and they go with that tree when it is, but sweep collects none on its
  own.
- **Subagent records below the retention floor.** A child record older than its
  binding's floor is already ignored by every reader. It stays until its binding
  or pane tree is removed.
- **A half-removed binding directory in a live pane.** A crash while a binding
  directory was being removed can leave part of it behind. While the pane is
  live, retention does not touch its tree, so the remainder stays.

Collecting any of these would be a new deletion, and would need the same
evidence rule the others have.

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

## A mark stamped before an unbound clear

`attention mark clear --source NAME` in a claimed launch that no provider
session has bound yet removes the launch's activity, but leaves no record of
when it did. `attention mark` reads its clock before taking the launch lock, so
a mark stamped just before the clear and written just after it shows the
activity again. A bound pane keeps a clear record and ignores such a mark.

## The acknowledgement write has no compare-and-swap

`write_v2_record` in `plugin/overlays.lua` reads the existing record only to
check that it is readable, then renames over it unconditionally. Two GUI writers
acknowledging at the same moment can lose one dismissal. This predates v2 records
and is not widened by them.

The sequence needs two GUI processes on one binding:

1. Activity A (`stop`) is published. GUI process G1 reads A and prepares its
   acknowledgement, then stalls before the rename.
2. The Rust writer publishes a different activity B (`notify`) on the same
   binding.
3. GUI process G2 focuses the pane and acknowledges B. Focus moves away.
4. G1 resumes and renames its acknowledgement of A over the acknowledgement of B.

`plugin/reader.lua` suppresses an activity only when the acknowledgement's
`activity_event_id` equals the activity's `event_id`, so B lights again although
a human dismissed it. The loss is durable, not a torn read. No live occurrence
has been observed.

The Rust side reads `ack.json` too, and does not change this. With the
acknowledgement lost, Rust sees an acknowledgement of A against activity B and
reaches the same conclusion the plugin does, so a repeat of B is `skipped` while
B is already displayed. Rust neither adds a failure here nor rescues one.

`docs/record-contract.md` already says a Lua read/check/write sequence is not a
cross-writer transaction, so closing this is a contract change and not a patch.
It needs three things: a compare-and-swap or a lock on the acknowledgement write,
or else one named GUI process that owns it; a decision on whether an older
acknowledgement may ever replace a newer one; and a Lua fixture that can
interleave two plugin instances, which the suite does not have.

A fix is done when that fixture holds G1's rename, lets B be published and
acknowledged by G2, releases G1's rename, and a fresh unfocused read still
suppresses B.

## The library's public surface is wider than its supported surface

`src/lib.rs` names the supported entry points in its crate documentation and
classifies everything else as implementation. Read that first: it is the
declaration, and this section only explains why it is a declaration rather than
an enforced boundary.

`launch` is private and re-exported, which is the shape the rest of the crate
should follow. Most other modules are still public, `records` most consequentially
— it exposes locking, atomic replacement, path construction and durable deletion
because this crate's own tests drive them.

Documenting the boundary does not prevent an external program from compiling
against internals; only privacy does that. `publish = false` in `Cargo.toml`
keeps the crate off crates.io, so such a program has to build from a checkout. Making `records` private is not a
one-line change either, because the currently public, unstable
`read_pane_facts_with_ports` needs
publicly nameable reader types, and several test files mix white-box storage
tests with CLI subprocess tests in one module, so they cannot simply move inward.
The declaration turns an accidental commitment into an explicit unstable one; the
enforcement is separate work.

`identity` is on the supported list, and its types do not keep the proof its
functions establish. `PaneAddress` has three public `String` fields and no
validating constructor, so a linking caller can build an address no environment
could produce. `socket_identity` returns two positional strings, `realm_id` then
`incarnation_id`, which the type system lets a caller swap. The path that derives
these from the environment does validate them. Enforcing it means private fields
with a validating constructor, and distinct types for the two identifiers. Once
modules start moving to `pub(crate)`, `clippy::unreachable_pub` becomes a useful
lint to turn on.

## Validated record fields are read as if they could be missing

`src/protocol.rs` proves a record's required fields are present and well formed,
then hands callers a `serde_json::Value` that remembers none of it. Later reads
re-derive each field with a fallback, most often
`record["observed_mono_ns"].as_str().unwrap_or("")`. `src/lifecycle.rs` and
`src/maintenance.rs` hold about thirty of these each. `src/query.rs` has the
sharper form in its TTL check: `.parse::<u128>().unwrap()` on a record-sourced
string.

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
subagent-clear watermark a parent stop writes. Do not rewrite every site
mechanically; some read fields that really are optional.

A fix is done when a record with a missing or mistyped `observed_mono_ns` is
rejected before any visibility decision runs, and the TTL check in `src/query.rs`
produces a diagnostic instead of a panic when the written timestamp cannot be
parsed.

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
