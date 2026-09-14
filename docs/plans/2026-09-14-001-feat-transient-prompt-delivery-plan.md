---
title: Transient prompt delivery
objective: Consumers can correlate submitted input without duplicating provider payload parsing.
type: feat
status: completed
date: 2026-09-14
origin: conversation
---

# Transient prompt delivery

## Background and requirements

The September 14 conversation authorizes a quick architecture pass followed by a local build. The incoming `2026-09-14-include-prompt-on-submit-delivery.md` note asks for opt-in callback prompt text; it withdraws stored digest and sequence proposals. Attention owns provider extraction; consumers own markers, receipts and acceptance decisions.

- R1: `hooks event --include-prompt` supplies exact decoded callback text for admitted lead Claude/Codex `UserPromptSubmit` events.
- R2: Prompt content is transient, never written to Attention records, diagnostics or GUI caches. Delivery remains after lock release and uses the admitted identity and existing persistence gates.
- R3: Pin availability, size and independent opt-in behavior with executable tests and exact documented examples.
- R4: Qualify provider support honestly: Pi remains `unsupported`; no live activation or model spend.

Existing source: `src/main.rs` applies native events before `delivery_bytes`; `src/consumer.rs` checks persistence and bounds serialization; `src/providers.rs::reply_content` implements the matching content contract. Claude's official hooks reference documents `UserPromptSubmit.prompt` (https://code.claude.com/docs/en/hooks#userpromptsubmit). Codex source at `cd8dc1e9b6dd24d335f4fcb26546f36d045fd59a` serializes `UserPromptSubmitCommandInput.prompt` in `codex-rs/hooks/src/events/user_prompt_submit.rs`. These are documentation/source evidence, not live runtime proof.

Pi's bundled `invokeWriter` has no consumer registration and drops input text. Its upstream input callbacks can be transformed or handled by subsequent extensions. Do not add unused text transport or a new Pi configuration surface in this build.

## Naming ledger

| Meaning | Existing term | Chosen name | Owner | Status | Reason / sibling disposition |
|---|---|---|---|---|---|
| Optional transient text and its availability | ReplyContent | HookContent | src/consumer.rs | rename | Shared by reply and prompt; no alias for an intermediate build |
| Submitted callback text | provider prompt | prompt / prompt_content / include_prompt | consumer.rs / providers.rs / main.rs | new | Public delivery boundary; matches reply naming |
| Completed callback text | reply | reply / reply_content / include_reply | same files | reuse | Separate event applicability and independent opt-in |

## Architecture decision

Extend the existing envelope with an always-present `prompt` field and reuse one tagged content enum for both fields. Keep schema 1: this is an additive envelope field, not a record-format change. Prefer two explicit flags over `--include-content` so requesting replies does not request prompts. No new module or dependency: existing serde serialization and provider extraction are sufficient. Retain no Rust alias for the earlier ReplyContent name.

The content is the provider callback's text, not original keystrokes, a complete multimodal message, proof of model processing, or a controller acceptance receipt. Only consumers decide what receipt their evidence supports.

## Public contract and representation authority

Directional flow: native callback JSON → existing provider parser/application → released locks → prompt/reply extraction → bounded HookDelivery → executable stdin. The persisted observation continues to carry no text.

`HookContent` is the sole Rust/JSON availability authority, serialized by serde; documentation is a necessary prose mirror locked by exact JSON tests. `serde_json::Value` narrows through provider/event/actor checks and string matching. No GUI or record representation is added.

| Input / condition | prompt | reply |
|---|---|---|
| Neither flag | not_requested | not_requested |
| Both flags, lead Claude/Codex UserPromptSubmit with prompt string | available with exact text | unsupported |
| Both flags, lead Claude/Codex Stop with reply string | unsupported | available with exact text |
| Prompt flag, eligible submit, missing prompt | absent | not_requested |
| Prompt flag, eligible submit, null or non-string prompt | invalid | not_requested |
| Prompt flag, child / other event / Pi | unsupported | independently selected |
| Available content makes envelope exceed max_json_bytes | available fields become too_large, with no text | all non-available meanings preserved |

Empty strings are available. Text exists iff availability is available. No truncation. If the envelope still exceeds the limit after available text is omitted, no consumer is dispatched. Oversized native stdin is rejected before application under the existing limit. If both fields are available in a directly constructed envelope, omit both on overflow; no field has priority. Production errors use existing typed availability or not-dispatched outcomes.

## Implementation units

### U1 — Deliver opt-in prompt text

- Goal / requirements: implement R1–R4 as one executable CLI slice.
- Dependencies: none.
- Files: modify `src/main.rs`, `src/consumer.rs`, `src/providers.rs`; test `tests/rust/hook_consumer_spec.rs`.
- Approach: rename the shared enum, add independent extraction/flag, extend envelope size handling without touching native application or dispatch ordering.
- Patterns: `reply_content`, `delivery_bytes`, `admitted_reply_is_transient_scoped_and_runs_after_locks_release`.
- Tests: happy path exact Claude/Codex text through real binary and executable; edge empty/Unicode/newlines, missing/null/all wrong JSON types, opt-out/both flags, child/Pi/other events, raw and serialized size bounds; error rejected/partial/unconfirmed native state and stale identity prevent dispatch; integration consumer reentry proves lock release and state scans prove no text persistence.
- Verification: exact prompt JSON reaches only eligible consumers; existing reply and persistence behavior passes unchanged.
- Runtime evidence: unverified until the synthetic subprocess tests run; source evidence establishes only provider field names.
- Checkpoint: auto — focused hook consumer tests pass with zero temporary failures.
- Rollback: revert code before local activation; no stored data migration exists.

### U2 — Qualify and document the interface

- Goal / requirements: R3–R4; executable native evidence plus runnable documentation.
- Dependencies: U1.
- Files: modify `docs/consumer-guide.md`, `tests/provider_contact_hook.py`, `tests/lifecycle_contact_probe.mjs`; extend `tests/rust/cli_shell_spec.rs` help checks if needed.
- Approach: extend the existing disposable Codex contact harness to deliver synthetic submit content and assert a safe comparison result. Document exact prompt shape, independent flags, unsupported Pi and acceptance limits. Reuse existing fixture processes; add no separate framework or application recipe file.
- Patterns: existing fresh-home loopback Codex fixture and real subprocess consumer tests.
- Tests: native synthetic submit text reaches consumer with observation ID and scope; CLI help exposes flag; full repository gate stays green, including v1 and reply behavior.
- Verification: focused tests and current full `sh tests/gate.sh` pass on an exact disposable candidate; docs describe only evidence-backed support.
- Runtime evidence: unverified until contained native fixture executes; real Claude/profile activation remains outside authority and is reported separately.
- Checkpoint: auto — documentation parses, native fixture and full candidate gate pass.

## Invariants, failure cases and confidence

O1: prompt/reply use one exhaustive tagged availability enum; only available carries text.
O2: text is never attached to ProviderEvent, HookOutcome, persistence records or diagnostics.
O3: content extraction cannot bypass admission/persistence checks or run consumers inside locks.

No persistent transition changes: existing admitted/confirmed events retain their native writes and emit content afterward; rejected/unconfirmed/legacy-unadmitted events retain existing native outcomes and emit no envelope. Concurrent/repeated events keep distinct delivery IDs and their captured source; no exactly-once claim. Tests reuse persistence-rejection and rotation fixtures with prompt content. Cross-axis hazards are content overflow masking another field's availability, and content leaking on native/consumer failure; explicit tests cover both.

Disconfirming evidence: a non-string emitted as available, sentinel text in state/diagnostics, a consumer executing for unconfirmed persistence, or incorrect native callback text fails the build. A blocked native fixture is unverified, never passed. Missing text on an eligible callback means absent, not unsupported; unsupported actors/events never inspect text. These clauses cover the motivating correlation gap without turning a callback into acceptance.

## Scope, impact and risks

No stored text/digest/sequence/token, ownership flags, record schema change, Pi transport/configuration, controller implementation, new retries, GUI changes or live activation. Native and consumer failure meanings remain separate. Existing reply extraction is shared only at the string-validation boundary; event applicability remains independent. Privacy risk is addressed by no Debug implementation and executable state/diagnostic checks. Historical plans remain untouched.

## Build execution contract

- Closed: exact flags/fields and availability meanings above; Pi unsupported; consumers own acceptance.
- Builder autonomy: local helper/test organization and synthetic fixture details; record choices separately. No new approval needed for this authorized build.
- Verify at contact: actual provider fields and installed pinned Codex fixture; if native execution is unavailable, retain source/synthetic qualification and explicitly report the missing proof rather than claiming it ran.
- Stop only for an invariant that cannot be preserved within scope or required effects outside granted authority; continue independent work.
- Authority: original checkout, gated local commit, synthetic fixtures/disposable candidate only. No push/PR, live GUI/panes/configuration, real credentials or provider spend. No implementation in recipient repositories.
- Gate map: U1 focused tests; U2 help/documentation, native contact and full repository gate; no permitted assertion failures. Run /gate lenses before commit separately from command checks.
- Human inventory: no unresolved input. User authorized architecture then build; synthetic content proves the mechanism, leaving live profile registration and real Claude callback execution unverified.
