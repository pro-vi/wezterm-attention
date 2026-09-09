---
title: Independent attention facts and consumers
objective: Custom WezTerm consumers and the live Relay can use the same validated V2 facts without inheriting the bundled tab renderer's presentation priority, while producers are taught the current Rust writer path.
type: feat
status: active
date: 2026-09-08
origin: conversation; I1/I2 boundary audit; .inbox/.read/2026-09-04-relay-needs-provider-from-binding.md
---

# Independent attention facts and consumers

## Background

The V2 reader already derives an `AttentionView` with separate activity, review, subagent, identity, presence, confidence, and health facts. The bundled formatter intentionally reduces those facts to one presentation winner. `get_attention_view(pane)` currently copies only part of the cached view, so another consumer sees the winning `type` but cannot recover the independent `activity_type` when review wins.

The live bootstrap Relay has the related integration gap recorded in the archived inbox note. Its `detect_agent` reads the legacy activity source, foreground process, and title. A quiet Codex pane attached through a mux can supply none of those after acknowledgement, even though the V2 cached view still carries the binding provider.

The README also teaches direct flat-file writes as the general protocol before it explains the Rust CLI. That is the V1 compatibility protocol, not the current producer boundary. The bundled shell example already calls `bin/attention` and should become the leading producer pattern.

Facts verified at planning contact:

- `plugin/reader.lua:340-364` already owns the complete internal `AttentionView`; no new filesystem read or record model is required.
- `plugin/runtime.lua:544-559` returns a field-by-field copy, not the cached table, but omits `activity_type`, source, identity, presence, and health.
- `puppet` is already a required V2 activity field and part of the shipped six-value API. Latest wezpup injects `WEZPUP_PANE` into managed Claude launches, and bootstrap's live V1 writer converts that signal to `puppet=true`; the manual renderer can then exclude wezpup-supervised lead activity while retaining subagent counts. The Rust V2 writer currently drops the distinction by always writing false. Evidence is recorded in `.research/synthesis-wezpup-puppet-attention-boundary-2026-09-08.md`.
- `plugin/format.lua` consumes the internal view and owns bundled presentation priority. The accessor does not need to replace it.
- `src/query.rs:15-35` exposes binding identity and liveness axes, but not activity, review, or subagent facts. It is a binding query, not a complete external attention reader.
- `tests/fixtures/consumer-migration/detect_agent.lua` proves the intended provider-first Relay lookup against a stand-in.
- Bootstrap's real `configs/wezterm.lua:993-1026` has not adopted that lookup.
- `~/.wezterm.lua` points to bootstrap's `configs/wezterm.lua`, and the installed plugin points to this checkout. Edits in either source tree become live on the GUI's next configuration reload.

## Requirements

| ID | Required outcome | Reason / consumer |
|---|---|---|
| R1 | `get_attention_view(pane)` exposes independent activity, review, subagent, binding, identity, presence, confidence, and health facts while retaining `type` | Custom GUI consumers choose their own presentation policy |
| R2 | The accessor remains cache-only and returns no mutable alias into the cache | Polling and reader ownership remain unchanged |
| R3 | The README leads producers to `bin/attention`, labels direct flat-file writes V1 compatibility, and teaches the consumer decision rubric | New producers should not create V2 records themselves; consumers need fact-selection guidance |
| R4 | Bootstrap Relay prefers the validated V2 binding provider, then keeps its existing activity/process/title fallbacks | Quiet mux-attached panes remain addressable after acknowledgement |
| R5 | The bundled renderer, six-value `get_attention`, repository structure, and `bindings --json` contract remain intact | This is additive access and documentation, not a renderer rewrite, CLI expansion, or package split |
| R6 | A wezpup-managed pane retains `puppet=true` through the Rust V2 activity record and public view | V2 must not erase provenance that current V1 producers and renderers already use |

## Naming Ledger

| Role / meaning | Existing repo term | Chosen name | Owner / placement | Status | Second consumer / reason | GR6 sibling disposition |
|---|---|---|---|---|---|---|
| Complete cached assessment of one pane | `AttentionView` | `AttentionView` | `plugin/reader.lua` | reuse | formatter and public accessor | no asymmetry |
| Renderer-selected compatibility value | `type` | `type` | `AttentionView` and public accessor | reuse | existing callers and formatter | documented as effective presentation type |
| Current activity independent of review priority | `activity_type` | `activity_type` | `AttentionView` and public accessor | reuse | custom renderer and consumer guide | paired with `review`, not renamed to `type` |
| User-owned review fact | `review` | `review` | `AttentionView` and public accessor | reuse | bundled formatter and custom consumers | no asymmetry |
| Wezpup-managed activity provenance | `puppet` | `puppet` | activity record, six-value API, formatter, public accessor | reuse | V1 compatibility and custom consumers | keep the shipped name; do not introduce a second `managed` alias |
| Record validity summary | `binding_health` | `binding_health` | `AttentionView` and public accessor | reuse | Lua consumers and binding query use the same vocabulary | no generic `health` alias |
| Full pane identity | `address`, `launch_id`, `marker_id` | same | `AttentionView` and public accessor | reuse | Relay and custom consumers | `cache_key` stays internal |
| Guidance for non-bundled consumers | none | consumer guide | `docs/consumer-guide.md` | new | custom Lua consumers and CLI consumers | no sibling guide exists; record contract stays low-level |

No module, command, event, persisted field, or protocol enum is renamed or introduced.

## Architecture Decision

**Approach:** Keep `read_attention_view` as the single semantic authority. Preserve wezpup-managed provenance at the Rust activity-write boundary, without reading wezpup state. Expand the existing `get_attention_view` field allowlist to copy the supported public facts, including `puppet` and a fresh copy of the nested address. Keep `type` as the compatibility presentation projection and expose `activity_type` plus `review` as independent axes. Document the facts and their intended consumers, then update bootstrap Relay to prefer the view's provider before its existing fallbacks.

**Why this approach:** It reuses the complete cached model already consumed by the formatter. Returning the raw cached table would be smaller code but would expose `_records`, diagnostics, scheduling fields, and mutable nested tables. Recomputing a second view in the accessor would duplicate reader policy and add I/O. A package split or new reader would add ownership without adding a capability.

**Trade-offs:** The public table grows and therefore becomes a compatibility surface. It intentionally remains an allowlisted projection rather than a dump of every internal field. `bindings --json` remains narrower; consumers needing activity facts inside WezTerm use the Lua view.

**Approval criteria:** Approval accepts the additive public field set, exact matched-pane ingestion of wezpup provenance under the shipped `puppet` name, the distinction between `type` and `activity_type`, provider-first Relay detection, and the explicit decision not to expand the CLI or replace the bundled renderer in this work.

## Representation Ledger

| Concept | Authority | Derived consumers | Necessary mirrors / projections | Boundary parser | Drift / equivalence guard |
|---|---|---|---|---|---|
| Pane attention facts | `plugin/reader.lua` `read_attention_view` | runtime cache, formatter, accessor | public `get_attention_view` allowlist | V2 record parsers in `plugin/protocol.lua` | Lua test compares every public field to the cached view |
| Effective display type | `effective_attention_type(activity_type, review)` | bundled formatter and compatibility `type` | six-value `get_attention`; public view `type` | configured priority map | mixed activity-plus-review tests under both priority orders |
| Pane identity | V2 wire `address` and `launch_id` | cache key derivation and public view | `marker_id` compatibility projection | `parse_wire_json` and pane address validator | nested-copy mutation test plus full-address fixture |
| Binding provider | validated binding record | public view and bootstrap Relay | bootstrap's supported-agent vocabulary | plugin provider enum validation; Relay reply-record validation | provider-first detection test and unknown-provider fallback test |
| Wezpup-managed activity | matching `WEZPUP_PANE` and `WEZTERM_PANE` in the hook environment | V2 activity, public view, renderer | V1 bootstrap writer's `puppet=true` | canonical pane-id validation plus equality | Rust lifecycle cases and V1/V2 projection parity test |
| Presence, confidence, health | reader diagnostics and exact record scope | public view and consumer guide | `BindingRow` uses the same names for a different query scope | Lua diagnostic classifier; Rust query validators | documentation states the GUI and CLI scopes separately |
| V1 marker state | flat marker parser | six-value `get_attention` and bundled compatibility | README V1 section | legacy JSON reader | existing V1 Lua suite stays unchanged |

Intentional asymmetry: `get_attention_view` is the full cached GUI assessment. `bindings --json` enumerates binding identity and liveness from disk and process probes. The two surfaces share field names where meanings match, but they are not required to return the same rows or fact set.

## Program obligations

- **O1 — Public allowlist:** `get_attention_view` returns only supported semantic fields; `_records`, `diagnostics`, `cache_key`, `next_wakeup_unix_ns`, and formatter-only state never cross the public boundary.
- **O2 — Copy isolation:** every returned table, including nested `address`, is detached from the cache; mutating any returned value cannot change a later read or render.
- **O3 — Independent axes:** `activity_type` reports eligible activity and `review` reports review presence regardless of configured priority; `type` alone applies the configured display priority.
- **O4 — Cache-only read:** the accessor performs pane identity resolution and a cache lookup only. It performs no filesystem, clock, process, tty, or subprocess work.
- **O5 — Existing nullability:** absent activity keeps `activity_type`, `event_id`, and `source` absent; absent binding keeps provider and binding fields absent. No placeholder strings are invented.
- **O6 — Honest consumer scopes:** documentation never describes direct V1 writes as V2 production and never describes `bindings --json` as a complete attention-state reader.
- **O7 — Relay precedence:** bootstrap uses a supported view provider first. If the accessor is absent, errors, returns no provider, or returns an unknown provider, the existing activity-source, foreground-process, and title fallbacks run unchanged.
- **O8 — Puppet provenance:** a Rust activity writes `puppet=true` iff `WEZPUP_PANE` and `WEZTERM_PANE` are both canonical pane ids and equal. Missing, malformed, or mismatched provenance writes false. Attention reads no wezpup file, socket, lifecycle record, or process state.
- **O9 — Puppet parity:** the same V2 activity and its flat V1 projection carry the same `puppet` value; the public view copies that value without turning it into display policy.

## High-Level Technical Design

Directional guidance for review, not an implementation specification:

```text
matching WEZPUP_PANE + WEZTERM_PANE --> Rust activity puppet fact
                                             |
                                             v
validated V2 records
        |
        v
reader.read_attention_view()  -- semantic authority
        |
        v
runtime attention cache
   |            |                     |
   |            |                     +--> get_attention() -- six-value V1 compatibility
   |            +--> format.lua ---------> bundled priority and tab presentation
   +--> get_attention_view() -- copied public fact allowlist
                              |
                              +--> custom Lua consumers
                              +--> bootstrap detect_agent() --> Relay

validated V2 records --> Rust bindings query --> binding identity/liveness rows
                                           (separate, intentionally narrower scope)
```

### Public field contract

| Axis | Public fields | Meaning |
|---|---|---|
| Compatibility presentation | `type` | Current winner after configured activity-versus-review priority |
| Activity | `activity_type`, `event_id`, `source`, `puppet` | Eligible lead activity and its provenance; independent of review |
| Review and children | `review`, `subagents` | Independent review presence and eligible subagent count |
| Binding | `provider`, `binding_id`, `binding_phase` | Validated provider binding facts, including a quiet or ended binding |
| Pane identity | `address`, `launch_id`, `marker_id` | Full V2 pane address, current launch, and compatibility pane id |
| Read assessment | `pane_presence`, `reader_confidence`, `binding_health` | What the GUI reader observed and how safely a consumer may rely on it |

`frame` remains a bundled/legacy presentation value and is not added by this plan. `puppet` remains a fact: consumers decide whether it changes presentation, while `show_puppet` keeps the bundled renderer's existing policy. Internal records, raw diagnostics, cache keys, and wakeup deadlines also remain private.

## State-Action Contracts

### Cached view × public read

| Cached state | Caller observation | Cache / durable state | Side effects | Duplicate / race behavior | Locking test |
|---|---|---|---|---|---|
| Activity only | `type` and `activity_type` match; `review=false` | no cache or file change | no I/O or redraw | repeated calls return equivalent detached tables | `get_attention_view_exposes_activity_facts` |
| Review only | `type=review`, `activity_type=nil`, `review=true` | no cache or file change | none | repeated calls cannot create activity | `get_attention_view_distinguishes_review_only` |
| Activity plus review, review wins | `type=review`; activity fields remain present; `review=true` | no cache or file change | none | priority changes affect only `type` | `get_attention_view_keeps_activity_when_review_wins` |
| Activity plus review, activity wins | `type=activity_type`; `review=true` remains | no cache or file change | none | same facts survive the opposite priority order | `get_attention_view_keeps_review_when_activity_wins` |
| Bound but quiet | provider, binding, identity, and assessment fields remain; activity fields are absent | no cache or file change | none | acknowledgement does not erase binding identity | `get_attention_view_keeps_quiet_binding_facts` |
| Unavailable or invalid current read | fields mirror the reader's cached result with its confidence, presence, and health | no recovery write | none | exact-scope cache recovery cannot cross address or launch | `get_attention_view_exposes_uncertain_state` |
| Returned table mutated by caller | next read and formatter still see original values | no cache or file change | none | nested address mutation is isolated on every call | `get_attention_view_returns_deep_public_copy` |
| No cached pane | returns nil | no cache or file change | none | concurrent poll may make a later call non-nil; the accessor never waits | existing no-cache case plus public-view test |

Invariants:

- `event_id`, `source`, and `puppet` describe activity iff `activity_type` is present; absent activity exposes no positive puppet provenance.
- `type` may differ from `activity_type` iff review is present and configured review priority wins.
- Returned `address` equals the cached address by value and never by table identity.
- A public read never changes cache state, durable state, or renderer scheduling.

Omitted-state challenge:

- V1 panes are not promoted into a synthetic V2 identity; their shipped six-value API remains authoritative.
- An unpublished mux pane has no full identity and therefore no cached V2 view; publication scheduling remains the recovery mechanism.

## Implementation Units

### U5. Preserve wezpup-managed activity provenance

- **Goal:** Rust V2 activity and its flat projection retain the managed-pane fact currently emitted by bootstrap's V1 writer.
- **Requirements:** R2, R5, R6
- **Dependencies:** None
- **Files:**
  - Modify: `src/lifecycle.rs`
  - Test: `tests/rust/lifecycle_spec.rs`
- **Approach:** Derive the existing `puppet` boolean from the hook environment at activity construction. Accept it only when canonical `WEZPUP_PANE` equals canonical `WEZTERM_PANE`; do not inspect wezpup state or add a new protocol field.
- **Patterns to follow:** `identity::canonical_pane_id` for canonical decimal validation; bootstrap `configs/claude/hooks/utils/wezterm-attention.ts:48-53` for current V1 intent; wezpup `src/puppet.rs` `compose_spawn_command_with_session` for the producer contract.
- **Test scenarios:**
  - *Happy path:* matching canonical pane ids plus provider activity → V2 activity and flat marker both carry `puppet=true`.
  - *Edge cases:* missing `WEZPUP_PANE` → false; malformed, leading-zero, or different pane id → false; duplicate activity repair preserves the same value without refreshing event time.
  - *Error path:* provenance mismatch never rejects an otherwise valid provider event and never changes launch or binding identity.
  - *Integration:* a wezpup-style environment passed through a real Rust hook subprocess produces the same puppet fact the current V1 bootstrap writer would emit.
- **Verification:** the Rust writer carries true only for the exact managed pane, V1/V2 values agree, and no wezpup-owned state is read.
- **Runtime evidence:** unverified — run the lifecycle cases and one disposable-state hook subprocess with matching and mismatching pane values.
- **Checkpoint:** auto — focused provenance cases and the full Rust lifecycle suite pass in the disposable copy.

### U1. Expose independent cached facts

- **Goal:** `get_attention_view` returns the supported independent facts without exposing cache-owned tables.
- **Requirements:** R1, R2, R5, R6
- **Dependencies:** U5
- **Files:**
  - Modify: `plugin/runtime.lua`
  - Test: `tests/auto_clear_spec.lua`
- **Approach:** Extend the current explicit return-table allowlist. Copy `address` by value. Preserve `type`, existing fields, nil/false behavior, and the cache-only implementation; do not return the cached table wholesale.
- **Patterns to follow:** `plugin/reader.lua:340-364` for field meanings; `plugin/runtime.lua:544-559` for the existing copied-accessor shape; `tests/auto_clear_spec.lua:3068-3090` for cache mutation coverage.
- **Test scenarios:**
  - *Happy path:* thinking plus review with review ranked higher → `type=review`, `activity_type=thinking`, `review=true`, and source/puppet/identity/assessment fields match the cached view.
  - *Edge cases:* reverse the configured priority → only `type` changes; review-only has no activity; bound-but-quiet retains provider and identity; mutate scalar fields and `address.realm_id` on the returned table → the next read and formatter cache remain unchanged.
  - *Error path:* an unavailable current read exposes `pane_presence`, `reader_confidence`, and `binding_health` without exposing diagnostics or internal records.
- **Verification:** every field in the public contract is value-equal to the internal view, mixed activity and review remain independently observable under both priority orders, and caller mutation cannot affect the cache.
- **Runtime evidence:** unverified — the builder adds and runs the focused Lua cases against the real module loader in the disposable copy.
- **Checkpoint:** auto — focused public-view cases and the full Lua suite pass in the disposable copy.

### U2. Teach current producers and fact consumers

- **Goal:** A cold reader learns the Rust writer path first and can choose a consumer surface and presentation policy without confusing independent facts with the bundled renderer's winner.
- **Requirements:** R3, R5, R6
- **Dependencies:** U1
- **Files:**
  - Modify: `README.md`, `docs/record-contract.md`
  - Create: `docs/consumer-guide.md`
- **Approach:** Replace the README's general direct-write framing with `bin/attention` producer examples, then retain the existing file format under an explicit V1 compatibility heading. Put the detailed consumer rubric in one guide rather than expanding the README into a second protocol spec.
- **Patterns to follow:** `examples/hook.sh` for the canonical producer command; `README.md:332-372` for public API examples; `docs/record-contract.md` for low-level authority statements.
- **Test scenarios:**
  - *Documentation contract:* producer entry path calls `bin/attention`; V1 direct writes are visibly labeled; public field table matches U1; `type` is described as derived while activity/review remain independent.
  - *Consumer rubric:* guide covers which facts matter, where to display them, consumer-owned ranking, acknowledgement behavior, and uncertain state.
  - *Mode boundary:* guide states that `renderer="manual"` disables only the bundled tab formatter; `auto_poll`, `request_redraw`, `review_key`, and acknowledgement configuration remain separate controls.
  - *CLI boundary:* guide describes `bindings --json` as identity/liveness data and states that it does not yet expose activity, review, or subagent counts.
  - *Managed provenance:* guide defines `puppet` as a producer-supplied wezpup-managed activity fact, not a second lifecycle reader or an instruction every consumer must hide.
- **Verification:** README and guide can be read without inferring that V2 producers write records directly, that `type` is the only attention fact, or that manual rendering disables other plugin behavior.
- **Checkpoint:** auto — `md map` passes for all three documents and every referenced example/field exists at contact.

### U3. Use the V2 provider in bootstrap Relay

- **Goal:** The actual bootstrap Relay identifies a quiet bound mux pane from the V2 view before using legacy heuristics.
- **Requirements:** R4, R5
- **Dependencies:** U1
- **Files:**
  - Modify (bootstrap repository): `configs/wezterm.lua`, `configs/wezterm/relay.lua`
  - Test (bootstrap repository): `tests/wezterm-agent-relay/relay_spec.lua`
  - Test (attention repository): `tests/fixtures/consumer-migration/detect_agent.lua`, `tests/fixtures/consumer-migration/check.lua`
- **Approach:** Keep pane and process reads in `configs/wezterm.lua`; move the existing pure agent-precedence decision into the already-owned Relay module so its provider-first order is directly testable. Provider wins, followed by legacy activity source, foreground process metadata, and title. Unknown/missing/error view results fall through.
- **Patterns to follow:** bootstrap `configs/wezterm.lua:993-1026` for current precedence and call sites; bootstrap `configs/wezterm/relay.lua:51-60` for pure process detection; the attention consumer-migration fixture for the provider-first call.
- **Test scenarios:**
  - *Happy path:* a quiet Codex pane with `view.provider=codex`, no activity source, no process, and a plain title resolves as Codex and no longer returns `agent_unknown`.
  - *Edge cases:* provider remains usable after activity acknowledgement and under any renderer priority; both reply resolution and overview collection use the same precedence; a valid provider wins over a misleading title.
  - *Error path:* absent accessor, thrown accessor, nil view, missing provider, or unknown provider preserves existing process/title detection and never throws from the key handler.
  - *Integration:* the Relay still validates the selected agent against the reply record before returning exact content.
- **Verification:** the real bootstrap config consumes `get_attention_view`; provider-first and every fallback are locked in the Relay suite; the attention repository's adapted consumer remains equivalent in behavior.
- **Runtime evidence:** unverified — run bootstrap's Relay tests and config lint in a disposable bootstrap copy wired to the disposable attention copy.
- **Checkpoint:** auto — attention consumer-migration checks and bootstrap `scripts/ci-lint.sh` pass in their disposable copies.

### U4. Rehearse and activate the cross-repo bundle

- **Goal:** The tested provenance ingestion, accessor, documentation, and real Relay integration enter the two live-linked source trees together without an untested reload window.
- **Requirements:** R1-R6
- **Dependencies:** U5, U1, U2, U3
- **Files:**
  - Promote (attention repository): `src/lifecycle.rs`, `tests/rust/lifecycle_spec.rs`, `plugin/runtime.lua`, `tests/auto_clear_spec.lua`, `README.md`, `docs/record-contract.md`, `docs/consumer-guide.md`, `tests/fixtures/consumer-migration/detect_agent.lua`, `tests/fixtures/consumer-migration/check.lua`
  - Promote (bootstrap repository): `configs/wezterm.lua`, `configs/wezterm/relay.lua`, `tests/wezterm-agent-relay/relay_spec.lua`
- **Approach:** Run both repository gates against ordinary disposable copies. After explicit activation consent, move the exact rehearsed files into both source trees as one announced bundle. Do not trigger the live GUI reload automatically.
- **Patterns to follow:** the V2 Lua bundle promotion recorded in `docs/reviews/2026-09-07-rust-port-certification.md`; bootstrap `scripts/ci-lint.sh` for its full local contract gate.
- **Test scenarios:**
  - *Integration:* working-plus-review is distinguishable in the public view; the bundled tab appearance remains unchanged; a simulated quiet Codex mux pane resolves through Relay; both legacy fallback and V1 rendering stay green.
  - *Live acceptance:* after separate reload consent, a quiet acknowledged Codex mux pane resolves through the actual Relay key path without `agent_unknown`.
- **Verification:** both disposable-copy gates pass, promoted files match the rehearsed copies byte-for-byte, and no unrelated source file changes during promotion.
- **Runtime evidence:** unverified — disposable cross-repo rehearsal is mandatory; live key-path evidence remains held until reload consent and user assistance.
- **Checkpoint:** gate — disposable gates plus byte manifests green → request activation consent; consent granted: promote and continue; consent absent: keep U1-U3 in disposable copies and hold promotion; live reload consent absent: stop after promotion without reloading.

## Scope Boundaries

- Do not change record schemas, provider event meanings beyond exact `puppet` derivation, provider hooks, shell integrations, or the six-value `get_attention` contract.
- Do not rename `puppet`, infer it from anything other than exact matched pane ids, or make attention read wezpup-owned state.
- Do not expand `bindings --json` with activity, review, or subagent state in this plan. Document its current boundary accurately.
- Do not add a daemon, package split, renderer replacement, public setter, callback subscription API, or new dependency.
- Do not expose raw diagnostics, internal record snapshots, cache keys, retry state, or TTL wakeup deadlines through the Lua accessor.
- Do not redesign Relay reply records or screen scraping; only change agent detection precedence.
- Do not change the bundled renderer's default priority, colors, acknowledgement set, review key, or redraw behavior.

### Deferred to Follow-Up Work

- A process-external complete attention reader would require a separate design for activity, review, and subagent enumeration in Rust. This plan records only that `bindings --json` is not that reader; it does not create the API.
- Rendering `reader_confidence` or `pane_presence` in the bundled tab bar remains outside scope. They become available to custom consumers through U1.
- Replacing the shipped `puppet` name with a producer-neutral provenance vocabulary remains outside scope; this plan restores and exposes existing behavior without creating an alias.

## System-Wide Impact

- **Interaction graph:** wezpup launch env → Rust activity writer → V2 record and V1 projection → Lua reader → cache → expanded accessor → custom consumers and bootstrap Relay. The formatter continues to consume the same cache directly.
- **Error propagation:** accessor lookup failures remain nil/no-throw; bootstrap wraps the call and falls back. Health and confidence are facts, not exceptions.
- **State lifecycle:** no new persisted state. The only mutation risk is aliasing a nested cached table, prevented by O2 and its test.
- **API surface parity:** `type` and all existing accessor fields retain meaning. New fields reuse names already present in the internal view and protocol. `get_attention` and CLI JSON do not change.
- **Integration coverage:** Lua module tests prove fact separation and copy isolation; bootstrap Relay tests prove precedence; disposable-copy gates prove both source trees; optional live acceptance proves the installed key path.
- **Unchanged invariants:** focus acknowledges exact activity only; review remains independent; renderer priority remains consumer-local; V1 markers still render; unknown provider values never become Relay agents.

## Build Execution Contract

- **Closed decisions:** exact matched-pane `puppet` ingestion; additive allowlisted fields including `puppet`; `type` preserved; `activity_type` and `review` independent; nested address copied; no raw cache table; provider-first Relay; Rust CLI remains a binding query; bundled renderer and repository structure stay.
- **Builder autonomy:** order public table fields, refine guide wording, and arrange focused fixtures without asking, provided every named field and precedence rule remains. Record any test-only seam added inside an existing test file.
- **Verify at contact:** internal view still owns the named fields → map from `reader.lua`; if a field moved, follow its current semantic owner rather than duplicating it. Bootstrap detection call sites still serve reply and overview → update both; if one was removed, test the surviving consumers. Live symlinks still target these source trees → if yes, use disposable copies; if no, record the resolved paths and continue without assuming live impact.
- **Stop conditions:** stop if any requested fact requires new filesystem/process I/O in `get_attention_view`; if preserving `type` cannot coexist with independent axes; if Relay must bypass exact reply-record agent validation; or if a disposable gate reveals a V1/rendering behavior with no plan-preserving fix.
- **Authority boundaries:** no commits or pushes unless asked. Do not edit either live-linked source tree during U1-U3; use ordinary disposable copies. Do not promote the cross-repo bundle without explicit consent. Do not reload the live WezTerm config or write live attention state without separate consent. Use fake panes, temporary state roots, and disposable config files for every automated exercise.
- **Expected gate map:** U5 → focused provenance subprocess/lifecycle cases and full Rust lifecycle suite green; U1 → focused public-view cases and full Lua suite green; U2 → three Markdown maps green, no code expectations change; U3 → attention consumer fixture plus bootstrap Relay/config gate green; U4 → both full gates green in disposable copies, byte manifests match, permitted temporary failure: live acceptance unrun while reload consent is absent.
- **Human inventory:** cross-repo promotion — consent — permits moving the named rehearsed files into the two live-linked source trees; without it U1-U3 remain complete only in disposable copies and U4 promotion is held. Live acceptance — consent plus assistance — permits one config reload and one Relay keypress against a quiet acknowledged Codex mux pane; without it source promotion may complete but live behavior remains explicitly unverified.

## Disconfirming Evidence

| Claim | Probe that would disconfirm it | Required response |
|---|---|---|
| Public facts remain independent | Working plus review makes `activity_type` nil, or changing priority changes `review`, source, or identity | hold U1; repair the accessor projection |
| Managed provenance survives V2 | Matching `WEZPUP_PANE` writes false, mismatched pane writes true, or V1/V2 projections disagree | hold U5; repair exact-pane derivation before exposing the field |
| Returned values cannot mutate cache | Changing returned `address.realm_id` changes a later accessor result or formatter input | hold U1; copy every nested public table |
| Accessor remains cache-only | Focused probe observes file, clock, process, tty, or subprocess work during a call | stop; the architecture boundary is broken |
| Bundled renderer is unchanged | Existing formatter output or acknowledgement tests change after U1 | hold promotion; remove accessor-side effects |
| Relay uses V2 provider | Quiet bound Codex fixture still reaches `agent_unknown` | hold U3; inspect both real detection call sites |
| Fallbacks remain | Missing/throwing accessor prevents process or title detection | hold U3; restore guarded fallthrough |
| Documentation is honest | A cold README path still instructs V2 producers to write flat files, or calls `bindings --json` a full attention reader | hold U2; correct the surface before promotion |

## Bug-trace cross-check

| Bug / requirement | Contract clause | Planned behavior | Expected behavior | Match? |
|---|---|---|---|---|
| I1: review winner hides simultaneous thinking | O3 and mixed-state matrix | `type=review`, `activity_type=thinking`, `review=true` | consumer can choose its own priority | yes |
| I1: public view omits identity and assessment | public field contract | address, launch, marker, presence, confidence, and health are copied | consumer sees supported internal facts | yes |
| I1: returned table could alias cache after address is added | O2 | nested address copied on each call | caller mutation cannot alter cache | yes |
| Wezpup-managed pane loses `puppet` under Rust V2 | O8, O9, U5 | exact matching pane env writes true to V2 and V1 projection | existing supervised-pane distinction survives canonical writer migration | yes |
| Inbox: quiet mux Codex becomes `agent_unknown` | O7 and U3 | binding provider precedes activity/process/title fallbacks | Relay resolves the pane after acknowledgement | yes |
| I2: README presents flat writes as general protocol | O6 and U2 | Rust CLI leads; direct writes are V1 compatibility | current producers use the canonical writer | yes |
| I2: manual renderer controls are conflated | U2 mode boundary | renderer, polling, redraw, review key, and acknowledgement documented separately | users can disable only what they intend | yes |
| I2: CLI looks more complete than it is | intentional asymmetry and scope boundary | binding query limitations are explicit | no false external-reader promise | yes |

## Risks & Dependencies

| Risk | Mitigation |
|---|---|
| Additive public fields become accidental internal API | explicit allowlist and O1 exclusion tests |
| Nested address leaks a mutable cache alias | per-call value copy and mutation test |
| Consumers continue treating `type` as raw activity | README and guide define `type` as effective presentation and show both axes |
| Bootstrap trusts an arbitrary provider | plugin validation plus supported-agent guard and existing reply-record validation |
| Ambient `WEZPUP_PANE` is stale or inherited from another pane | require canonical equality with current `WEZTERM_PANE`; mismatch becomes false, never a write rejection |
| Transient unavailable reads erase Relay detection | exact-scope cached provider remains consumable; guide explains confidence and health so other consumers may choose differently |
| One live-linked tree changes before the other | build and test in disposable copies; one consented cross-repo promotion bundle; no automatic reload |
| Documentation duplicates the protocol authority | guide links to `docs/record-contract.md` and describes consumption decisions, not record schemas |
| Scope expands into a new external reader or renderer framework | explicit non-goals; no new dependency, package, or CLI row shape |
