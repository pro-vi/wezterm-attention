---
title: Attention lifecycle in one Rust executable
objective: Every consumer of a pane's agent state reads facts that were written by the right agent for the right pane, and recovers them after a reconnect or a missed callback without anyone typing a repair command.
type: feat
status: completed
date: 2026-09-06
origin: docs/reviews/2026-09-06-attention-v2-modules.md (revised module spec); replaces docs/plans/2026-09-05-001-feat-attention-rust-lifecycle-plan.md
review_source: ../reviews/2026-09-05-attention-v2-eight-choices.md
research: .research/synthesis-attention-v2-hooks-and-read-api-2026-09-06.md (untracked)
---

# Attention lifecycle in one Rust executable

## Background

`libexec/attention.py` (2,731 lines, uncommitted) is the v2 writer: provider hooks and the shell call `bin/attention`, it writes separate JSON records per pane under `~/.local/state/wezterm-attention/v2/`, and `plugin/init.lua` reads them on every poll. The module spec at `docs/reviews/2026-09-06-attention-v2-modules.md` is the contract this plan builds; the eight-choice review settled that separate binding and child files, safe child cleanup and ordinary launch commands stay. The earlier Rust plan proposed a single pane snapshot, a 64-event history and a manual Bash claim; all three were declined and are not carried here.

Facts verified on 2026-09-06 that shape the units:

- Nothing in v2 is committed. HEAD `4d3d0a5` is v1; the working tree carries the whole v2 build.
- The live GUI's plugin directory is a symlink into this checkout. Whatever `plugin/init.lua` and `bin/attention` contain is what runs. A Rust binary placed in this checkout must therefore stay inert until an explicit activation step.
- No provider hook calls `bin/attention` today. Bootstrap's own bun hooks write v1 flat markers directly. The only live caller of the v2 command is bootstrap's ViTerm launcher, which runs `hooks publish --realm` twice after GUI launch as a stopgap for the PATH failure.
- The live shell is zsh with the v1 `WEZTERM_PANE` precmd. Neither v2 shell file is sourced, so no launch claim exists live.
- The non-GUI suites pass: 79 Python cases, 35 protocol rows plus 10 eligibility rows, 102 Lua cases.
- Python's `_validate_field` accepts an unknown manifest field type; Lua rejects it; the fixture checker raises. One manifest edit can make the writer emit what the reader refuses.
- The Lua reader never reads `absence-probe.json`; the shared fixture covers all 14 record kinds; the reattach and tty-guard rehearsals are manual instruments the gate does not run.
- Rust toolchain on this machine: `cargo 1.94.1`, `rustc 1.94.1`. `File::try_lock` is stable since 1.89.
- The installed Pi extension copy under `~/.pi/agent/extensions/wezterm-attention/` is a stale 314-line file that writes `updated_at` instead of `updated_at_ms` and never publishes the pane id. That is a live drift bug outside this plan; it is recorded under Deferred work.

## Requirements

| ID | Required outcome | Consumer or reason |
|---|---|---|
| R1 | Bind full pane address, launch and actual provider session; reject stale or wrong-pane attribution | Lua title bar, Relay, `bindings --json` |
| R2 | Interpret supported Claude, Codex and Pi callbacks, child work and ending exactly as the provider fixtures state | Provider hooks, Pi adapter |
| R3 | Recover identity after GUI reconnect without agent input and without a human running a command | Mux-attached GUI; the two 2026-09-05 inbox failures |
| R4 | Re-anchor a record after a missed callback: prompt return, TTL, focus acknowledgement, explicit sweep | Escape interrupts and denied permissions emit no hook |
| R5 | Preserve shipped attention, acknowledgement, review, v1 inputs, the six-value Lua API and the v1 flat projection while bridge reads it | Bootstrap `wezterm.lua`, `bridge`, existing hook writers |
| R6 | Install and diagnose one Rust executable with bounded failures and explicit, preview-first cleanup | Shells, hook registration, operators |
| R7 | A provider `SessionStart` with no inherited launch id mints the launch itself and proves the pane through the controlling terminal | The 2026-09-06 ruling; the live zsh setup |

## Naming Ledger

| Role / meaning | Existing repo term | Chosen name | Owner / placement | Status | Second consumer / reason | GR6 sibling disposition |
|---|---|---|---|---|---|---|
| One server pane in one socket lifetime | `pane_address` / `validate_pane_address` | `PaneAddress` | `src/identity.rs` | reuse | — | no asymmetry |
| One top-level agent process | `launch_id` | `LaunchId` | `src/identity.rs` | reuse | — | no asymmetry |
| Actual provider session in a launch | binding | `Binding` | `src/protocol.rs` | reuse | — | no asymmetry |
| Manifest field type as a closed set | none (string chain in Python) | `FieldType` | `src/protocol.rs` | new | validator and manifest loader; the fail-open bug is the reason | — |
| Normalized provider callback | `ProviderEvent.action` (free string) | `ProviderAction` | `src/providers.rs` | rename | `lifecycle.rs` and the fixture vocabulary test | fixture `expected` strings map by name |
| Lock, reread, decide, apply, project in one place | none (14 lock sites) | `commit` | `src/records.rs` | new | every mutating command; `main.rs` never locks | — |
| Which launch a hook belongs to | `_claim_for_env` | `resolve_launch` | `src/lifecycle.rs` | rename | provider hooks and `mark` | — |
| Claim minted by a provider `SessionStart` | none | self-claim | `src/lifecycle.rs`, record kind stays `claim` | new | spec ruling; second consumer is `hooks claim` sharing the same write | — |
| Shell prompt returned after the current launch | none | prompt return | `hooks publish` → `activity_clear` and `subagent_clear` records | new | no new record kind; reuses two existing kinds | — |
| Locate the `wezterm` binary | `shutil.which("wezterm")` | `wezterm_executable` | `src/wezterm.rs` | rename | `hooks publish` and `bindings` presence probe | — |
| GUI assessment of one pane | `read_attention_view` result | `AttentionView` | `plugin/init.lua` | reuse | — | no asymmetry |
| Cached full-pane view for in-GUI consumers | none | `get_attention_view(pane)` | `plugin/init.lua` public API | new | Relay's `detect_agent` and the title formatter | `get_attention` keeps its six values |
| Derived v1 flat marker and `.agents` sidecar | `_write_v1_activity`, `_reconcile_v1_agents_locked` | `LegacyProjection` | `src/compat.rs` | rename | bridge and the v1 Lua path | one owner for units and field names |

## Architecture Decision

**Approach:** One short-lived Rust executable behind the existing `bin/attention` shim, writing the unchanged v2 record layout, validated by the embedded `protocol/v2.json` interpreted at runtime through a closed `FieldType` enum. Pure decisions live in `lifecycle.rs`; every mutation goes through one `commit` helper in `records.rs`. The Lua plugin keeps its reader and gains a retried reconnect publish, a public full-pane accessor and no other structural change.

**Rationale:** Consistency decided it. The record layout, the manifest, the shared fixtures, the shim path and the Lua reader are all in place and tested; the Rust binary replaces one process and nothing a consumer touches. The rejected alternative is the single pane snapshot from the earlier plan: it would have changed the layout and the wire revision, dropped child cleanup and forced a coordinated reader rewrite, for a commit-atomicity gain the existing launch lock already provides. Embedding the manifest rather than reading it from disk at hook time was chosen because a hook runs under whatever checkout the shell exported and must not depend on that file being present or unedited; `doctor` compares the on-disk manifest digest to the embedded one.

**Trade-offs:** Multi-file transitions stay non-transactional; binding-before-pointer, stop-then-clear and floor-before-delete remain explicit sequences with retry repair. Rust interprets a JSON manifest at runtime, which is slower than compiled constants by an amount that must be measured, not assumed. The Lua module split proposed by the spec is U16 and lands last among the Lua units, because the live GUI loads this checkout.

**Approval criteria:** Approving this plan agrees that the record layout and wire revision 2 are unchanged; that `SessionStart` may mint a launch when the shell did not; that a shell prompt return clears the current activity and children but does not end the binding; and that the Rust binary is inert in this checkout until U14's activation step, which needs separate consent because the live GUI loads this checkout.

## Existing-thing check

| New thing | What it beat | Why |
|---|---|---|
| `serde`, `serde_json` | hand-written JSON codec (Python and Lua both hand-roll parts) | std has no JSON; typed round-trip is the equivalence mechanism |
| `clap` | manual argv parsing | layered help and errors matching the Python `argparse` surface; not installed yet, no lockfile exists |
| `uuid` | `python3 -c 'import uuid'` subprocess in both shell files | removes a Python subprocess from every launch |
| `sha2` | hand-rolled SHA-256 (Lua has one, pinned by a known-answer vector) | std has none; digests are identity, not presentation |
| `libc` | `std::time::Instant` | `Instant` exposes no raw value; the coordinate must be `CLOCK_MONOTONIC_RAW` to match records Python wrote |
| `std::fs::File::try_lock`, `OpenOptions::create_new`, `IsTerminal` | `fs2`, `atty` crates | stable in std on the inspected 1.94.1 toolchain |
| `scripts/install-cli.sh` | Cargo alone | a WezTerm or Pi checkout does not run Cargo; the shim must never build during a hook |
| `src/compat.rs` | dropping the v1 projection | bridge reads only the flat files today and has no v2 reader |

## Program obligations

- O1: The manifest field-type set is a closed Rust enum with an exhaustive match; a manifest naming an unknown type fails at load, never per field.
- O2: Every mutating command runs through one `commit` helper that takes the scope lock, rereads, calls a pure decision, applies ordered replacements and writes the legacy projection. `main.rs` holds no lock. Locks nest in one order only: launch lock outside, pane claim lock inside, and the projection write re-takes the claim lock and re-reads the claim before writing (as `attention.py:956` and `:1342` do). `attention.py:2441` nests them the other way; the port does not copy that. A self-claim takes the claim lock, releases it, then takes the launch lock; it never holds both.
- O3: Provider callbacks normalize into a closed `ProviderAction` enum; a test asserts the provider fixtures' `expected` vocabulary equals the enum's variant names.
- O4: The published wire envelope is a typed value serialized from a struct and re-parsed through the manifest's `wire` spec before it is written to the tty; the OSC byte bound is stated once.
- O5: `resolve_launch` accepts, in order: an inherited `WEZTERM_ATTENTION_LAUNCH_ID` matching the pane's claim; else the pane's claim whose tty equals the caller's controlling terminal; else, for `SessionStart` only, a self-claim. Anything else is `claim_stale`.
- O6: `acknowledgement` records are written only by Lua; Rust validates and prunes them and a test feeds a Lua-written record through the Rust validator.
- O7: Every diagnostic code Rust emits is a member of the manifest's `diagnostic_codes`; a test scrapes and asserts it.
- O8: The v1 projection fixes its units and names: `updated_at` in seconds and `updated_at_ms` in milliseconds, both written (bootstrap's `bridge` reads only `updated_at_ms` and today gets 0 from the v2 projection; the Lua reader accepts either), `.agents.last_ms` in milliseconds, `.agents.type` carrying the provider's `agent_type` when present, else the source. A fixed-clock test pins all three.
- O9: Ordering values are 20-digit decimal strings from `CLOCK_MONOTONIC_RAW`; no `Instant` value is ever persisted.

## High-Level Technical Design

Directional guidance for review, not implementation specification.

```text
hook / shell / Pi / mark
  -> main.rs      parse argv, capture observation, read bounded stdin
  -> providers.rs normalize -> ProviderAction
  -> records.rs   commit(scope, |records| lifecycle::decide(...))
                    lock -> reread -> decide -> apply ordered files -> compat.rs projection
  -> wezterm.rs   tty publication (claim, self-claim, publish)

GUI poll (Lua)
  resolve_pane_read -> read_attention_view -> cache -> format
  unpublished pane  -> background `hooks publish --realm` with executable dir on PATH,
                       retried with backoff until no pane in the domain is unpublished

Launch resolution for a hook with no env id
  SessionStart: controlling tty -> pane address from WEZTERM_PANE + socket -> self-claim
  other events: pane claim whose tty_path == controlling tty -> that launch, else claim_stale
```

Record layout and wire revision are unchanged from `docs/record-contract.md`. Two manifest edits are data-only: `publication_id` leaves the `activity` optional list, and `digests` gains the recipes for `realm_id`, `incarnation_id`, `tty_fingerprint` and `binding_id`.

## Implementation Units

U-IDs continue from the plan this replaces. U9, U10, U12, U13 and U14 keep their goals with changed approaches; U11 (snapshot reader) is retired and its number stays unused; U15 and U16 are new.

### U9. Claim, publish and query one pane through the Rust binary

- **Goal:** The Rust binary claims a disposable pane, publishes its identity, and answers `bindings --json`, with the manifest, identity, records and tty ports proven.
- **Requirements:** R1, R3, R6
- **Dependencies:** None
- **Files:**
  - Create: `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/main.rs`, `src/protocol.rs`, `src/identity.rs`, `src/records.rs`, `src/wezterm.rs`, `src/query.rs`, `scripts/install-cli.sh`, `tests/rust/claim_publish_spec.rs`
  - Modify: `bin/attention` (select `libexec/attention-rs` only when `WEZTERM_ATTENTION_IMPL=rust`; otherwise unchanged Python path), `.gitignore` (`target/`, `libexec/attention-rs`), `protocol/v2.json` (digest recipes)
- **Approach:** `include_str!` the manifest; parse it into typed limits, enums and record specs with a closed `FieldType` (O1). `records.rs` owns paths, private permissions, atomic replace, the three lock scopes and `commit` (O2). `wezterm.rs` lists panes with a typed row, writes the tty after fingerprint and opened-descriptor checks, and finds `wezterm` by PATH, then `WEZTERM_EXECUTABLE`, then the macOS bundle path. `query.rs` reads bindings with the four axes.
- **Patterns to follow:** `libexec/attention.py:613-645` opened-descriptor validation; `:815-860` claim ordering; `:1778-1865` bindings; `tests/fixtures/v2/check.py` as the independent oracle.
- **Test scenarios:**
  - *Happy path:* disposable pty → `hooks claim` → claim record, OSC on the tty, `bindings --json` names the address with no binding.
  - *Edge:* equal pane numbers in two sockets stay distinct; an old tty after socket rebirth cannot claim the reused pane; a delayed claimant with an older observation loses; duplicate claim republishes without rewrite (port `test_claim_is_private_durable_and_published`, `test_delayed_older_claim_cannot_replace_newer_launch`, `test_opened_tty_descriptor_is_revalidated`).
  - *Error:* manifest with an unknown field type refuses to start; lock contention is bounded and diagnosed; `wezterm` absent from PATH but present in the bundle directory still publishes; malformed enumeration row makes the probe unavailable, not an empty list.
  - *Integration:* every row of `tests/fixtures/v2/protocol-cases.json` round-trips through the Rust types with the same verdict as `check.py`; the four digest recipes match known-answer vectors computed from the Python implementation before it is removed; the embedded manifest equals `protocol/v2.json` byte for byte.
- **Verification:** Rust identity, record and tty behavior match the Python tests named above; the shim keeps calling Python unless the environment selects Rust.
- **Proven through:** injected clock, pane lister and tty writer ports (constructor-injected doubles), plus one real disposable pty via `script` or Python's `pty` in the test.
- **Rollback:** delete `libexec/attention-rs`; the shim's default path never changed.
- **Runtime evidence:** unverified — `cargo test` and `WEZTERM_ATTENTION_IMPL=rust bin/attention hooks claim` on a disposable pty inside an isolated `WEZTERM_ATTENTION_DIR`.
- **Checkpoint:** auto — `cargo test` green and the disposable-pty claim shows an OSC write with zero bytes on the pane's stdin (`tests/tty_input_guard.py`).

### U10. Drive provider lifecycles, launch resolution and prompt return

- **Goal:** Claude, Codex and Pi fixture processes produce the same records the Python writer produced, a `SessionStart` without an inherited id self-claims, and a prompt return clears activity without ending the binding.
- **Requirements:** R1, R2, R4, R5, R7
- **Dependencies:** U9
- **Files:**
  - Create: `src/providers.rs`, `src/lifecycle.rs`, `src/compat.rs`, `tests/rust/lifecycle_spec.rs`
  - Modify: `src/main.rs` (`hooks event`, `mark`, prompt-return on `hooks publish`), `src/records.rs`, `protocol/v2.json` (drop `publication_id`), `tests/fixtures/providers/*.json` (add `self_claim_binding` and `no_launch_non_start_is_stale` cases)
- **Approach:** Port the behavior of `attention.py:340-463` into `ProviderAction` (O3). `lifecycle.rs` is pure: transition functions take validated event, current records and the observation and return an ordered list of file replacements or a rejection. `resolve_launch` per O5; the self-claim reuses U9's claim write with the tty taken from `/dev/tty`. `hooks publish` from the current pane writes an `activity_clear` at the current binding, fenced by the observation, so a strictly newer provider event reactivates; children are not cleared, because a hidden child would reappear only on its own next tool call, so clearing them buys nothing and undercounts until then; the binding stays active. Known false positives: Ctrl-Z, an agent backgrounded with `&`, and a nested shell; each self-corrects on the agent's next hook. `compat.rs` writes the flat marker and `.agents` sidecar with fixed units (O8) and repairs them on duplicate events without refreshing timestamps.
- **Patterns to follow:** `attention.py:1139-1235` binding replacement; `:1274-1320` activity; `:1392-1450` children; `:1569-1610` Codex parent stop; `:928-960` and `:1336-1389` projection; provider fixture format in `tests/fixtures/providers/claude.json`.
- **Test scenarios:**
  - *Happy path:* each provider fixture case yields its `expected` action and record; a `SessionStart` with no env id and a matching controlling tty writes a claim then a binding; a prompt return after `thinking` leaves the view empty and a later `PreToolUse` reappears.
  - *Edge:* stop then active continuation; compact and reload confirmations; delayed older writers lose; duplicate retries repair the projection without new event ids; child stop fences a delayed child tool call; Ctrl-Z prompt return followed by the same session's next hook reactivates (port `test_older_inflight_child_tool_call_cannot_reactivate_stopped_presence`, `test_same_session_resume_reopens_older_end_and_accepts_a_new_end`, `test_manual_activity_after_clear_mints_a_new_visible_event`).
  - *Error:* a non-`SessionStart` hook with no env id and no tty-matching claim is `claim_stale`; a `SessionStart` whose controlling tty is not a character device owned by the user is rejected; a nested agent that inherits the pane environment and starts with a different session follows Python's rule, now stated: a `startup` start cannot replace an active binding (`binding_conflict`, `attention.py:1177-1190`); a `resume` or `clear` start (Pi: `new`, `resume`, `fork`) replaces it, because those are deliberate user actions in the pane; the self-claim never fires while a tty-matching claim exists (O5), so it cannot mint a second launch for a claimed pane (`nested_startup_cannot_replace_active_binding`, `nested_resume_replaces_active_binding`, `claimed_pane_never_self_claims`).
  - *Integration:* 100 real hook subprocesses through the shim with `WEZTERM_ATTENTION_IMPL=rust`; concurrent child bursts with a parent stop; `pi/index.ts` dispatch through Node produces Rust-written bindings.
- **Verification:** the fixture vocabulary equals the enum; every Python lifecycle test named in `tests/attention_cli_test.py` has a Rust counterpart or a recorded cut; the projection files are byte-identical to Python's for the same inputs except the unit fix, which is pinned by its own test.
- **Proven through:** the injected ports from U9 and real subprocesses in the integration cases.
- **Rollback:** the shim default is still Python; delete the binary.
- **Runtime evidence:** unverified — the 100-process run and the Node dispatch test; the `/dev/tty` reachability from a real provider hook is a Verify-at-contact item with a stand-in (see contract).
- **Checkpoint:** auto — Rust lifecycle cases and the real-process integration run green; the self-claim path stays behind the fixture stand-in until the contact check below reports.

### U12. Expose facts, diagnose and clean up explicitly

- **Goal:** `bindings --json`, `doctor` and `sweep` reach parity with the Python commands, including two-observation absence, child compaction with a floor, per-realm retention and preview-first apply.
- **Requirements:** R4, R6
- **Dependencies:** U9, U10
- **Files:**
  - Create: `src/maintenance.rs`, `tests/rust/maintenance_spec.rs`
  - Modify: `src/query.rs`, `src/main.rs`, `src/wezterm.rs` (scoped process probe)
- **Approach:** Port `attention.py:1983-2480`. Preview writes nothing; apply reacquires locks and rechecks the target immediately before mutation. Absence needs two pane-list negatives under different operation ids at least 60 monotonic seconds apart plus an identity-scoped process negative; probe failure is unavailable. Query and maintenance reuse `lifecycle.rs` predicates for current, ended and expiry. `doctor` adds the embedded-versus-on-disk manifest digest check and reports emitted diagnostic codes against the manifest (O7).
- **Patterns to follow:** `attention.py:2086-2230` compaction plan and apply; `:2300-2480` sweep; `:1983-2031` doctor.
- **Test scenarios:**
  - *Happy path:* preview lists the same targets apply would touch; two absences end exactly one binding; compaction advances the floor before deleting.
  - *Edge:* an eligible active child blocks the whole equal-timestamp group; a live claim clears the first absence probe; a reactivated child is revalidated at apply; retention caps are per realm (port `test_sweep_preview_writes_nothing_and_two_monotonic_absences_end_exactly_one_binding`, `test_equal_order_active_child_blocks_floor_for_whole_group`, `test_compaction_apply_revalidates_a_reactivated_child`, `test_binding_history_cap_is_calculated_per_realm`).
  - *Error:* negative wall age reports `clock_skew` and never prunes; future schema and unknown files survive; process probe unavailable never counts as absence.
  - *Integration:* sweep apply under a racing writer preserves current and unknown state.
- **Verification:** every sweep and doctor test in the Python suite has a Rust counterpart; JSON envelopes carry `schema`, `command`, `status`, `complete`, `result`, `diagnostics` and stay bounded.
- **Proven through:** injected ports; disposable state roots.
- **Runtime evidence:** unverified — `cargo test` plus one `sweep --apply --operation-id` run against a disposable state root.
- **Checkpoint:** auto — maintenance cases green; preview and apply agree on a seeded state root.

### U15. Reconnect publish that completes, and a full-pane Lua accessor

- **Goal:** A mux-attached GUI recovers every pane's identity without a human running `hooks publish`, and in-GUI consumers can read a pane's provider when nothing is drawn.
- **Requirements:** R3, R5
- **Dependencies:** U9 (publish with executable discovery)
- **Files:**
  - Modify: `plugin/init.lua` (`request_republish_once` → retried publish with backoff; child PATH carries `wezterm.executable_dir`; child failure surfaces through `report_error_once`; views recovered after reconnect carry `reader_confidence = "unconfirmed"` until a newer event; `subagent_live_ms` derived from `protocol.limits.subagent_ttl_ms`; new `M.get_attention_view(pane)` returning the cached view with `provider`, `binding_id`, `binding_phase`, `type`, `event_id`, `subagents`, `review`, `reader_confidence` and no I/O), `tests/auto_clear_spec.lua`, `tests/wezterm_reattach_smoke.lua`, `README.md`, `docs/mux-setup.md`
- **Approach:** Keep one spawn point. Replace the per-socket latch with a per-socket schedule: first publish when the domain's pane count has been equal across two polls, then retry at 2 s, 5 s, 10 s, then every 30 s while a poll still sees an unpublished pane in that domain; stop when none remain. Repeat publication is idempotent because the wire bytes are identical. The accessor reads the cache only.
- **Patterns to follow:** `plugin/init.lua:2225-2246` current spawn; `:2250-2281` timer token pattern for cancellation; `:1983-2000` change comparison; inbox notes of 2026-09-05 for the measured failure shape.
- **Test scenarios:**
  - *Happy path:* three polls with an unpublished pane produce publishes at the scheduled offsets and stop after the pane resolves.
  - *Edge:* a domain not declared in `config.unix_domains` never spawns; a second window does not double-schedule; a failed spawn logs once and keeps the schedule; a byte-identical republish does not redraw.
  - *Error:* `wezterm.executable_dir` missing → child still spawns with the unchanged PATH and the failure is logged, not silent.
  - *Integration:* `tests/wezterm_reattach_smoke.lua` with `WEZTERM_ATTENTION_ENABLE_PUBLISH=1` on a disposable domain shows every pane published after attach, and `tests/tty_input_guard.py` records zero stdin bytes.
- **Verification:** `get_attention_view(pane)` returns `provider` for a bound pane whose stop is acknowledged; the six-value `get_attention` is unchanged; no per-poll subprocess other than the scheduled publish.
- **Proven through:** the stubbed `wezterm` module in `tests/auto_clear_spec.lua` (recorded `background_child_process` calls and a fake clock), plus the manual reattach rehearsal.
- **Runtime evidence:** unverified — the reattach rehearsal against a disposable mux with the Rust publish selected; the live GUI is not used.
- **Checkpoint:** gate — the reattach rehearsal's JSON shows all panes `published` and the guard shows zero input bytes → continue; any input byte → hold U14 activation while U12 and U13 continue; unknown (rehearsal not run): U13 and U14 candidate work proceed, activation stays held.

### U16. Split `plugin/init.lua` into the spec's eight modules

- **Goal:** The Lua plugin's responsibilities live in the eight files the spec names, with `init.lua` as the public API and composition point, and every existing Lua case still green.
- **Requirements:** R5 (the public API and shipped behavior survive the move); accepted as a build-it answer to the cut candidate on 2026-09-06
- **Dependencies:** U15
- **Files:**
  - Create: `plugin/protocol.lua`, `plugin/reader.lua`, `plugin/runtime.lua`, `plugin/overlays.lua`, `plugin/legacy.lua`, `plugin/titles.lua`, `plugin/format.lua`
  - Modify: `plugin/init.lua` (requires the seven modules, keeps `M.*` and `M._internal`), `tests/auto_clear_spec.lua` (module loading through the same loader path), `tests/wezterm_protocol_smoke.lua`, `tests/wezterm_reattach_smoke.lua`, `README.md`
- **Approach:** Move code without changing behavior, one module per checkpoint (a saved patch under the scratchpad; this plan grants no commit authority) in this order: `protocol.lua` (validation, digests, time arithmetic), `legacy.lua` (v1 marker and sidecar reading), `titles.lua`, `format.lua`, `overlays.lua`, `reader.lua`, `runtime.lua`. Each module receives its dependencies as arguments or requires `protocol.lua` only; `runtime.lua` owns the cache and `reader.lua` receives previous records and returns a view. `format.lua` takes values only and performs no I/O, clock sampling or process work. WezTerm's plugin loader resolves `require` relative to the plugin directory; the loader path derivation at `plugin/init.lua:69-81` is reused, not duplicated.
- **Patterns to follow:** the spec's Lua module table for ownership and boundaries; `plugin/init.lua:2918-2945` `M._internal` export list for what tests reach.
- **Test scenarios:**
  - *Happy path:* all 102 cases in `tests/auto_clear_spec.lua` pass unchanged after each module move; the installed-WezTerm smoke loads the split plugin.
  - *Edge:* `format.lua` has no `io`, `os.time`, `os.execute` or `wezterm.time` reference (a grep test); `reader.lua` keeps no module-level cache table.
  - *Error:* a missing module file yields one `report_error_once` line naming the module, and the v1 path keeps rendering.
  - *Integration:* the reattach rehearsal on a disposable domain still publishes and renders.
- **Verification:** `M.pane_marker_id`, `M.get_attention`, `M.get_attention_view`, `M.poll`, `M.remove_marker`, `M.wrap_title_formatter`, `M.apply_to_config` and `M.doctor` keep their signatures; no new public function.
- **Proven through:** the existing stubbed `wezterm` module in the Lua spec; the installed-WezTerm smoke for the real loader.
- **Rollback:** apply the reverse of the last checkpoint patch; each move is one patch.
- **Runtime evidence:** unverified — the smoke and the reattach rehearsal after the last move, run in the separate copy named under Authority boundaries; the live GUI loads whatever is in this checkout on its next config reload, so the split is moved into this checkout only as one bundle after the rehearsal, and the move is announced before it happens.
- **Checkpoint:** auto — Lua cases and the installed-WezTerm smoke green after every module move.

### U13. Shell, Pi and consumer migration without production Python

- **Goal:** Both shell integrations and the Pi adapter drive the Rust binary, the Pi fallback rule matches the spec, and adapted copies of bootstrap's consumers prove the handoff.
- **Requirements:** R1, R3, R5, R6, R7
- **Dependencies:** U10, U12, U15
- **Files:**
  - Modify: `shell/wezterm-attention.bash`, `shell/wezterm-attention.zsh` (launch id comes from `hooks claim` output; no `python3` subprocess; Bash keeps automatic claiming, zsh keeps the explicit function, both republish at prompt), `pi/index.ts` (one fallback rule: v1 fallback only when `WEZTERM_ATTENTION_ROOT` is unset; a set root that names no executable checkout is a configuration error logged once with no v1 marker; a writer that was invoked and failed is reported through Pi's logging and never falls back), `examples/hook.sh`, `examples/hook.ts`, `tests/pi_extension.test.ts`, `tests/pi_node_runtime.mjs`
  - Create: `tests/fixtures/consumer-migration/` (adapted copies of bootstrap's `bridge` marker reader, `detect_agent`, and one Claude and one Codex hook script rewritten to exec `bin/attention hooks event`)
- **Approach:** `hooks claim` prints the launch id that is current after the call: the caller's own on `applied` or `confirmed`, the existing winner's on `ignored` (the envelope with `--json` carries the disposition beside it, as `attention.py:815-860` already publishes the selected claim). The parent shell exports the printed id only after exit 0. Claim failure clears any inherited id. The Pi adapter's boolean stays a two-value projection of the disposition, but the false branch now distinguishes missing root from failed writer. Consumer copies are exercised, not installed.
- **Patterns to follow:** `shell/wezterm-attention.bash:52-58` claim site; `pi/index.ts:245-293` writer and fallback; `tests/pi_extension.test.ts:198-241` exact argv assertions.
- **Test scenarios:**
  - *Happy path:* sourced Bash claims one supported command and exports the id printed by the binary; zsh explicit claim rotates each launch; Pi dispatch produces Rust bindings; the adapted bridge copy reads `updated_at_ms` from the projection, which O8 writes beside `updated_at`.
  - *Edge:* claim commits but tty publication fails → export kept, publication reported pending; a prior DEBUG trap is preserved (port `test_bash_debug_hook_handles_assignments_and_preserves_quoted_trap`).
  - *Error:* no root set and no binary → v1 fallback; root set to a directory without the binary → no v1 marker, one log line; exit 3 from an invoked writer → no v1 marker, one log line.
  - *Integration:* `bun test`, `node tests/pi_node_runtime.mjs` and `bun run typecheck` stay green with the unchanged argv contract.
- **Verification:** no production path invokes `python3`; the argv strings `hooks event pi <event>` and `hooks publish --realm <socket> --quiet` are unchanged.
- **Proven through:** sourced shell subprocesses in tests; Node and Bun test doubles for `bin/attention`.
- **Runtime evidence:** unverified — the shell tests and Pi tests under the Rust shim selection.
- **Checkpoint:** auto — shell, Pi and consumer-copy tests green.

### U14. Certify, activate by consent, retire the Python writer

- **Goal:** One tested Rust implementation is the default behind `bin/attention`, the gate runs it, and the Python writer and its test file are removed after their behavior is mapped.
- **Requirements:** R1–R7
- **Dependencies:** U9, U10, U12, U13, U15, U16
- **Files:**
  - Modify: `bin/attention` (default to `libexec/attention-rs`; fixed exit-3 failure naming `scripts/install-cli.sh` when absent; no Python fallback), `tests/gate.sh` (drop `sh -n bin/attention` only if the shim stops being a shell script; drop the two Python `py_compile` entries and the unittest step; add `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` before the Bun steps; keep `check.py` and `provider_contact_hook.py`), `README.md`, `docs/mux-setup.md`, `docs/record-contract.md` (acknowledgement is Lua-owned; prompt return; projection units)
  - Delete: `libexec/attention.py`, `tests/attention_cli_test.py`
- **Approach:** Run the full gate in a disposable copy of the checkout first. Record a retained/changed/removed table for every Python test. Measure 100 sequential hooks and a concurrent child burst against the Python baseline on this machine; report p95 and max, not a speedup claim. Before the flip, run `doctor` through the binary `bin/attention` will select and compare its embedded manifest digest with `protocol/v2.json` in this checkout; a mismatch holds the flip. Then flip the shim default. Because the live GUI loads this checkout, the flip is the activation and needs the consent item below.
- **Patterns to follow:** `tests/gate.sh` ordering; `tests/run_wezterm_smoke.sh` for the installed-WezTerm contact.
- **Test scenarios:**
  - *Happy path:* fresh isolated install → claim → hook cycle → render through the installed WezTerm smoke.
  - *Edge:* the shim with no binary exits 3 with the fixed message; `git diff --check` clean; every `md map` passes.
  - *Error:* a Python test with no Rust counterpart and no recorded cut blocks certification.
  - *Integration:* full gate green in the disposable copy; the reattach rehearsal repeated once with the default shim.
- **Verification:** the gate passes with no Python writer present; the mapping table has no unmapped row.
- **Proven through:** the gate itself and the measurement script under `tests/rust/`.
- **Rollback:** restore `bin/attention` and `libexec/attention.py` from the pre-activation copy kept beside the disposable-copy gate run (the v2 Python writer is uncommitted, so git history cannot supply it); state files are unchanged by activation.
- **Runtime evidence:** unverified — the disposable-copy gate run.
- **Checkpoint:** gate — disposable-copy gate green and measurement recorded → the shim-default flip waits on the activation consent; without it, every other unit is complete and the flip is the only held action.

## State-Action Contracts

### Claim × existing claim (U9, U10)

| Action × state | Observation | Durable result | Effects | Race rule | Test |
|---|---|---|---|---|---|
| Shell claim, no claim | applied | new claim, manifests written | OSC to validated tty | later observation wins under `.claim.lock` | `claim_exact_tty` |
| Shell claim, same launch and tty | confirmed | unchanged | OSC republished | none | `duplicate_claim_republishes_without_rewrite` |
| Shell claim, older observation than existing | ignored | unchanged | existing identity republished | reread under lock decides | `delayed_older_claim_loses` |
| Self-claim on `SessionStart`, no env id, tty matches pane | applied | claim with minted launch, then binding in the same command | OSC through `/dev/tty` | same lock and ordering as shell claim | `session_start_self_claims` |
| Self-claim, env id present but mismatching claim | `claim_stale` | unchanged | one diagnostic | none | `stale_env_id_cannot_self_claim` |
| Any claim, socket incarnation changed between read and write | `incarnation_changed` | unchanged | none | revalidated after lock | `old_tty_cannot_claim_reused_pane` |

Invariant: a `claim.json` exists iff the pane directory has manifests above it; `current-binding.json` names a binding iff that binding's `binding.json` exists.

### Hook × launch resolution (U10)

| Action × state | Observation | Durable result | Effects | Race rule | Test |
|---|---|---|---|---|---|
| Env id matches claim | resolved | per event transition | projection | launch lock | existing fixture cases |
| No env id, claim tty equals controlling tty | resolved to that launch | per event transition | projection | launch lock | `tty_resolves_launch_without_env` |
| No env id, no tty match, event is `SessionStart` | self-claim then binding | see claim table | OSC | claim lock then launch lock | `session_start_self_claims` |
| No env id, no tty match, other event | ignored, `claim_stale` | unchanged | one diagnostic, exit 0 | none | `no_launch_non_start_is_stale` |
| Controlling tty unavailable (`/dev/tty` open fails) | ignored, `unsafe_tty` | unchanged | one diagnostic | none | `no_controlling_tty_is_inert` |

### Prompt return × activity (U10)

| Action × state | Observation | Durable result | Effects | Race rule | Test |
|---|---|---|---|---|---|
| `hooks publish` from pane, bound, activity visible | applied | `activity_clear` at current binding with this observation; child records untouched | projection cleared, `.agents` unchanged; identity republished | a provider event with a strictly newer observation reappears | `prompt_return_clears_until_newer_event` |
| `hooks publish`, bound, no activity | applied | clear written once; duplicate observation skipped | identity republished | none | `prompt_return_is_idempotent` |
| `hooks publish`, unbound launch | applied | no clear (launch-level activity is manual only and stays) | identity republished | none | `prompt_return_leaves_unbound_activity` |
| `hooks publish --realm` (GUI) | applied | no clear records ever | identity republished per pane | none | `realm_publish_never_clears` |
| Agent suspended with Ctrl-Z, then resumed | cleared, then reactivated | next hook's observation is newer | view returns | ordering only | `suspend_resume_reactivates` |
| Agent backgrounded with `&`, or a nested shell returns a prompt | cleared, then reactivated on the agent's next hook | same as Ctrl-Z | view returns | ordering only | `background_agent_reactivates` |

Invariant: prompt return never writes `end.json`; `binding_phase` is `ended` iff an `end.json` with observation at or after the binding's exists.

### Reconnect publish schedule × poll (U15)

| Action × state | Observation | Durable result | Effects | Race rule | Test |
|---|---|---|---|---|---|
| Poll sees unpublished pane, no schedule | schedule armed | none | none yet | one schedule per socket | `unpublished_pane_arms_schedule` |
| Pane count stable across two polls | first spawn | none | child process | spawn once per due time | `first_publish_after_stable_count` |
| Later poll, still unpublished, backoff elapsed | spawn again | none | child process | timer token cancels stale timers | `retries_follow_backoff` |
| Poll sees no unpublished pane in domain | schedule cleared | none | none | none | `schedule_clears_when_resolved` |
| Spawn fails | logged once, schedule kept | none | one log line | none | `spawn_failure_is_visible` |

Omitted-state challenge: a pane that publishes then loses its user var after a domain detach re-arms the schedule on the next unpublished sighting; a GUI with two windows on the same domain shares one schedule because it is keyed by socket.

## Scope Boundaries

- No event log, cursor or `changes` command.
- No pane snapshot file; the record layout and wire revision are unchanged.
- No daemon, no native Lua module, no schema generation framework.
- No edits to bootstrap, `~/.claude/settings.json`, `~/.codex/hooks.json` or the installed Pi extension; consumer migration is proven on copies.
- No process scan on a schedule; the sweep stays explicit.
- Windows lifecycle publishing and additional providers are outside this plan.

### Cut candidates put to the human

- **Lua module split** (the spec's eight `plugin/` files): put to the user on 2026-09-06; answer: build it. It is U16. Key Technical Decision, do not re-ask on a replan.
- **`paths` section in the manifest**: not added. `records.rs` owns the path grammar and the shared `state_case` fixture is run through Rust, Lua and `check.py`, which is the equivalence the section would have provided.

### Deferred to Follow-Up Work

- Installed Pi extension copy is stale and writes `updated_at`: re-copy after U13 lands; recorded in this plan's Background and to be raised with bootstrap.
- Whether `notify` clears on focus or only on a newer provider event: open decision in the spec; shipped behavior is preserved here.
- Whether Lua keeps recomputing SHA-256 per child record per poll: measure in U14's report; no change here.

## System-Wide Impact

- **Interaction graph:** shell precmd → `hooks publish` (now also prompt return); provider hooks → `hooks event`; Pi queue → `hooks event pi`; GUI poll → scheduled `hooks publish --realm`; ViTerm launcher → `hooks publish --realm`; bridge → flat projection; Relay → `get_attention_view`.
- **Error propagation:** hook failures exit 0 with one stderr diagnostic unless `--strict`; explicit commands fail visibly with exit 2 or 3; the Lua reader turns any I/O failure into `probe_unavailable` and keeps the last good scoped view.
- **State lifecycle risks:** multi-file sequences are not transactions; each has a retry repair. Prompt return adds one watermark write per prompt in a bound pane; duplicate observations are skipped.
- **API surface parity:** `hooks claim|publish|event`, `mark`, `bindings`, `doctor`, `sweep` keep their flags and exit codes; the six-value `get_attention` is unchanged; `get_attention_view` is additive.
- **Integration coverage:** real hook subprocesses, the installed-WezTerm smoke, the disposable-mux reattach rehearsal and consumer copies.
- **Unchanged invariants:** record layout, wire revision 2, record schema 2, `writer_version` read from the manifest, acknowledgement written only by Lua, review clear scope asymmetry between Rust (own owner) and Lua (all owners in a tab), `hooks event` exit 0 on ignored.

## Build Execution Contract

- **Closed decisions:** Rust; unchanged record layout and wire revision; embedded manifest interpreted at runtime with a closed `FieldType`; one `commit` helper; `resolve_launch` order per O5; prompt return clears lead activity only, never children, and never ends a binding; shim selects Rust only by environment until U14; v1 projection kept with fixed units; the Lua module split is U16 and lands after U15's rehearsal.
- **Builder autonomy:** internal factoring within the named modules; crate versions pinned in `Cargo.lock`; fixture file organization under `tests/rust/`; backoff constants in U15 within the 2 s to 30 s shape; record and continue.
- **Verify at contact:**
  - A provider hook subprocess can open `/dev/tty` and its `ttyname` equals the pane's `tty_name` from `wezterm cli list` → stand-in first: a Node child with piped stdio inside a disposable WezTerm pane runs the Rust binary and compares; then one `claude -p` run with a scratch `CLAUDE_CONFIG_DIR` whose `settings.json` registers `SessionStart` to the shim (consent item below). If false: self-claim stays disabled, R7 falls back to the shell claim, and the spec's reversal condition 1 is reported. A Claude result does not stand in for Codex or Pi; each provider's self-claim is enabled only after its own spawn path is checked.
  - Every supported start path emits `SessionStart` (Claude `startup|resume|clear|compact`, Codex `startup|resume`, Pi `session_start` reasons) → provider docs and the fixture cases; if a path is missing, that path keeps the shell claim as its only mint.
  - `wezterm.executable_dir` exists in the installed WezTerm → executed 2026-09-06 against WezTerm `20260905-195314-b99b1ca2` with a disposable config: it returns `/opt/homebrew/bin` from a shell launch, which holds a `wezterm` symlink into the app bundle, and `background_child_process` is a function. A Dock launch returns the bundle directory itself; both contain `wezterm`. The bundle path stays as the last fallback in `wezterm_executable`.
  - Claude `PermissionRequest` hooks with empty stdout do not fail closed → the same scratch-config run; if they do, `hooks event` prints `{}` for that event only.
  - `CLOCK_MONOTONIC_RAW` through `libc` returns values comparable with existing Python-written records on this machine → a test reads one Python-written record and asserts ordering against a fresh Rust observation; if not comparable, activation is held until an ordering boundary is designed, because a shim flip does not change the socket incarnation and Python-written and Rust-written records share panes and launches across the flip.
- **Stop conditions:** a required identity or atomicity boundary has no std or named-crate mechanism; parity would require dropping a behavior R5 retains; a Python test maps to neither a Rust test nor a recorded cut at U14.
- **Authority boundaries:** no commits or pushes; no edits to bootstrap, user settings, hooks files or the installed Pi extension (fallback: adapted copies under `tests/fixtures/consumer-migration/`); no writes to the live state directory (fallback: `WEZTERM_ATTENTION_DIR` set to a disposable root in every test); no provider spend beyond the one consented `claude -p` run; no shim-default flip without the activation consent; U15 and U16 Lua is built and rehearsed in a separate copy of the checkout (whether that copy is a git worktree is the user's call at build time) and moved into this checkout as one bundle after the rehearsal, announced before the move, because the plugin symlink makes any Lua edit here live on the next config reload.
- **Expected gate map:** U9 → `cargo test`, `check.py`, Lua and Bun suites unchanged; U10 → provider fixture tests, permitted temporary failure: none; U12 → maintenance cases; U15 → `luajit tests/auto_clear_spec.lua` with the republish argv assertion updated to the scheduled form; U16 → Lua cases and `sh tests/run_wezterm_smoke.sh` after every module move; U13 → `bun test`, `node tests/pi_node_runtime.mjs`, shell tests; U14 → full `tests/gate.sh` in the disposable copy with the Python steps removed.
- **Human inventory:**
  - Activation consent — role: consent — flipping the shim default in this checkout changes the live GUI's writer immediately. Scoped grant sought before U14's last step: action, flip `bin/attention` default; target, this checkout; effect, live hooks and the ViTerm launcher use Rust; limit, only after the disposable-copy gate is green and the measurement is recorded; reversal, restore the shim branch. Held action without it: the flip. Everything else completes.
  - One `claude -p` run with a scratch config dir — role: consent — token spend for the `/dev/tty` contact check. Stand-in: the Node child inside a disposable pane proves the OS mechanism and leaves only the provider-specific spawn flags unverified. Held action without it: enabling the self-claim by default; R7 stays behind the stand-in.
  - Verdict on the measurement report — role: judgment — pass shape: p95 hook latency under Rust at or below Python's on this machine and state size unchanged; if not, U14 proceeds but the report is flagged in the activation ask.

### Second-opinion fold (2026-09-06)

ChatGPT Pro (Agentify run `5ee69ec0`, Pro confirmed, generation unverified, saw 60k of the 80k brief) returned REJECT. Folded: O8 writes both timestamp fields; one Pi fallback rule; claim output names the current launch; U16 checkpoints are patches, U14 rollback is a copied tree; O2 fixes the lock order; the clock fallback is removed; the selected binary's manifest digest gates activation; nested-start rule stated; prompt return clears lead activity only. Kept against the review: a nested `resume` may replace the lead binding, as Python does today, because it is a deliberate user act in the pane.

## Disconfirming Evidence

| Risk or prior failure | Required proof / kill condition |
|---|---|
| Wrong-pane attribution or stale launch takeover | equal pane ids in two sockets, old tty after rebirth, self-claim with a mismatching env id; any cross-address write rejects the design |
| Self-claim from a nested or inherited process | nested `startup` is rejected with `binding_conflict` and nested `resume` replaces the binding, both pinned by U10 tests; a nested process that mints a second launch for a claimed pane fails U10 |
| Prompt return hides a live agent | Ctrl-Z, backgrounded-agent and nested-shell cases; a view that stays empty after the agent's next hook fails U10 |
| Reconnect still depends on a human | reattach rehearsal on a disposable mux with a Dock-like minimal PATH; any pane left unpublished after the schedule fails U15 |
| Manifest drift between binary and plugin | embedded-equals-file test in U9; before activation, `doctor` run through the binary the shim selects must report the checkout's manifest digest (U14); hooks do not detect skew at run time, so a later mismatch is a deployment error `doctor` reports, not a hook diagnostic |
| Rust slower than the Python it replaces | the U14 measurement; no performance claim is made before it exists |
| Activation breaks the live GUI | the shim stays Python by default until the consented flip; Lua from U15 and U16 lands only as a rehearsed bundle and is announced first; the flip is the only held action |

## Bug-trace cross-check

| Reported failure | Contract clause | Behavior | Expected | Match |
|---|---|---|---|---|
| Republish fires once before panes attach (inbox 2026-09-05) | U15 schedule table | retries until no unpublished pane | all panes published | yes |
| Child cannot find `wezterm` under launchd PATH (inbox 2026-09-05) | U9 `wezterm_executable`, U15 child PATH | bundle fallback and executable dir on PATH | publish succeeds from a Dock-launched GUI | yes |
| Relay needs provider from binding (inbox 2026-09-04) | U15 `get_attention_view` | returns `provider` when nothing is drawn | `detect_agent` gets the provider | yes |
| No launch claim on the live zsh setup | O5, U10 self-claim | `SessionStart` mints the launch | v2 hooks bind without a shell claim | yes, pending contact check |
| Escape interrupt leaves `thinking` until TTL | U10 prompt return | prompt return clears at the next prompt | view clears when the agent exits or the user gets a prompt | yes for exit; interrupt inside a live agent still waits for TTL, as the spec accepts |
| Pi adapter silently downgrades on exit 3 | U13 fallback rule | fallback only with no root | failure reported, no v1 marker | yes |

## Risks & Dependencies

| Risk | Mitigation |
|---|---|
| `/dev/tty` unreachable from a provider's hook subprocess | stand-in proof first; self-claim ships disabled if the contact check fails; shell claim remains |
| Runtime manifest interpretation slower than expected | measured in U14; the manifest is small and parsed once per process |
| Prompt return adds a write per prompt in bound panes | duplicate observations skipped; one small file; measured in U14 |
| The symlinked live plugin picks up Lua changes from U15 or U16 before U14 | both units are built in a separate copy and moved in as one rehearsed bundle; the user is told before the move because the live GUI will load it on its next config reload |
| Bootstrap hooks never switch to `bin/attention` | outside this plan; the projection keeps bridge working either way, and the consumer copies show the change needed |
