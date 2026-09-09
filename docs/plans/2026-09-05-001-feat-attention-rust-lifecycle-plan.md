---
title: Minimal attention lifecycle layer in Rust
objective: Terminal extensions share reliable pane-to-agent identity, lifecycle state and recent changes without reconstructing them independently.
type: feat
status: superseded
superseded_by: 2026-09-06-001-feat-attention-rust-lifecycle-plan.md
date: 2026-09-05
origin: conversation — attention-v2 intent alignment and authorized redesign
supersedes: 2026-09-03-001-feat-attention-v2-plan.md
review_source: ../reviews/2026-09-05-attention-v2-eight-choices.md
module_spec: ../reviews/2026-09-06-attention-v2-modules.md
---

# Minimal attention lifecycle layer in Rust

> **Review hold:** The user retains separate binding/child files and requires safe handling of record growth and delayed callbacks. History is conditional on cost. The single-pane snapshot, removal of child compaction, 64-event history and manual Bash launch requirement below are earlier proposals, not approved build instructions. Revise the affected contracts and units after the [eight-choice review](../reviews/2026-09-05-attention-v2-eight-choices.md); do not build this draft as written.

## Intent and authority

The user described the common shape in herdr, orca and luvus as “binding hooks all around agents” and using them “for various reasons.” The instruction is to “iterate into the minimal plan,” “reimplement in rust,” and “allow redesign. cut unearned features.” Those applications are references, not integration targets.

This layer binds observations to the correct terminal agent and makes them available to consumers. The title bar, Relay, navigation and workflow observers are consumers; none defines the entire data model. A stopped turn is not a successful task, a dismissed notification is not a forgotten session, and missing evidence is not an ended agent.

This proposed replacement supersedes the old implementation design, not its demonstrated safety failures. The old plan, program and HTML remain historical references. They are not additional requirements for the Rust builder. U1–U7 name the earlier build; U8 remains bootstrap-owned activation. New implementation units are U9–U14.

### Requirements and actual consumers

| ID | Required outcome | Consumer or reason |
|---|---|---|
| R1 | Bind full pane address, agent launch and actual provider session; reject stale or ambiguous attribution | Lua title bar, Relay and CLI |
| R2 | Interpret supported provider activity, child work and ending consistently | Claude/Codex hooks, Pi; existing provider fixtures |
| R3 | Recover current/last-known facts after GUI reconnect or server loss without sending agent input | GUI republish; bootstrap recovery |
| R4 | Read current facts and catch up on a bounded suffix of accepted lifecycle changes, explicitly admitting gaps | Notification/automation consumers; cursor exerciser in U12 |
| R5 | Preserve shipped attention, focus acknowledgement, user review and v1 input/API behavior | Existing Lua configuration and hook writers |
| R6 | Install and diagnose one Rust executable, with bounded failures and safe explicit state cleanup | Shells, hook registration and operators |

R4 adopts the narrow catch-up recommendation as a design choice: 64 compact changes per pane, not a complete event archive. It does not promise to reconstruct unobserved provider events or every intermediate turn.

## Scope cuts

| Earlier feature or mechanism | Replacement decision |
|---|---|
| Fourteen independently persisted v2 record kinds and per-binding directories | One Rust-owned pane snapshot; separate acknowledgement and per-owner review files |
| Separate claim and current-binding pointer commits | Current claim and binding are fields in the same atomic snapshot |
| Searchable catalogue of every replaced binding; 30-day/500-binding history policy | Last-known binding per pane plus the bounded change suffix; no conversation archive |
| Process scanning and two-observation inferred binding endings | Report pane availability separately; only accepted provider ending ends a binding |
| Child-file retention floors and floor-before-unlink compaction | Retain child rejection evidence within the current binding; discard that set on binding replacement; explicit capacity failure instead of a compaction subsystem |
| Nanosecond-exact wall-age expiry and deadline callbacks | Millisecond UTC age, evaluated on the normal poll; retain monotonic ordering separately |
| Settled-title sampling, title-churn diagnostics and new built-in provider suffix options | Preserve shipped title formatting and customization; expose provider facts to extensions |
| Expected-session, model and transcript-path persistence | Consumers keep their expectations; retain actual provider/session plus optional cwd/config directory needed for recovery |
| Rust-to-v1 output mirroring and projection repair | Coordinated consumer cutover; preserve v1 inputs and old Lua API, not old readers of new records |
| Python production authority and Python UUID subprocesses in shell integration | Rust binary; one shell claim/output/export handshake |
| Bash command parsing and DEBUG-trap chaining | The same explicit pre-launch claim used by zsh; ordinary commands remain untouched |
| Generic provider registry, pluggable storage, daemon, SQLite, native Lua module, global event index | Static provider adapters and ordinary files |
| Built-in workflow execution, wait scheduler, resume launcher, topology restoration, transcript/reply storage | Consumer responsibilities; expose facts and cursors only |

These are authorized cuts, not an untracked future backlog. Reintroduce one only for a named consumer requirement. Windows lifecycle publishing, additional providers and binary auto-download/update are outside this implementation; shipped v1 Lua remains available.

## Naming and representation ledger

| Meaning | Name and disposition | Authority / consumers / necessary mirror |
|---|---|---|
| One server pane in one socket lifetime | `PaneAddress`, reuse | Rust validated identity; Lua user-var/state mirror; shared identity vectors |
| One top-level agent process | `LaunchId`, reuse | Rust claim; shell exports it to hooks; inheritance probe |
| Actual provider conversation in that launch | `AgentBinding`, reuse | Rust snapshot; Lua/CLI consumers; provider transition fixtures |
| All Rust-owned facts for one pane | `PaneState`, new | `src/lifecycle.rs`; persistence and CLI, Lua boundary mirror; cross-language fixture parity |
| One accepted semantic change | `LifecycleChange`, new | Rust enum; bounded history query and U12 observer; exhaustive variants and cursor tests |
| GUI assessment of current pane state | `AttentionView`, reuse | Lua cache; built-in formatter and new full-pane accessor; no scalar-ID reconstruction |
| Exact display dismissal / explicit owner request | acknowledgement / review, reuse | Lua acknowledgement/user review; Rust source review; shared overlay fixtures |

The Rust types are the production authority. Lua is a necessary process/language mirror, not a second provider-policy engine. Retire `protocol/v2.json` as the new-format authority; retain it only with old-format fixtures while those are still exercised. Do not add a hand-maintained generic schema interpreter or a generated-code framework. Rust serialization, Lua decoding and shared expected-result fixtures must agree.

## Architecture decision

Use one short-lived Rust executable with a testable pure transition function and three concrete provider adapters. Persist one atomic JSON snapshot per full pane address. Keep GUI-owned overlays independent. Lua reads snapshots during its existing poll and renders only from cache.

This beats the existing per-binding layout because claim selection, binding replacement, lead activity, child clear and recent changes commit together. The existing binding/activity/child writers already share a launch lock; their separate files were not providing independent writer concurrency. It beats SQLite here because Lua otherwise needs another published file or a subprocess to read it, restoring a second commit boundary. No daemon or FFI is needed.

Trade-offs: every accepted child refresh rewrites a snapshot; malformed JSON affects that pane's whole provider snapshot; catch-up is finite; bounded snapshot capacity can make observation incomplete. Those costs have explicit refusal rules and probes below. No line-count or speedup claim is made before implementation.

### Directional data flow

```text
provider hooks / Pi / manual mark
              -> Rust parse -> transition under pane lock -> atomic PaneState
                                                               |          |
                                                        Lua poll/cache  CLI queries
                                                        title bar/Relay state/changes
Lua focus and user review -> independent exact overlays ---------+
```

### Storage and identity contract

- Use storage and wire revision **3** to avoid interpreting existing unshipped v2 bytes as the redesign. The product initiative remains attention-v2. Do not import or delete old v2 state automatically.
- Layout: `v3/realms/<realm>/incarnations/<incarnation>/panes/<pane>/state.json`, `.lock`, `ack.json`, and `reviews/<owner-key>.json`. No realm/incarnation manifests, claim file, binding pointer or child files. Snapshot identity includes the socket path and device/inode/change-time evidence needed by Rust discovery and diagnosis.
- Reuse full socket-path realm identity and socket-lifetime identity. Claims must prove that `wezterm cli list` names the caller's actual controlling tty for that server pane. A surviving old shell must not claim a reused pane number after socket replacement. Socket uncertainty refuses the claim.
- All Rust mutations use the same stable pane lock, never a lock on the replaced JSON inode. Capture OS-comparable monotonic observation before reading hook stdin. Require exact current launch and binding under lock; stale hooks neither alter current state nor recreate old directories. Newer authorized claims replace the claim and reset binding-local state atomically.
- A snapshot holds current claim, latest actual binding, optional activity, child entries, end/clear ordering evidence, completeness, and recent changes. A binding is current iff its launch matches the claim. A new claim clears live activity/children but retains the last binding's scoped recovery facts until the next binding replaces them; queries label those facts last-known, never current. Pending launch activity becomes ineligible when a current binding is accepted. User review remains pane-owned across launches.
- Keep raw bounded child IDs inside JSON, not filenames; uniqueness is validated. Review owner IDs are at most 64 UTF-8 bytes, encoded as lowercase byte hex in filenames, with exact reversible validation. Realm/incarnation/binding IDs remain validated opaque identifiers; Lua need not reimplement SHA-256 for every child.
- Require a local filesystem supporting the tested lock/rename semantics. Write private unique temporary files with create-new semantics, flush/sync, then rename over the prior file; sync the parent directory where supported. Directory mode 0700, file mode 0600. Failure before rename retains the prior snapshot. Successful rename installs the new snapshot; a subsequent directory-sync failure reports uncertain durability, not a claimed rollback. Unknown versions, malformed top-level state and identity mismatch are never overwritten or downgraded.
- Keep bounded lock acquisition (10 ms retries, 2 s deadline) and bounded mux subprocesses (5 s). Hook stdin and tty publication each have a 2 s deadline; tty writes use a nonblocking descriptor. Timeout is unavailable, never absence or a guessed successful write. File-sync latency is measured, not claimed to be cancellable. Test these failure paths with controlled pipes/ttys, not the user's terminal.

### Lifecycle and time contract

- Port the **behavior** of the existing Claude/Codex/Pi provider matrices, not the optional-field `ProviderEvent` structure. Rust variants contain required facts; unknown event/source combinations remain inert and diagnosed. Preserve child/lead distinction, compact/reload confirmation, explicit session replacement rules, nested-agent exclusions, Codex parent clear, and Pi settled-versus-agent-end behavior.
- Manual/provider activity share one transition after target selection. An activity event ID changes only for a newly accepted activity; unrelated child or metadata updates do not revive a dismissal. Codex parent Stop commits lead activity and child-clear evidence in one snapshot.
- Per-target monotonic observations order writes, child stops and clear evidence. Commit sequence orders history only. Serialize large monotonic values/counters as checked decimal strings. Never serialize elapsed time from a freshly created Rust `Instant` as a cross-process observation.
- Wall timestamps are integer Unix milliseconds within Lua's exact-integer range. They select age only, never writer precedence or identity. A child counts through age 600,000 ms and is omitted on the first poll with greater age. Negative/invalid age is unavailable, not eligible. No expiry timer and no expiry write. Activity TTL retains its configured duration under the same rule.
- Retain stopped-child evidence until binding replacement; repeated stop is a semantic no-op. A strictly newer child observation may reactivate it. Removing recent history never removes these rejection facts. There is no child compaction floor in this design.
- Bound incoming JSON at 1 MiB and persist only bounded fields, never raw input, prompts, command lines, transcript content or environment dumps. Bound serialized pane state at 1 MiB, reserving 1 KiB for an ingestion-failure marker. If a mutation cannot fit, preserve all previously accepted child/activity facts and atomically mark observation incomplete. Do not evict active children to fit. Incompleteness is sticky until a new binding/launch resets its state; ordinary later success cannot certify the missed facts. Readers omit an exact-looking child count and automatic consumers must reject incomplete state.
- Syntactically invalid snapshots fail as a whole. A malformed child inside otherwise parseable state is omitted from the read projection and makes it incomplete; Rust mutations refuse that malformed source rather than rewriting away its unknown content.

### Recent changes and consumer contract

- Each pane snapshot has a random epoch, a checked committed change sequence and the last **64** compact `LifecycleChange` entries. The epoch persists across launch/binding replacements, but changes if the pane store is deliberately recreated. Every entry carries its exact launch/binding scope; there is no global order across panes.
- Closed change kinds: `claim_changed`, `binding_changed`, `activity_changed`, `child_changed`, `children_cleared`, `activity_cleared`, `binding_ended`, `observation_gap`. Activity changes retain manual/provider origin and activity type. They do not claim task success or invent provider turn IDs.
- Append for accepted semantic changes, not semantic retries, raw tool pings, TTL refresh, GUI acknowledgement/review, cleanup or hydration. Each summary is at most 1 KiB and contains identifiers/change facts, not cwd/configuration copies or child maps. A new activity and its related Codex clear may produce two adjacent entries in one atomic commit. The first capacity refusal records `observation_gap` using reserved space and sets incomplete state; repeated refusals append nothing. Counter exhaustion refuses rather than wraps.
- No cursor means bootstrap: return current state and a cursor, with **no completion events**. A matching epoch/cursor returns later retained entries. A cursor older than the retained prefix returns `gap` plus the retained suffix and current state; ahead-of-state or foreign-address cursors are errors. Epoch mismatch means reset/resnapshot, not an empty successful catch-up. Consumers own durable cursors and deduplication.
- History is a suffix of accepted observations, not every provider callback, every rendered change or every completed turn. An unobserved tool-free turn cannot be reconstructed from identical Stop snapshots. Consumers needing stronger per-turn guarantees must supply a concrete provider correlation contract; this release does not advertise an exactly-once completion queue.
- `attention bindings --json` returns current/last-known pane binding **and activity** facts, full address, availability, quality and cursor. Availability is present/missing/unavailable based on successful mux enumeration and socket evidence; missing does not synthesize provider ending. Duplicate current provider/session assignments are conflicted, never silently selected. Unknown JSON enumeration rows make the probe unavailable, not an incomplete negative list.
- `attention changes --address <JSON> [--after <cursor>] --json` reads one pane's bounded history, including exact older binding scopes still retained there. No consumer registration, background watcher, scheduler, resume command or unbounded event API.
- Add cached Lua `get_attention_view(pane)` for the actual pane object. It returns binding/provider even when activity is acknowledged. Title formatting and Relay consume the same view. Retain the six-value `get_attention(marker_id)` contract; if a scalar marker ID maps to multiple full addresses, return unavailable rather than guess. New consumers must use the full-pane accessor or CLI full address.
- Query envelopes remain versioned and bounded, with separate state quality and response truncation. Exits: 0 success, 1 findings/gap, 2 usage/cursor error, 3 unavailable. JSON mode emits one document on stdout and no stderr. No command failure includes raw provider data or secrets.

### GUI overlays, shell and operations

- Lua remains responsible for cache/freshness, focus and rendering; no subprocess from normal polling or formatting. One attach-triggered background publisher per missing realm remains permitted. All missing/invalid/unavailable distinctions and last-good-cache scope checks remain explicit.
- Acknowledgement names address, epoch, launch, optional binding and **activity event ID**, not snapshot/history revision. Review is one atomic file per owner. Source clear removes only its owner; Alt+B can clear valid owner requests in the active tab. Concurrent writes/clear are per-file operations, not a claimed cross-owner transaction; a request completed after clear remains visible.
- Preserve shipped priorities, manual renderer, custom formatter, tuple API, v1 formats, Pi bus and puppet field. Default titles use shipped server-name/cwd behavior. Count-only display is neutral. The new lifecycle API does not depend on which indicator wins.
- Simplify both shell integrations to explicit `wezterm_attention_claim && <agent>` plus prompt republish. Remove Bash arbitrary-command parsing and DEBUG-trap chaining. `hooks claim` mints and commits a UUID, outputs exactly that UUID (or the standard envelope with `--json`), and the parent shell exports it only after success. Claim failure clears any inherited launch ID. A committed claim remains successful if tty publication alone fails; report publication pending without losing the exported launch. Accepted hook writes also republish that same identity through the validated tty, allowing recovery before the next shell prompt.
- Preserve `bin/attention` as the stable path and `hooks claim|publish|event`, `mark`, `bindings`, `doctor`; add `changes`. Default recognized provider hooks always exit zero with at most one sanitized diagnostic; strict mode exposes rejection. Pi uses Rust only with a new-protocol launch and available binary; absent integration permits legacy fallback, but rejection of an established new-protocol claim does not.
- `doctor` checks own state, binary/protocol compatibility, publication evidence and socket/tty accessibility; GUI diagnosis remains separate. No process environment scan. `sweep` is only explicit obsolete-incarnation cleanup: preview by default, exact realm/incarnation plus `--apply` to delete recognized old-format-3 pane state. Lock/reread each target and re-prove socket absence/replacement immediately before removal. Every Rust lifecycle mutation revalidates socket incarnation under the pane lock, so it cannot recreate obsolete provider state. Preserve unknown files and malformed/future state. A racing Lua overlay can leave a harmless file but cannot recreate `PaneState`; repeated validated cleanup may remove it. Cleanup erases recovery/catch-up facts, never asserts provider completion.
- There is no automatic total-disk retention promise. History and each snapshot are bounded; retained last-known pane snapshots across obsolete incarnations remain until explicit cleanup. Doctor reports their count/bytes. Keeping this limitation is smaller than an automatic archival policy.

## Rust implementation and dependencies

Use one Cargo package with a library and binary, not a workspace or plugin framework. Proposed files: `src/lifecycle.rs` (types/transition/history), `src/identity.rs` (validated identity), `src/providers.rs` (three adapters), `src/persistence.rs` (locked snapshots/overlays), `src/wezterm.rs` (enumeration/tty publication), `src/main.rs` (CLI), `src/lib.rs` (module declarations).

Use `serde`/`serde_json` for typed JSON, `clap` for help/arguments, `uuid` for random IDs, `sha2` for existing identity digests, and `libc` narrowly for OS clock/UID calls. These replace manual codecs/parsers/cryptography or absent standard APIs; none is installed in this repo yet. Pin resolved dependencies with Cargo.lock; no speculative storage/async/schema-generation dependency.

Use standard `File::try_lock`, `File::sync_all`, `OpenOptions::create_new`, Unix metadata and `IsTerminal` rather than another lock/terminal crate. File locking is stable since Rust 1.89; the inspected local compiler is 1.94.1. Set the package minimum to 1.89 unless actual dependency support requires a declared increase. [Rust file API](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock), [terminal detection](https://doc.rust-lang.org/std/io/trait.IsTerminal.html), [clap](https://docs.rs/clap/latest/clap/), [serde_json](https://docs.rs/serde_json/latest/serde_json/).

One explicit `scripts/install-cli.sh` builds locked release code and installs a version-checked binary into this checkout's `libexec/attention-rs`; the stable shim never builds or downloads during a hook. WezTerm's Git plugin loader does not compile Rust. A missing binary gives one fixed actionable failure; v1 Lua operation stays available. Prebuilt distribution is not required for this build.

## Program obligations and state-action checks

- **O1:** Validated Rust identity types and an exhaustive provider-event enum precede path selection and mutation; Lua mirrors only the reduced serialized contract.
- **O2:** Claim, current binding, activity/children and appended history commit atomically under one pane lock. A change entry exists iff that semantic change was committed; refresh-only writes do not advance change sequence.
- **O3:** A displayed acknowledgement applies iff its full scope and activity event ID match; provider identity remains queryable independently of acknowledgement.
- **O4:** Per-target observation ordering, pane history sequence, wall age and GUI acknowledgement are different concepts. No one counter or timestamp substitutes for another.
- **O5:** Confirmed/complete current evidence is required for automatic actions. Hydration, gaps, unavailable probes and capacity loss never masquerade as new successful completion.
- **O6:** Every retained behavior and intentional cut has a named regression or contract test; obsolete file-layout assertions are replaced only after the corresponding behavior is covered.

| Action × state | Caller observation; durable result; effects; race rule; test |
|---|---|
| Claim, exact live tty | UUID; atomic new claim/reset plus change; bounded OSC output; older claimant loses; `claim_exact_tty` |
| Claim, reused socket/pane number from old tty | unavailable; unchanged; no OSC; revalidate after lock; `old_tty_cannot_claim_reused_pane` |
| Hook, current scope/new observation | applied; state and semantic suffix commit together; no agent input; compare under lock; `concurrent_children_and_parent_stop` |
| Hook, old scope/older observation/unknown source | ignored; unchanged; one bounded diagnostic; cannot recreate old state; `stale_hook_is_inert` |
| Semantic duplicate / genuine child refresh | skipped / refreshed; preserve event ID or update child freshness; no history append; lock decides latest; `retry_vs_refresh` |
| Capacity loss | strict finding/default hook zero; preserve known facts and sticky incomplete marker; observation-gap entry; no eviction of live children; `capacity_never_claims_exact_count` |
| Focus ack racing child refresh/new lead event | exact target write; only matching activity suppressed; no lifecycle event; new lead survives old ack; `ack_targets_activity_not_snapshot` |
| Source/user review overlap | owner-local result or reported partial clear; other owners retained; no lifecycle event; per-file linearization; `review_owner_isolation` |
| Catch-up: initial / retained / gap / future cursor | snapshot / suffix / explicit gap / error; no writes; consumer owns cursor; one captured snapshot; `cursor_bootstrap_suffix_gap` |
| Expiry / probe failure / GUI detach | recomputed view/unavailable; no lifecycle mutation; redraw only on changed view; no inferred ending; `expiry_and_detach_are_not_completion` |
| Obsolete-incarnation cleanup / uncertain identity | preview or explicit removal / refusal; no provider end; locked reread and socket proof; `cleanup_cannot_touch_current_incarnation` |

Non-firing cases for completion consumers: SessionStart/resume, manual `mark stop`, child stop, TTL expiry, acknowledgement, hydration, history gap and namespace loss. Provider Stop is a reported lead stop, not proof of task success. Omitted-state challenge: an old shell surviving socket replacement and a valid snapshot with one malformed child are included above; neither is treated as impossible.

## Implementation units

### U9. Claim a pane and query it through Rust

- **Goal / requirements:** an installed candidate binary claims an exact disposable pane and returns its state; R1, R3, R6. **Dependencies:** none.
- **Files:** Create `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/main.rs`, `src/identity.rs`, `src/lifecycle.rs`, `src/persistence.rs`, `src/wezterm.rs`, `scripts/install-cli.sh`, `tests/lifecycle_spec.rs`.
- **Approach / patterns:** one pane lock and atomic snapshot; reuse current socket/tty tests and `attention.py:613` opened-descriptor validation, not the old file layout.
- **Tests:** *Happy:* exact live tty → claim → UUID/current address. *Edge:* equal pane IDs in two realms and old tty after rebirth → isolated/refused claims; delayed claimant loses. *Error:* missing binary, malformed enumeration, lock/stdin/tty timeout and pre-rename write/sync failure preserve prior state; directory-sync failure after rename reports installed-but-durability-uncertain state. *Integration:* shell exports committed UUID; tty receives OSC and agent input remains empty.
- **Verification / proven through:** real binary against an isolated state root, fake clock/enumeration ports and disposable tty; no input bytes reach the pane.
- **Runtime evidence:** Rust implementation unverified; exercise std locking, cross-process OS-clock order and tty publication in this unit. **Checkpoint:** auto — `cargo test` and isolated claim exerciser; failures block dependent writes, not fixture preparation. **Rollback:** remove only candidate binary/state; no live migration.

### U10. Drive provider lifecycles through the snapshot

- **Goal / requirements:** Claude, Codex and Pi fixture processes produce correct current state and recent changes; R1, R2, R4. **Dependencies:** U9.
- **Files:** Create `src/providers.rs`; modify Rust modules and `tests/lifecycle_spec.rs`; create `tests/fixtures/lifecycle/provider-cases.json`, `state-cases.json`.
- **Approach / patterns:** parse to variants, one pure transition, one atomic commit; carry behavior from current provider fixtures and `attention.py:1139,1274,1392,1569`.
- **Tests:** *Happy:* each supported provider fixture → expected binding/activity/child transition. *Edge:* stop→active continuation, compact/reload and delayed writers → exact current scope; duplicate versus refresh → unchanged history versus refreshed TTL. *Error:* unknown sources, raw null/duplicates/invalid IDs, capacity loss and malformed children → inert or explicitly incomplete state. *Integration:* concurrent real hook processes commit parent Stop/clear together; Pi clear/shutdown matches its fixture.
- **Verification / proven through:** real hook subprocesses using sanitized fixture stdin and injectable time; every retained provider regression maps to a Rust case.
- **Runtime evidence:** new Rust adapter dispatch unverified until exercised; no paid provider calls required. **Checkpoint:** auto — Rust provider/transition cases and 100 real fixture-hook processes. **Rollback:** old writer stays intact until U14.

### U11. Read and render the new state in WezTerm

- **Goal / requirements:** one named cached view serves title rendering and a quiet-pane Relay lookup; R1, R3, R5. **Dependencies:** U9, U10.
- **Files:** Create `plugin/lifecycle.lua`, `tests/lifecycle_reader_spec.lua`; prepare integration changes to `plugin/init.lua`; extend `tests/wezterm_protocol_smoke.lua` and shared lifecycle fixtures.
- **Approach / patterns:** keep old v1 adapter and public tuple; new-format reader consumes one snapshot and independent overlays. Use native `wezterm.json_encode`; carry focused-ack and owner-review behavior, not custom codecs or old child-file parsers.
- **Tests:** *Happy:* Rust state → Lua view → expected indicator and quiet-pane provider. *Edge:* child refresh preserves ack; new activity survives old ack; owner reviews stay isolated; exact millisecond expiry and detach/reload preserve semantics. *Error:* collisions, negative age and malformed/capacity state → unavailable/incomplete, never guessed truth. *Integration:* installed WezTerm agrees with Rust fixtures; poll/render spawn no process and count-only color stays neutral.
- **Verification / proven through:** LuaJIT plus installed WezTerm with candidate module/config in a disposable directory; emitted view and six-value API agree where unambiguous.
- **Runtime evidence:** new reader unverified; existing installed-WezTerm harness supplies contact. **Checkpoint:** auto — disposable module-load/render proof. The checkout may be live-linked: final `init.lua` activation is held until U8 consent, not exercised against the user's window. **Rollback:** discard candidate integration copy; old live module unchanged.

### U12. Expose current facts, catch-up and narrow maintenance

- **Goal / requirements:** a separate consumer can bootstrap, disconnect, catch up or detect loss without guessing; R3, R4, R6. **Dependencies:** U9, U10.
- **Files:** Modify `src/main.rs`, `src/persistence.rs`, `src/wezterm.rs`, Rust tests; create `tests/consumer_contract.rs`.
- **Approach / patterns:** query captured pane snapshots, not a second index. Keep standard layered help, bounded JSON/exits and explicit cleanup preview; remove old process-probe and inferred-end paths.
- **Tests:** *Happy:* bootstrap cursor then new change → exact suffix. *Edge:* 65 changes → explicit oldest-cursor gap; retries/TTL/hydration → no false completion; two panes/epochs never share authority. *Error:* future cursor, duplicate session and unavailable mux → error/conflict/unavailable, not empty success. *Integration:* cleanup preview/apply preserves current and unknown state under a racing writer.
- **Verification / proven through:** independent CLI consumer process records a cursor, resumes, and checks exact events and gap; no source-code imports needed by the consumer.
- **Runtime evidence:** new consumer protocol unverified until subprocess test. **Checkpoint:** auto — CLI contract cases and cleanup preview/apply on disposable state. **Rollback:** remove only disposable data; no global store conversion.

### U13. Prove shell, Pi and existing consumer migration

- **Goal / requirements:** producer adapters need no production Python; legacy inputs remain usable and new consumers retain full identity; R1, R3, R5, R6. **Dependencies:** U10–U12.
- **Files:** Prepare candidate `bin/attention`, both shell integrations, `pi/index.ts`, examples and docs; extend Pi/Bun/Node tests; create `tests/fixtures/consumer-migration/` adapted copies/exercisers for bootstrap bridge and Relay.
- **Approach / patterns:** preserve Pi's Node dispatch/queue and shipped fallback without downgrading a claimed new session. Explicit claim in both shells; no new launcher. Bootstrap GUI already uses the tuple, while bridge reads flat files and must migrate before activation.
- **Tests:** *Happy:* explicit shell claim → inherited UUID; Pi dispatch → Rust state. *Edge:* durable claim/publication failure retains launch export; prior shell traps remain untouched. *Error:* missing binary permits v1 fallback only without a new-protocol claim; no production Python invoked. *Integration:* legacy input/tuple stays compatible, new Rust creates no flat projections, migrated bridge separates realms and Relay identifies an acknowledged pane.
- **Verification / proven through:** sourced shell subprocesses, Node dispatch, and adapted consumer fixtures. Copies prove the handoff contract, not live bootstrap adoption.
- **Runtime evidence:** new bindings unverified until these checks; actual bootstrap/live registration remains U8. **Checkpoint:** auto — consumer/installation rehearsal in an isolated checkout. **Rollback:** no live source/config writes; keep old installation available.

### U14. Certify the candidate and retire obsolete implementation

- **Goal / requirements:** one tested Rust implementation replaces the Python v2 authority in a release candidate; all R1–R6. **Dependencies:** U9–U13.
- **Files:** Modify `tests/gate.sh`, README/setup/record-contract docs; remove candidate-package `libexec/attention.py` and obsolete v2-only fixture machinery only after behavior mapping; preserve test-only Python tty harnesses where useful.
- **Approach / patterns:** run chained checks in a disposable candidate checkout, record retained/changed/removed test obligations, then assemble activation patch and bootstrap U8 handoff. Do not delete the current uncommitted build from the working checkout during this planning iteration.
- **Tests:** *Happy:* clean candidate install → hook/query/render cycle. *Edge/error:* snapshot capacity, concurrent writes and crash points preserve the contract; diagnostics expose no secrets. *Integration:* Rust tests/fmt/clippy, Lua v1/new reader, Bun/Node/TypeScript, installed-WezTerm module/formatting and two-pane reattach all pass; pane stdin remains empty.
- **Verification / proven through:** fresh isolated install with no production Python; current-state/history consumers and rendering agree. Measure 100 sequential hooks and concurrent child bursts against the same-hardware Python baseline before retirement; report p95/max and state size, not an assumed Rust speedup.
- **Runtime evidence:** full Rust candidate unverified. **Checkpoint:** auto — full candidate gate; regression failures block certification. Activation is an explicit U8 hold, not a passing test. **Rollback:** retain old checkout/state and provide a reverse activation patch; never migrate bytes in place.

## Disconfirming evidence, impact and execution contract

| Risk or prior failure | Required proof / kill condition |
|---|---|
| Wrong-pane attribution or stale launch takeover | Two realms with equal IDs and socket-rebirth old-tty case; any cross-address write rejects the design |
| Lost child or revived notification | Concurrent child/lead writes and exact-ack race; any acknowledged child refresh or lost accepted child fails |
| Snapshot too large/slow | Boundary-size refusal remains explicit and non-destructive; compare same-machine hook latency and GUI poll cost; no unmeasured performance claim |
| Reconnect depends on agent activity | Attach with two idle/no-prompt agents, republish, observe next poll; any input byte or required new provider event fails |
| Recent history mistaken for a durable queue | Bootstrap/gap/reset/manual-stop/TTL cases never authorize completion; missing retained events or false completeness fails |
| Mixed migration silently loses bridge/Relay | Adapted current source consumers pass before activation; unmigrated bridge blocks cutover, not local Rust development |
| Shared-library loading assumed | Inspected WezTerm uses safe `Lua::new`; no Rust cdylib assumption enters implementation |

Data flow, failure propagation and compatibility effects are specified above: provider failures do not block the provider; explicit commands fail visibly; uncertain reads preserve scoped last-known evidence without authorizing actions. Source facts were inspected, not re-proven live in this planning turn. All Rust runtime claims remain unverified until their named unit exercise.

**Closed decisions:** shared lifecycle purpose; Rust; redesigned revision-3 per-pane snapshots; independent GUI overlays; 64-change catch-up with gaps; explicit shell claim; v1 inputs/API but no default v1 outputs; named scope cuts. **Builder autonomy:** internal factoring, pinned compatible crate versions and fixture organization within these contracts; record uncovered decisions without expanding features.

**Verify at contact:** installed provider payload/source support against sanitized fixture and local runtime; OS monotonic coordinate on each supported OS; standard file locking/tty metadata; WezTerm module/JSON behavior; Pi host dispatch; binary installation; exact bridge/Relay callers. Failed probes keep the affected integration unavailable and block its activation; they do not authorize title scraping, process scans, polling subprocesses or a daemon fallback.

**Authority / human inventory:** local candidate source and isolated tests may proceed when build is requested. No commits, pushes, credentials, provider spend, live hook/shell registration or live-linked plugin activation without separate instruction. U8 consent is the known external contribution: coordinated bootstrap bridge/Relay migration and restart/claim of affected agent launches. Test copied configurations until that consent exists. Do not silently reinterpret an established new-protocol failure as v1 fallback.

**Gate map:** U9 claim/identity/storage; U10 provider transitions/history; U11 Lua/render/overlays; U12 CLI/cursor/cleanup; U13 shell/Pi/consumer migration; U14 complete candidate gate. Existing v1 regressions stay green throughout. Old v2 physical-layout assertions may retire only with an explicit mapping to new safety tests or this plan's authorized cuts; unrelated failing tests are not waived.

**Stop conditions:** no supported runtime mechanism can satisfy a required identity/atomicity boundary; parity would require dropping a retained behavior; activation needs ungranted live authority. Continue all independent candidate work while one contact check is unresolved. Do not call the plan approved or the implementation complete merely because the document exists.

## Evidence anchors

- Current implementation: `libexec/attention.py:815` claim, `:1274` activity, `:1392` children, `:1575` parent clear; `plugin/init.lua:1407` record composition and `:2298` public tuple; `pi/index.ts:245` Node invocation.
- Current downstream source: bootstrap `configs/wezterm.lua:946,982` and `configs/wezterm/relay.lua:393`; `scripts/bridge:153`; `scripts/viterm-rebuild.py:242`. These are consumer evidence, not authorization to edit live-linked bootstrap files.
- Reference shape: local Herdr `src/api/wait.rs:540` identity/new-change waits; Luvus `src/agent.rs:1` exact session versus directory inference; Orca `src/shared/agent-status-types.ts` separates status, session and retained transitions. No application adoption is implied.
- New contract must preserve the motivating wrong-pane, reattach, stale-writer, child-stop, ack, review-owner and incomplete-read protections while explicitly replacing their old implementation mechanisms.
