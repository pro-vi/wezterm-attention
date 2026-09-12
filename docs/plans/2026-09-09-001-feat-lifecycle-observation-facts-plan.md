---
title: Lifecycle observations for Attention consumers
objective: Downstream terminal extensions can distinguish agent requests and outcomes without changing Attention's existing badges or controlling the agent.
type: feat
status: completed
date: 2026-09-09
updated: 2026-09-10
origin: conversation
---

# Lifecycle observations for Attention consumers

**Ready for build preflight; not implemented.** This revision keeps **18 lifecycle rows, nine with Pi support**, in eight build units. It supersedes the 21-row working scope from the [historical hook map](../reviews/2026-09-08-attention-hook-map.md): H13, H14 and H21 are deferred; Pi input stays; H12 has no native Pi support. **Later** remains out. A build still requires a separate request.

The change adds one binding-scoped lifecycle snapshot with separate request and general retention budgets. It explicitly distinguishes blocking questions from nonblocking follow-ups so a consumer can choose a different pane appearance while a published follow-up needs its attention. It leaves activity, acknowledgement, child presence, and rendering policy in their existing records. Consumers can read request evidence after dismissing a badge, without treating a tool return as a human answer.

## Existing code, new evidence, proposed work

| State | What is established | What it does not establish |
|---|---|---|
| **Existing code** | On 2026-09-09, branch `feat/attention-v2`, HEAD `c9cc3e1d9906a3a3a4d70ac4bae9c9752a0c3b5e`, has the committed Rust rewrite. The worktree was clean before this plan. Rust writes v2; Lua reads it; Pi has a queued CLI bridge. | A source implementation is not proof that live provider registrations call it. This turn did not audit or change live registration. |
| **Existing code** | `activity.json` controls badges. An exact `ack.json` hides an eligible activity event. Claude root Stop leaves children; Codex root Stop hides earlier child work; Pi `agent_settled` stops, while `agent_end` is inert. | Stop does not prove success, session exit, or absence of an outstanding question. Focus does not answer a request. |
| **Historical runtime evidence** | Pi 0.84.4's actual ExtensionRunner delivered `ui_prompt_end` before `ui_prompt_start` when an earlier extension delayed the start handler. Both events lacked a request ID. The test used synthetic extensions and UI promises, not a model. | This supports deferring H13/H14. It proves a possible ordering failure, not its frequency; it is not an active UI feature or build obligation. |
| **Source-verified in this revision** | At Pi 0.80.5, all retained subscriptions and required payload fields exist, including input, tool results, assistant outcomes and compaction attempt/success. The three removed callbacks were the only new version-dependent subscriptions. | Source/type checks do not prove the revised extension runs on that older runtime. U4 requires a baseline compatibility exercise; no production version parser is added. |
| **Source-verified in Codex 0.154.0** | Both async user tools pass through native tool hooks. `request_user_input_async` publishes a question and returns accepted=true without waiting; the queue heading also appears for pending questions. | Source commit `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` was read by Luna and checked by the parent. No combined async-tool/native-hook runtime test was executed. An answer becomes ordinary user input without the original call ID. |
| **Proposed here** | A typed snapshot with request/general pools, exact native correlation where supplied, additive cached access, passive provider subscriptions, retention integration, and executable coverage of the 18 retained rows. | No universal pending-request count, answer-received flag, replay API, screen reader, or provider controller. Wrapper-free execution identity remains a contact proof, not an assumption. |

**Planning tier: Deep.** This changes a persisted cross-language contract and consumes concurrent external callbacks. The repository's pinned herdr, orca, luvus, and T3 research is sufficient for the design choice; targeted contact checks, not another broad survey, settle the remaining availability claims.

The [2026-09-08 consumer plan](2026-09-08-001-feat-attention-view-consumers-plan.md) keeps its puppet writer, bootstrap Relay migration and live activation work. **This plan's U2 owns its getter projection; U8 owns the shared consumer documentation.** The older plan records that transfer and must not reimplement those two pieces. Exposing the cached `puppet` value does not implement its writer-side provenance requirement. No complete-build dependency on the older plan is introduced.

## Requirements

| ID | Required outcome | Completion evidence |
|---|---|---|
| R1 | Cover the 18 retained rows at the narrowest native meaning actually supplied. | Exact active set H01–H12 and H15–H20 reaches the reader through production adapters; excluded callbacks are rejected or unregistered. |
| R2 | Keep provider, session, launch, binding, actor, and native correlation distinct. | Wrong launch, sibling child, reused session, malformed identity, and missing-ID cases cannot be attributed by a convenient fallback. |
| R3 | Preserve tool identity and observed results without storing tool arguments or output. | Preflight, result, error, and automatic-denial fixtures retain only the allowlist. |
| R4 | Preserve submission, response completion, settling, attempt failure, and interruption as different observations. | Failure followed by retry does not end the binding; provider Stop policies remain unchanged. |
| R5 | Preserve approval, question, elicitation and notice observations without asserting an answer or decision that was not exposed. | Exact correlation and narrower outcome distinctions survive the full pipeline; no native Pi question outcome is claimed. |
| R6 | Preserve provider context-compaction attempt and success without changing settled state. | Attempt, cancelled-before-hook and success fixtures leave lead and child policy intact; compaction failure/abort support is excluded. |
| R7 | Retain child ownership without promoting child work into lead activity or inventing a Pi child contract. | Same tool ID in two children stays separate; malformed child identity never becomes lead. |
| R8 | Let consumers read lifecycle facts and exact badge acknowledgement independently, from a detached cache-only value. | Dismissed badge plus retained request evidence is visible; mutation of returned nested values cannot alter the cache. |
| R9 | Bound storage and hot-path work, protect request evidence from generic traffic, and survive partial writes/reconnect without resurrecting evicted evidence. | Independent pool count/byte/floor tests, bounded-read, equal-time retention, cache-scope and crash checks pass. |
| R10 | Preserve v1/v2 badge behavior, existing public values, hook failure policy, and provider control boundaries. | Legacy regression, stdout, side-effect, installed-WezTerm, and frozen-reader tests pass. |
| R11 | State baseline compatibility and coverage by evidence level; verify wrapper-free identity before claiming it. | A contact report distinguishes synthetic dispatch, installed runtime dispatch, native emission, registration, and execution-identity proof. |
| R12 | Deliver one reproducible local gate and consumer contract without changing live setup. | Every requirement maps to a passing local exercise or an explicitly held operator-dependent claim; docs name the remaining limits. |
| R13 | Consumers can distinguish a nonblocking question publication from agent execution and choose a separate follow-up appearance without asserting an answer or changing bundled badges. | U2 exposes question mode/publication IDs; U8's consumer fixture tints after publication, survives Stop, and dismisses only its own presentation. |

## Naming ledger

Names below are proposed unless marked reuse. GR6 asks whether a nearby existing name is the same responsibility; the last column records that decision.

| Role / meaning | Existing term | Chosen name and owner | Status and reason for a boundary | GR6 sibling disposition |
|---|---|---|---|---|
| Native callback plus its existing display action | `ProviderEvent`, `ProviderAction` | Reuse in `src/providers.rs`; attach a typed optional observation and an observation-only action | Reuse; lifecycle dispatch and provider fixtures consume it | Do not rename the existing action enum into the richer fact model. |
| One allowlisted lifecycle observation | None; current event fields lose the detail | `LifecycleObservation`, `src/observations.rs` | New; protocol validation, provider normalization, and snapshot reduction need one typed meaning | Not another `Activity`; observation does not imply a badge. |
| Bounded latest-per-key evidence for one binding | Separate activity and child records | `LifecycleSnapshot`, `lifecycle.json` | New record; writer, Lua reader, and maintenance share it | Not a historical-binding catalogue or replay log. |
| Native identifiers and their namespaces | `agent_id`, provider session ID | `NativeCorrelation` inside an observation | New boundary type; strict native parsing and relation tests need missing values to remain missing | Observation UUID is not a native tool, turn, or request ID. |
| Question execution contract | Existing tool class distinguishes purpose only | `QuestionMode` (`blocking`, `nonblocking`) in `src/observations.rs`; classified in `src/providers.rs` | New closed field on both tool phases; writer validation and Lua consumers need the same meaning | Mode is required iff tool class is question; no mode for permission/generic tools. It is not current agent state. |
| Request facts and their narrower outcomes | None | Extend `RequestEvidence` in `plugin/reader.lua` with `question_mode` and `publication_observation_ids` | Reuse the planned projection, not a second question-state service | Question publication may stand alone without Pre; it is disjoint from tool-return/answer evidence and unrelated to tty/marker `publication_id`. |
| Consumer appearance for a published follow-up | Planned consumer fixture | Consumer-local follow-up tint and dismissal, in `tests/fixtures/lifecycle/consumer.lua` and the guide | Reuse the planned example; no production renderer module or durable dismissal record | Display dismissal is not badge acknowledgement or question resolution. |
| Which activity badge was dismissed | `acknowledgement` / `ack.json` | Reuse; expose as `lifecycle.badge_acknowledgement` | Reuse the existing exact-target record; no new writer, file, or command | Do not create `LifecycleSeen`, per-request read receipts, or a second focus state machine. |
| Public lifecycle data | `AttentionView`, `get_attention_view` | Reuse getter; add `lifecycle` | Additive facet; existing GUI consumers can opt in | Not a new reader package or a replacement `bindings` command. |
| Retention boundary within one evidence class | Child retention floor | `ObservationPool` at `pools.requests` and `pools.general`, each with `retention_floor_mono_ns` | New nested shape; reducer, Lua reader and maintenance validate two independent windows | Reuse the floor-before-removal invariant; keep `agents-floor.json` separate. No aggregate lifecycle floor. |

Question mode is classified once with provider/tool name and tool class. Validators and the public projection reuse this meaning through the manifest and fixtures; consumers do not need their own Codex tool-name lookup. The existing badge mapping remains a separate policy and must not derive new colors from question mode.

Pool names are storage classes, not priorities or proof of human waiting. `ToolClass` is reused on both tool phases; one provider-specific classifier assigns it from required native tool metadata. One exhaustive placement function in `src/observations.rs` maps every admitted kind/class to its pool. Lua/Python mirror that boundary rule through shared fixtures.

Only `src/observations.rs` is a new production module. It owns the typed observation model and pure snapshot reduction; it performs no IO, locking, provider decisions, or rendering. Existing modules retain those responsibilities. This separation is required to test concurrent reduction independently of callbacks, not to create a generic event framework.

## Architecture decision

**Approach:** keep display records stable; add one bounded typed snapshot beside them. Normalize the 18 retained rows through the existing CLI authority. Read the snapshot through the existing Lua poll and expose it independently of the badge projection.

**Why:** enriching `activity.json` itself changes its semantic equality. Current Rust deduplication excludes timestamps and event ID, then compares the remaining activity fields. A new request ID there can mint a new badge event and defeat focus acknowledgement. Separate storage lets facts change while the badge remains unchanged.

**Trade-offs:** there is one extra bounded snapshot read for the selected binding per normal reader poll. Each pool retains a bounded coalesced window, not all history. Generic tool traffic cannot spend the request pool's count or byte budget, but request traffic can still evict older requests. Missing native identity, reordered callbacks, eviction, and unsupported hooks limit what consumers can conclude. Older maintenance code may preserve the unfamiliar file and refuse to prune that binding; safe rollback is not transparent cleanup compatibility.

**Approval accepts:** the proposed architecture for all 18 retained observations, its evidence limits and resource bounds, and unchanged display behavior. Implementation requires a separate build request. Approval does not establish that requests are answered, hooks are active, or wrapper-free attribution works. It authorizes no live registration, provider spend, commits, or deployment.

### A question, a dismissed badge, and a returned tool

This is a proposed **blocking** Codex `request_user_input` example. Q1 and E1 are stand-in IDs for an admitted question with a supplied native tool ID. It does not claim the installed question path was exercised.

| Observed step | Existing badge behavior | New consumer facts |
|---|---|---|
| 1. Question preflight Q1 | Notify badge E1 appears | Question preflight Q1 was observed. No result is recorded. |
| 2. User focuses the pane | E1 is acknowledged and hides | The question observation remains. Badge E1 was dismissed. No answer is inferred. |
| 3. Matched question tool Q1 returns | The new result observation does not redisplay E1 | Tool Q1 returned. This is narrower evidence than confirmation that a human answered. |

### Nonblocking follow-up and consumer tint

**Proposed consumer example, not an implemented pane-color feature.** Q1 and P1 are stand-in native-call and publication-observation IDs. The example consumer uses a distinct appearance to say **a follow-up was published and has not been dismissed by this consumer**. It cannot certify that the user has not answered elsewhere.

| Observed step | Independent Attention facts | Example consumer appearance |
|---|---|---|
| Async question PreToolUse | `question_mode=nonblocking`; attempt only | Keep base appearance; an attempted call may be blocked or invalid. |
| Successful async PostToolUse | Publication P1 observed; no answer evidence | Use follow-up tint even while ordinary activity says thinking. |
| Stop after publication | Lead turn stopped; publication remains | Keep the follow-up tint. Stop does not resolve it. |
| Publication arrives after Stop | Published question becomes known while lead activity is stopped | Start the follow-up tint; publication need not precede the Stop observation. |
| Ordinary user input, including a reply | New submission; original question ID is absent | No automatic resolution. That input alone does not remove the tint. |
| Focus acknowledgement or consumer dismissal | Focus may acknowledge a badge; consumer dismissal does not change Attention facts | Focus keeps its existing badge behavior. An explicit consumer dismissal removes only its own tint for P1. |
| Another question Q2 is published in the same pane | Different exact publication ID | Q2 can tint independently of dismissed Q1. |

These rows are cases, not a required event order. Post can arrive before Pre, and a reply can arrive before or after Stop. A publication-only entry must be usable immediately. An async free-form message is not automatically a question. Missing/cached/invalid or evicted evidence is not a verified absence of pending follow-up work.

### Module responsibilities

| Existing or proposed module | Responsibility in this change | Must not take ownership of |
|---|---|---|
| `src/main.rs` — CLI entry | Keep the existing nested `hooks event` interface and failure modes | Native policy decisions, locks, a new consumer query command |
| `src/providers.rs` — native adapters | Parse identity strictly; select allowlisted observations and the existing legacy action | File IO; inferred answers; guessed child ownership |
| **New** `src/observations.rs` — typed evidence and reduction | Closed union, per-key merge, retention floor, structural invariants | Provider execution, terminal IO, source-order invention |
| `src/lifecycle.rs` — guarded mutation | Resolve admission, hold existing locks, re-read binding, apply legacy action and snapshot delta | A new session controller or a generic rewrite of all command paths |
| `src/records.rs`, `src/protocol.rs` — storage boundary | Atomic replacement, bounded reads, manifest validation, path identity | Multi-file transaction claims |
| `src/maintenance.rs` — audit and retention | Recognize the new file, preserve future/invalid records, prune only under existing binding rules | Ending an agent because a request or compaction observation looks finished |
| `pi/index.ts` — Pi bridge | Select native metadata at callback entry and enqueue a typed writer request using the 0.80.5 baseline API | Runtime version parsing, provider decisions, SDK-only lifecycle APIs |
| `plugin/protocol.lua`, `plugin/reader.lua` — validation and cached facts | Validate the mirrored wire shape, read exact scope, derive evidence relations | Guessing UI state from last callback arrival |
| `plugin/runtime.lua`, `plugin/overlays.lua` — public cache and focus | Copy the lifecycle facet; expose existing badge acknowledgement | New focus subprocesses or per-request seen writes |

### Record placement and write order

This is a file-tree change, not a call sequence. Existing siblings remain unchanged.

```diff
 launches/<launch>/bindings/<binding>/
   binding.json
   activity.json
   ack.json
   activity-clear.json
   end.json
+  lifecycle.json
   agents-clear.json
   agents-floor.json
   agents/<agent-key>.json
```

One admitted native callback is processed in this order:

1. Parse the native envelope and strict ownership fields. Capture the writer's existing incarnation-scoped monotonic coordinate and wall timestamp once. Preserve native ordering metadata separately if available.
2. Resolve the launch and provider binding; distinguish a matching inherited launch claim from tty-only recovery. An observation requires the current matching binding. Never write rich facts into the unbound launch activity path.
3. Under the existing launch lock, re-read the claim and selected binding. Validate the whole existing snapshot, classify the candidate, and reduce only its pool. Preflight observation, pool and full serialized byte limits before writing that snapshot. A future/corrupt snapshot is not two empty pools.
4. Apply the independently valid existing legacy action using its existing ordering and projection rules. Then replace `lifecycle.json` atomically with both pools and both floors if the candidate changes its bounded state. A rejected rich candidate may leave an independently valid legacy action in effect, with a partial-result diagnostic.
5. Report each committed or rejected part through existing diagnostic/result handling. Default hook mode remains fail-open and quiet; strict mode reports incomplete work. A failed observation write must not be described as full success.

`commit_with` currently applies replacement files sequentially. The launch lock serializes cooperating writers, not unlocked Lua readers. There is no atomic transaction spanning activity and lifecycle. A crash can leave a new badge with an older snapshot; failure after rename can also leave a new snapshot visible despite an error; the API exposes them independently and never joins them by timestamp. Retrying a callback may repair the missing observation; without a trustworthy native or transport ID, duplicate evidence is possible and bounded.

## Typed record and read contract

### Runtime-validated manifest and strict types

`protocol/v2.json` remains the wire authority, embedded by Rust and loaded by Lua and the Python fixture checker. Keep all existing record shapes, wire version 2, and existing schema-2 interpretation unchanged. Add one `lifecycle_snapshot` record kind, explicitly named nested shapes, closed discriminants, and the limits below. Extend only the bounded object/array/union validation those shapes need; do not implement a general JSON Schema engine or embed JSON strings inside JSON.

Rust's typed union must round-trip every allowed variant through the manifest validator. Lua and the independent Python checker must accept and reject the same fixture rows. A fixture checks every new enum and limit against Rust's constants; same-language users import the owning type rather than repeat strings. The compatibility exercise must verify old core readers with the new additive manifest before this versioning choice is accepted.

The frozen compatibility targets are explicit: old Lua core modules reading the new on-disk manifest plus valid old records/new sidecar; and an old Rust binary carrying its original embedded manifest. Old Rust source compiled against the new manifest is a different, unsupported combination: its strict manifest structs can reject new fields. Freeze source and embedded manifest together when producing baseline binaries. Older maintenance may preserve the unfamiliar sidecar and refuse pruning; that is the rollback limit, not proof of transparent compatibility.

The snapshot envelope requires existing full `address`, `launch_id`, `binding_id`, `provider`, `schema`, and record kind, plus `snapshot_id`, `written_at_unix_ns`, and `pools`. Both `pools.requests` and `pools.general` are required `ObservationPool` objects: an `observations` array and an optional `retention_floor_mono_ns`. There is no top-level observation array or scalar floor. Each observation has UUID `observation_id`, kind, exact native `source_event`, optional safe `source_version`, `observed_mono_ns`, `written_at_unix_ns`, actor, optional native correlation, and only its variant's fields. Source version is passive metadata when already available; it requires no Pi VERSION import or parser.

Unknown fields, explicit null, wrong scalar types, controls and over-limit values are invalid. Validate both pools together, including correct membership, no duplicate retained identity, and no member at or below its own floor. One malformed pool invalidates that snapshot; never recover the other as a fresh generation or reset the malformed floor.

Bound the lifecycle file read at 262,144 bytes plus one overflow-detection byte before parsing in Rust, Lua and the test checker. The lifecycle grammar also caps container nesting at eight levels before recursive parsing. Do not route it through Lua's existing unbounded `read("*a")`. Extend the existing raw JSON validation to preserve array/object token distinctions before decoding: `[]` is an empty observation array; `{}` is not. Installed-WezTerm fixtures must pin empty/nonempty arrays, object-shaped collections, nested null, sparse/map shapes and excessive nesting. No general-purpose schema engine is introduced.

Actor is a closed lead/child choice. Child requires the existing canonical child identity and digest relationship. Session and binding are inherited from the validated envelope, not repeated independently in every observation. Native correlation carries separately optional `turn_id`, `tool_call_id`, `elicitation_id`, and `message_id`; the adapter records only fields actually supplied by that native source. A supplied malformed identifier invalidates the observation. Omission remains omission.

### Closed observation union

These are contract names, not implementation declarations. `source_event` and the provider retain the exact native origin. Optional fields are absent when the source does not expose them; there is no arbitrary metadata object.

| Kind | Variant fields | Meaning and excluded inference |
|---|---|---|
| `prompt_submitted` | Optional allowlisted input-source label | Submission observed; interception or transformation may follow. No prompt text. |
| `tool_preflight` | Safe tool name and tool class; `question_mode` required iff class is `question` | Preflight attempt only. Mode is the verified native tool contract, not proof that it currently blocks on a human. |
| `tool_result` | Same tool name/class/question mode as preflight; optional boolean `is_error`; result surface `success_hook`, `post_hook`, or `execution_end`; optional documented interruption flag | Nonblocking question success means publication; blocking question return is narrower tool-return evidence. Neither proves a human answer or task success. |
| `approval_requested` | Optional safe tool name | Permission handling requested. No universal native request ID or human waiting claim. |
| `automatic_denial` | Safe tool name; automatic-policy scope | Claude auto-mode denial only. Not a manual decision. |
| `response_finished` | Optional supplied continuation/stop-hook-active boolean | Claude/Codex Stop observed; other hooks can continue the turn. |
| `run_settled` | No message contents | Pi's outer run settled after automatic continuation, not process exit or task success. |
| `attempt_outcome` | Outcome `failed` or `aborted`; optional allowlisted native error category | Narrow Claude turn-error or Pi assistant outcome. No raw error message; abort cause remains unknown unless explicitly supplied. |
| `user_interrupt` | Supplied turn identity only | Codex's reported active root-turn interruption, not a general provider-independent cancellation. |
| `elicitation_requested` | Supplied mode enum; optional native correlation | MCP request observed. No form schema, URL, prompt, or contents. |
| `elicitation_action_selected` | Action `accept`, `decline`, or `cancel`; optional native correlation | An action selected before send. Not server receipt. |
| `notice` | Subtype `permission_prompt`, `elicitation_dialog`, or `elicitation_url_dialog` | Only the approved notification subsets. No notification text and no guessed request match. |
| `compaction_attempted` | Optional allowlisted trigger/reason | Provider context-compaction attempt; a before hook can cancel it. |
| `compaction_succeeded` | Optional allowlisted trigger/reason | Native compaction success observed; no task-success or settled-state implication. No failure/abort/retry fields are reserved in this active shape. |

Exact provider enums and selectors must be checked against the pinned payload sources at contact and added to the executed manifest. A new native value is not accepted as an arbitrary string. The safe fallback is to omit an absent optional descriptive field or reject an unsupported observation with a diagnostic, never to invent a supported value. A malformed ownership/correlation field is not treated as merely absent.

### Question mode and async publication contract

| Provider and actual hook tool name | Tool class / question mode | Meaning of a successful tool result |
|---|---|---|
| Claude `AskUserQuestion` | `question / blocking` | The question tool returned; cancellation, automatic handling and transformed results still limit human-answer inference. |
| Codex `request_user_input` | `question / blocking` | The blocking question tool returned. |
| Codex `request_user_input_async` | `question / nonblocking` | The question was accepted for publication, without waiting for a reply. |
| Codex `send_message_to_user_async` | `generic`; mode absent | A free-form message was accepted. It may be an update rather than a question. |
| Pi addon tool names | `generic`; mode absent | Generic tool result only; no native Pi question classification. |
| Permission and other generic tools | Existing class; mode absent | Existing narrow result meaning. |

The Rust type construction and manifest refinement must enforce: **question mode exists iff tool class is question**. Reject missing, null, unknown or forbidden modes and incompatible provider/tool/class/mode combinations in Rust, Lua and Python. Derive mode from the native provider/tool map; do not accept a caller-supplied label without validation. Mode is immutable tool metadata, checked on same-key retries and cross-phase correlation; it is not another component of the storage key.

The publication predicate is narrow: validated Codex `PostToolUse` for `request_user_input_async`, normalized as a successful nonblocking question `tool_result` with `result_surface=post_hook` and no error/interruption indication. Validate the supported async success receipt at the native boundary, using only the needed acceptance metadata; discard the raw output. A rejected, errored, malformed or unsupported receipt creates no publication. If the supported success receipt cannot be validated, the adapter emits no admitted nonblocking question `tool_result`; it reports rich-evidence rejection while preserving independently valid legacy behavior. Whole-snapshot validation rejects contradictory nonblocking question results carrying an error/interruption indication. No raw output or acceptance payload is retained. A generic post hook, Pi execution-end, Stop, user interrupt, or non-question async message cannot satisfy this predicate. Publication means the provider emitted the question; it does not prove GUI rendering or human attention. The synthetic hook exercise must pin the exact 0.154.0 receipt shape before relying on it.

The 0.154.0 async question tool is root-agent-only; a child-attributed async-question tuple is not a supported publication origin and is rejected as rich evidence. This does not promote the child event into lead activity.

The actual async hook tool name is `request_user_input_async`. `send_user_message_async` is an older model-catalog advertisement that selects the same handler in 0.154.0, not a verified extra emitted hook name. No runtime Codex-version parser or model-catalog lookup is added to Attention; observe the admitted native tool event. Root-agent/model availability remains a provider concern.

The answer path supplies no machine-identifiable answer wrapper to `UserPromptSubmit`: its prompt text contains an ordinary Markdown quotation of a title capped at 512 bytes plus the user's input. The internal `user.answered_question` metadata is not passed through that text path. This change adds **no answer-wrapper, title-matching, terminal-heading, or conversation-order parser**. Ordinary input stays ordinary submission evidence.

### Bounded storage, ordering, and duplicates

The two pools are required objects in **one** `lifecycle.json` snapshot. These are proposed resource bounds, not latency measurements.

| Budget | Request pool | General pool | Whole snapshot |
|---|---|---|---|
| Observation count | 64 | 64 | At most 128 |
| Compact UTF-8 serialized bytes, including each pool's array/object punctuation and floor | 122,880 | 122,880 | 262,144 including the final newline |
| Envelope budget excluding the two serialized pool values | No borrowing | No borrowing | 16,384 |
| Retention floor | `pools.requests.retention_floor_mono_ns` | `pools.general.retention_floor_mono_ns` | No combined floor |

One observation may use at most 2,048 compact serialized bytes. Keep existing safe-label limits. The two pool maxima plus the envelope maximum equal the file cap. Full-file overflow is a validation failure; it must never trigger eviction from the other pool.

**Byte accounting:** charge compact JSON with literal UTF-8, quotes/backslashes escaped, comma/colon/bracket/object punctuation included, and no whitespace. Object-key order does not affect length. Strings with controls are already invalid; JSON numbers in this shape use their validated canonical integer form. Rust's writer measures its actual compact serialization and final newline. Lua/Python compute the same defined cost from the bounded validated shape; do not use a runtime encoder's optional Unicode/slash escaping as the authority. Separately enforce the raw file cap before decoding, so alternate escape spellings or whitespace cannot cause unbounded allocation. Shared fixtures pin UTF-8, escaped equivalent spellings, punctuation, maxima and newline overhead.

**Pool placement is exhaustive and independent of retained history:**

| Validated observation | Pool | Rule |
|---|---|---|
| `approval_requested`, `automatic_denial`, `elicitation_requested`, `elicitation_action_selected`, and the three admitted `notice` subtypes | `requests` | Preserve request-related evidence even when no native match is possible. Membership does not assert current waiting. |
| `tool_preflight` or `tool_result` with class `question` or `permission` | `requests` | Both phases use the same provider classifier, even if a result arrives before its preflight. |
| `tool_preflight` or `tool_result` with class `generic` | `general` | Pi addon tools stay generic; do not guess questions from their names. |
| `prompt_submitted`, `response_finished`, `run_settled`, `attempt_outcome`, `user_interrupt`, `compaction_attempted`, `compaction_succeeded` | `general` | No request-pool promotion from temporal proximity or later events. |
| Missing required tool name, malformed class, unsupported kind, or wrong stored pool | Neither | Reject the rich observation/snapshot as applicable; no fallback class or pool migration. |

The fixed provider classifier uses the question-mode table above plus the existing permission names. Other valid names are generic; missing is not generic. Codex 0.154.0 async routing is source-verified and requires the U7 synthetic native-hook exercise before runtime coverage is claimed. Tool name, class and question mode are immutable metadata. A retained same-native-key retry that contradicts them is rejected; no entry moves between pools or becomes a second identity. This is bounded duplicate protection, not authentication or global history.

For each admitted observation, derive its storage key from kind, actor and trustworthy native correlation namespace. Tool keys use native tool-call ID and supplied turn scope; tool name/class/question mode are immutable metadata checked against any retained same-key entry in either pool. Other keys use source-supported elicitation/message/turn identity. Without trustworthy native correlation, use the local observation UUID. Do not collapse ID-less requests into one `unknown` entry. Request and result kinds remain separate entries. Relation matching also checks actor, tool identity, question mode and supplied native scope; pool membership alone never establishes a relation.

Check the candidate's own pool floor before deduplication or refresh. An identical semantic retry keeps the retained observation ID/timestamps. Older changed content loses; unequal content at an equal coordinate is a conflict. A later valid change updates only that key. A local Pi transport UUID may identify retry of one queued item, never a provider request.

When its count or byte budget is exceeded, remove the candidate pool's oldest whole `observed_mono_ns` groups until both bounds hold. Advance only that pool's floor to the greatest removed coordinate. Reject later receipts at or below that floor. An equal-time group may remove every member, including the candidate; report the bounded omission. An over-limit single observation is rejected before either pool is reduced.

**Isolation invariant:** an admitted general observation leaves the request pool and its floor value-equal; an admitted request observation leaves the general pool and its floor value-equal. Envelope snapshot ID/write time may change. No borrowing, cross-pool eviction, aggregate floor, or later reclassification. Snapshot replacement contains both resulting pools and floors atomically; a failed post-rename fsync requires rereading the actual file before retrying.

Store each pool in ascending `(observed_mono_ns, observation_id)` order; the public flat list uses the same ordering across pools and labels each member's pool. This is deterministic receipt order, not a native causal trace.

Receipt clocks order storage, not native causality. Result-before-request delivery stays valid. Unresolved evidence is not protected forever: further request evidence can evict old requests or their partners, and this is visible through that pool's floor. Coverage remains a bounded window, never a complete pending roster. No new TTL, expiry timer, periodic process scan, retention daemon or changes/replay API is added.

**Illustrative retention case, not a product runtime result:** retain question Q and its matched result in `requests`, then accept 64 distinct generic tool calls with preflight and result. The general pool evicts its own old entries; Q and its result remain unchanged. Repeating the experiment with large general entries must have the same outcome under byte pressure.

### Independent public facts

The proposed `lifecycle` facet comes from the existing poll and is returned as a detached table by `get_attention_view(pane)`. Its availability is `available`, `cached`, `absent`, `unavailable`, `invalid`, or `unsupported`.

It exposes the snapshot ID when present, a detached flat observation list with each observation's pool, separate `retention_floors.requests` and `retention_floors.general` when present, `coverage=bounded_window`, derived request evidence, and a valid exact badge acknowledgement. No aggregate floor is exposed. Expose at most eight lifecycle diagnostics under the existing bounded convention. Distinguish missing data from a failed read.

`RequestEvidence` groups only exact supported native identities within the same full binding and actor. Existing result/selection relations remain `tool_result_observed`, `automatic_denial_observed`, or `elicitation_action_selected`. Missing IDs and incompatible tool/mode/scope yield separate unpaired evidence; never join by name alone, title, timing, array position or the visible terminal heading.

A native async call may publish several questions; evidence here is call-level publication, not a count of unanswered questions.

For question entries, expose `question_mode` and a detached `publication_observation_ids` array alongside the existing request/result/selection evidence. Only the narrow nonblocking publication predicate populates that array. The same async Post ID must not also be presented as a blocking-question `tool_result_observed` relation or answer evidence. For blocking questions, publication IDs are empty and a matching tool-return relation retains its narrower meaning. For non-question request entries, question mode and question-publication fields are absent.

A valid async Post can create a **publication-only entry**: `request_observation_ids=[]`, one or more publication IDs, and no result/selection relation. This holds when Pre was not observed, arrives later, or was evicted. Without native correlation, keep an independent observation-based entry; do not require an ID merely to recognize what one valid observation means. A later exactly matching Pre may attach to the group but cannot mint another publication ID. Duplicate delivery preserves the retained publication ID and timestamps under the normal reducer.

Publication records a past event, not a live pending-request lease. Stop, focus, generic submission and elapsed time do not turn it into answered or erase it. Per-pool eviction may remove it, with the existing bounded-window disclosure. Consumers receive enough explicit meaning to choose a follow-up appearance without reproducing Codex-specific parsing.

`badge_acknowledgement` identifies the activity event and exact target already written by `ack.json`. It means that badge was dismissed. It does **not** label each lifecycle observation seen, prove a question was read, or imply a response. No per-request `seen`, `answered`, `resolved`, `currently_waiting`, or `pending_count` field is introduced. Consumers may choose how to display these facts, but must distinguish their display policy from observed provider outcomes.

Valid facts remain readable after activity acknowledgement, activity TTL, activity clear, and a stopped lead response. They remain under the binding that produced them. When the binding pointer changes, old facts are not recovered into the new binding. Optional lifecycle corruption reports separate availability/diagnostics; it must not turn an otherwise valid legacy badge into an invalid binding or different color.

U2 also exposes the eight cached base fields already required by the earlier consumer plan: `activity_type`, `source`, `puppet`, `address`, `launch_id`, `marker_id`, `pane_presence`, and `binding_health`. Preserve the current eight fields: `provider`, `binding_id`, `binding_phase`, `type`, `event_id`, `subagents`, `review`, and `reader_confidence`. Copy the address's three components into a fresh table; preserve nil/false semantics. Root `_records`, raw diagnostics, cache keys, deadlines and formatter state stay private. This projection does not fix Rust's current `puppet=false` ingestion behavior.

The getter performs no IO, subprocess, clock read, mutation, or relation recomputation. The poll updates the data cache when facts change even if the rendered badge does not. Every nested object/array is detached on return. Positive and unavailable-read caches obey the same full address, launch, binding, and path scope as normal record recovery. Fresh successful reads always win over cached siblings.

Only unavailable IO may reuse the matching validated cached snapshot. A successfully read malformed snapshot returns `invalid`; a future unsupported shape returns `unsupported`. Neither is replaced in the public view by cached older facts, and neither causes one pool or floor to be reset.

## Approved row coverage

The active set is **H01–H12 and H15–H20: 18 rows**, of which **H01–H07, H19 and H20 have Pi support: nine rows**. Surviving H-IDs retain their original numbers. H13/H14/H21 are recorded in [BACKLOG.md](BACKLOG.md), not silently renamed or implemented. A cell names narrow intended support, not proof of live emission.

| Row | Approved lifecycle observation | Native source and proposed treatment | Units / decisive test |
|---|---|---|---|
| H01 | User submits a prompt | Claude/Codex `UserPromptSubmit`; Pi `input` → submission and available source/turn identity, no prompt | U3, U4 — intercepted input remains submission, not run start |
| H02 | Tool preflight begins | Claude/Codex `PreToolUse`; existing Pi `tool_execution_start` → tool identity. Do not additionally subscribe `tool_call` for the same purpose | U1, U2, U4 — preflight blocked before execution remains preflight |
| H03 | Tool produces an execution result | Claude/Codex `PostToolUse`; Pi `tool_execution_end` → exact source and result flag when supplied | U1, U4 — Codex post hook is not universally success |
| H04 | Tool failure is observed | Claude `PostToolUseFailure`; Pi `tool_execution_end.isError`; no invented Codex failure hook | U1, U4 — failure is not permission denial |
| H05 | Lead response finishes / agent settles | Claude/Codex `Stop` versus Pi `agent_settled`; retain context, preserve current stop/count rules | U3, U4 — same badge can coexist with distinct outcome evidence |
| H06 | Agent attempt fails | Claude `StopFailure`; Pi assistant `message_end` error only | U3, U4 — error then retry leaves binding active |
| H07 | Active attempt is interrupted or aborted | Codex `Interrupt`; Pi assistant `message_end` aborted only; Claude gap remains | U3, U4 — Pi abort does not identify the human as cause |
| H08 | Tool approval is requested | Claude/Codex `PermissionRequest` → separate approval evidence; absent request IDs stay absent | U2 — focus hides notify without erasing approval evidence |
| H09 | Permission prompt remains outstanding long enough to notify | Claude `Notification.permission_prompt` → delayed notice, not general wait-start | U2 — notice without structured ID remains unmatched |
| H10 | Auto-mode policy denies a tool | Claude `PermissionDenied` → auto-mode scoped denial and native tool identity | U2 — do not classify an ordinary manual denial from this hook |
| H11 | Agent asks a structured question | Claude `AskUserQuestion`; Codex `request_user_input` (blocking) and `request_user_input_async` (nonblocking). Normalize explicit mode; async publication is source-verified at 0.154.0 and exercised under U7. | U2, U7 — async Post-only publication is visible without claiming an answer |
| H12 | A structured-question tool returns | Matched Claude/Codex `PostToolUse`: blocking return and nonblocking publication have different derived meaning. **No native Pi H12.** Pi generic results remain H03/H04. | U1, U2, U7 — async publication IDs never become blocking-result or answer evidence |
| H15 | MCP server requests user input | Claude `Elicitation` → request mode and supplied correlation only | U2 — missing elicitation ID does not match by timing |
| H16 | MCP elicitation response is selected | Claude `ElicitationResult` → selected action before send | U2 — selection is not delivery |
| H17 | MCP form or URL prompt produces a notice | Claude `Notification.elicitation_dialog` and `elicitation_url_dialog` → notice subtype; new URL fact does not silently add a badge | U2 — old badge fixture unchanged for newly observed URL subtype |
| H18 | Child work is observed active | Claude/Codex attributed preflight/permission → preserve child ownership and tool/request metadata alongside current presence write | U1, U2 — sibling children with equal tool IDs never correlate |
| H19 | Context compaction begins | Claude/Codex `PreCompact`; Pi `session_before_compact` → attempted, not guaranteed started | U5 — cancelled before-hook attempt does not imply an active compaction |
| H20 | Context compaction succeeds | Claude/Codex `PostCompact`; Pi `session_compact` → success observation without settling | U5 — compact SessionStart metadata update does not move the observation to another binding |

## Representation ledger and program obligations

| Representation | Authority and necessary copies | Required equivalence check |
|---|---|---|
| Native payload selectors | Pinned provider contract plus executed synthetic/contact fixtures; `src/providers.rs` selects them | Every H-row names its provider fixture and allowed/forbidden fields. Unknown or malformed identity never becomes lead. |
| Observation semantics | Closed Rust types in `src/observations.rs`; executed manifest describes their serialized shape | Every union member round-trips; every enum/limit is asserted; no arbitrary metadata map. |
| Cross-process Pi requests | Typed `WriterRequest` variants derived from installed `ExtensionAPI.on` event types; Rust is receiver | Actual Pi callback → production queue/argv → real Rust child → record → Lua fixture. No cast-based type widening. |
| Pool classification and resource bounds | One exhaustive Rust placement function and manifest budgets; Lua/Python validate necessary mirrors | All 14 kinds/classes have pool fixtures; wrong pool, independent count/bytes/floors, UTF-8 and result-first cases agree. |
| On-disk record grammar | `protocol/v2.json`; Rust embed, Lua parser, Python test-only checker | Shared positive and negative JSON rows, including nested null/unknown keys, exact types, byte bounds, and future kind/schema behavior. |
| Filename/kind identity | `src/records.rs` path registry consumed or exhaustively checked by maintenance | New file recognized in read, doctor, audit, and prune tests; unknown siblings remain preserved. |
| Question mode and publication projection | One native classifier, closed Rust/manifest mode, reader-owned RequestEvidence | Mode/phase/provider parity; publication-only entries; disjoint publication/result references; consumer-local dismissal tests. |
| Public lifecycle facet | Lua reader projection with a documented allowlist | Detached-copy tests, request relation fixtures, and installed-WezTerm getter smoke. Never expose internal `_records` aliases. |

- **O1 — Typed boundary:** the normalized event stream is a closed discriminated union, implemented in U1. No `unknown[]`, unrestricted `Value` payload, or fields added ad hoc by each adapter.
- **O2 — Identity admission:** a rich observation is admitted only to the same current binding and an explicitly verified launch source. Existing tty presence/recovery alone is not upgraded into execution identity. U7's missing proof holds only wrapper-free rich admission and its activation, not synthetic/inherited-claim work.
- **O3 — Actor integrity:** child identity is checked before selecting lead/child action. Bad child input is rejected, never reclassified as lead; relation keys include actor.
- **O4 — Display independence:** newly recognized observation-only callbacks produce no legacy activity or acknowledgement change. Existing recognized callbacks keep their existing legacy action, deduplication, clocks, and child policies.
- **O5 — Partial validity:** malformed rich correlation cannot be persisted. An independently valid existing display action may still run with an explicit partial-result diagnostic; malformed ownership rejects both. Default hook mode must remain non-controlling and bounded.
- **O6 — Retention isolation and atomicity:** each pool has its own count/byte budget and floor; overflow cannot alter the other pool. Check the candidate's pool floor before deduplication. Write both pools and floors in one snapshot replacement; validate them as one generation. A malformed pool is not empty.
- **O7 — Evidence not causality:** receipt monotonic time orders storage only. Results may precede requests. Classify both tool phases from required native metadata using the same provider mapping; never migrate an observation between pools. Relations require supported native identity and actor/tool scope.
- **O8 — Time domains:** keep the accepted shared raw monotonic coordinate for writer ordering and existing Unix-wall TTL rules for existing records. Rust must not substitute process-relative `Instant` values. No lifecycle TTL is added; negative wall age is reported, not converted into an outcome.
- **O9 — Passive bridge:** callbacks do not return a provider decision, rewrite input/result, await model work, or copy sensitive payloads. New Pi callbacks use the existing queue and capture only typed allowlisted metadata before enqueueing.
- **O10 — Cache separation:** successful core and lifecycle reads are authoritative independently. No stale binding, acknowledgement, child stop, or retention floor is overwritten by whole-view recovery.
- **O11 — Focus meaning:** expose the exact existing badge acknowledgement even after it suppresses the badge. Do not call that per-request seen, response, or resolution. No new focus writer or polling subprocess.
- **O12 — Complete coverage:** fixtures enumerate exact active H-IDs H01–H12 and H15–H20 and all 14 union kinds. Compare them with the historical Next set minus explicit H13/H14/H21 exclusions. Removed callbacks and Later branches sharing subscribed hooks produce no new writes.

- **O13 — Bounded nested parsing:** bound lifecycle reads before parsing; retain raw array/object distinctions; validate nested child digests, unique retained identity, pool membership and per-pool floors. Compact byte costs must agree across Rust/Lua/Python fixtures.
- **O14 — Baseline Pi API:** retain peer support at 0.80.5 without VERSION imports or callback version comparisons. Use baseline-exported `ExtensionAPI`/`ExtensionEvent` types or callback inference; do not assume newer named event-type exports exist.

- **O15 — Question mode:** require mode iff class is question on both tool phases; validate provider/name/class/mode consistency and reject same-key or cross-phase contradictions. Async free-form messages and Pi addons remain generic.
- **O16 — Publication is not response:** derive publication IDs only from the admitted Codex async success receipt; allow Post-only/ID-less publication entries. Do not place the async Post in a blocking-result/answer relation. Stop/focus/submission cannot resolve it.
- **O17 — Consumer-owned appearance:** U8 demonstrates follow-up tint and scoped dismissal using the getter. Its private state must not modify lifecycle records, badge ack, shared cache or another consumer. No core pending boolean or text-based answer inference is introduced.

The integration shape is not assumed identical: current `ProviderAction` has nine coarse actions; `ProviderEvent` carries optional strings; current Pi `WriterRequest` cannot carry native tool/request/outcome identity. U1 adds the typed normalized observation and observation-only dispatch. U4 is the explicit Pi bridge unit. Actual installed runtime execution of the new bridge remains unverified until those units' exercises pass.

## State-action contracts

Each row states caller result, durable change, effects, and concurrency rule. These small matrices cover separate state axes; they do not claim every cross-product is valid. Tests named here are normative obligations of the owning unit.

### Admission and mutation

| Input × current state | Caller result and durable delta | Effects / race rule / test |
|---|---|---|
| Valid event × same inherited claim and binding | Accept observation; legacy action only if independently applicable | Re-read under launch lock; `same_claim_event_reaches_snapshot` (U1) |
| Valid event × different current launch or binding | Ignored with exact scope diagnostic; no new fact or projection | A delayed writer cannot follow the newest pointer; `old_launch_cannot_write_facts` (U1) |
| Missing inherited claim × tty-only recovery | Existing legacy contract unchanged; rich admission held unless U7 proves the source | No latest-claim guess or self-mint from presence; `tty_presence_is_not_execution_identity` (U7) |
| Child field malformed × otherwise valid lead payload | Reject ownership; no lead or child write | Fail before target selection; `malformed_child_identity_never_becomes_lead` (U1) |
| Valid legacy event × malformed optional rich correlation | Run only independently valid legacy action; diagnostic marks rich rejection | Never persist a partially decoded fact; `rich_rejection_preserves_legacy_contract` (U1) |
| Valid event × future/invalid existing snapshot | Preserve snapshot; report rich unavailable/invalid; independently valid legacy action may continue | No replacement with an empty snapshot; `future_snapshot_is_preserved` (U6) |

### Snapshot reduction

| Candidate × retained state | Caller result and durable delta | Effects / race rule / test |
|---|---|---|
| Same supported key and equal semantic body | Duplicate; keep observation ID and times | No event refresh or redisplay; `correlated_retry_is_stable` (U1) |
| Different request IDs × equal badge | Retain both facts; legacy dedup stays unchanged | No join by badge event ID; `distinct_requests_with_equal_badges_remain_distinct` (U2) |
| Same key × older or equal conflicting receipt | Ignore older; diagnose equal conflict; keep current entry | Lock order cannot choose truth; `same_key_order_and_conflict` (U1) |
| New receipt × below/equal its pool floor | Ignore as outside that pool's retained window; no mutation | The other floor cannot block it; `delayed_receipt_checks_only_its_pool_floor` (U1, U6) |
| Valid receipt × full own pool | Evict own oldest whole groups and advance only its floor in one snapshot replacement | No cross-pool count/byte pressure; `equal_time_eviction_is_pool_local_and_atomic` (U1, U6) |
| Oversized single observation × valid snapshot | Reject; keep both pools and both floors | Rejection does not evict good data; `oversize_fact_cannot_empty_snapshot` (U1) |
| Generic tool churn × retained request and outcome | Accept general facts within their own limits; request pair and request floor stay equal | Count and byte variants; `general_overflow_preserves_request_pool_and_floor` (U6) |
| Request churn × retained general evidence | Accept request facts within their own limits; general pool/floor stay equal | Symmetric isolation; `request_overflow_preserves_general_pool_and_floor` (U6) |
| Question result × preflight absent | Classify from provider/tool metadata into requests immediately | No later pool migration; `question_result_before_request_uses_request_pool` (U1, U2) |
| Missing tool metadata or class-changing retained retry | Reject rich candidate; no fallback class, duplication or migration | `required_tool_identity_and_class_are_stable` (U1) |

### Relations and acknowledgement

| Evidence × action | Caller result and durable delta | Effects / race rule / test |
|---|---|---|
| Request plus exact same-actor tool result | Derived `tool_result_observed`; no extra record | Match independent of arrival order; `result_before_request_correlates_exactly` (U2) |
| Same tool ID in sibling children | Two unrelated evidence sets; no durable change | Full actor namespace required; `same_tool_id_in_sibling_children_does_not_correlate` (U2) |
| Approval/notice without request ID | Unmatched observation; no durable change | No name/time match; `missing_native_request_id_remains_unknown` (U2) |
| Elicitation action selected | Derive selected-action relation only for exact supported ID | No delivery flag; `elicitation_selection_is_not_delivery` (U2) |
| User focuses a notify pane | Existing exact ack write; rich snapshot unchanged; API exposes both | Ack target remains activity, not request; `facts_survive_acknowledgement_without_redisplay` (U2) |
| A request/result partner was evicted | Retained evidence remains partial; no inferred answer or pending total | Relevant pool floor/coverage visible; `eviction_never_implies_resolution` (U6) |

### Async question evidence and consumer action

| Input × state | Caller observation and durable change | Effects / race rule / test |
|---|---|---|
| Blocking/nonblocking mode on both question phases | Valid typed observation; existing pool placement | Missing/unknown/forbidden mode rejects; `question_mode_shape_parity` (U1) |
| Same native key with contradictory mode, or incompatible Pre/Post modes | Reject same-key conflict or leave cross-phase evidence unpaired | No duplicate identity or false relation; `question_mode_is_immutable_tool_metadata`, `question_phase_mode_mismatch_does_not_correlate` (U1/U2) |
| Accepted async Post, with missing or later Pre | Publication-only RequestEvidence visible; normalized tool receipt stored | Native ID optional for single-event meaning; `async_post_without_preflight_records_publication` (U2) |
| Later matching Pre or duplicate Post | Group may gain Pre; publication ID remains stable | No new tint/notification identity; `async_post_before_preflight_keeps_publication_meaning` (U2) |
| Async Post then Stop, or delayed Post after Stop | Publication and lead activity remain independent | No fixed cadence required; `async_publication_survives_stop_in_both_orders` (U2) |
| Generic submission before/after Stop or publication, including quoted-title input | Submission remains uncorrelated; no question resolution write | No title/timing parser; `stop_focus_and_input_do_not_resolve_async_question` (U2) |
| Non-question async message, rejected/invalid result or preflight-only evidence | No question-publication IDs | No false follow-up tint; `non_question_async_message_is_not_question_publication` (U2) |
| Consumer dismisses displayed P1; later unrelated traffic/Stop and new P2 occur | Consumer-local display state changes only; P1 stays dismissed and P2 can tint | Full scope + publication IDs, never snapshot/pane ID alone; `question_tint_dismissal_is_consumer_local` (U8) |

### Reader and partial failure

| Observation × read state | Caller result and durable delta | Effects / race rule / test |
|---|---|---|
| Lifecycle IO unavailable, core records fresh | Same-scope whole cached snapshot or unavailable; fresh core wins; no writes | A fresh ack and child stop cannot be undone; `partial_fact_read_preserves_fresh_core` (U6) |
| Lifecycle read succeeds but one pool is malformed or future-shaped | Invalid/unsupported lifecycle, no cached substitution; preserve stored file and core records | No pool/floor reset; `malformed_pool_never_resets_its_floor` (U6) |
| Pointer changes, new snapshot unreadable | New binding remains selected; no old-binding facts | Cache includes address/launch/binding/path; `binding_change_drops_fact_cache` (U6) |
| Legacy replacement succeeds, fact replacement fails | Legacy visible; rich result incomplete; retry may repair | Do not describe multi-file atomicity; `crash_between_activity_and_snapshot` (U6) |
| New facts, unchanged rendered badge | Public cache changes; formatter output does not | Data refresh must precede display equality shortcut; `fact_only_poll_updates_public_view` (U2) |
| Consumer mutates nested return | Consumer's copy changes only | Getter stays IO-free; `public_fact_view_cannot_mutate_cached_state` (U2) |
| Wall time before an observation's write time | Report clock-skew diagnostic with evidence retained; no outcome synthesis | No negative-age conversion into stopped/resolved; `fact_clock_skew_is_not_outcome` (U6) |

### Completion sources and consumers

| Source | Legacy formatter | Lifecycle consumer | Binding retention / child count |
|---|---|---|---|
| Claude root `Stop` | Existing stop badge, ack rules unchanged | `response_finished`, not successful task | Does not end binding or clear children |
| Codex root `Stop` | Existing stop badge | `response_finished`, continuation may follow | Existing parent child-clear policy only; not session end |
| Pi `agent_settled` | Existing stop badge | `run_settled`, outcome independent | Existing binding/children policy |
| Pi `agent_end` | Inert | No new observation | No change |
| Tool result, attempt error, interrupt | Existing behavior only; no newly invented badge | Narrow result/outcome observation | No binding end or automatic child clear |
| Compaction success | No new badge | Compaction success only | Never used as session-ending evidence |
| Codex async question successful Post | Keep existing badge policy | Question publication, not answer; later Stop does not remove it | No binding end or child clear |
| Consumer follow-up tint dismissal | No bundled badge change | Presentation state only; question facts remain | No writer, retention or identity change |
| Exact activity acknowledgement | Existing eligible badge hides | Badge dismissal only, request evidence remains | No child or binding change |
| Existing `SessionEnd` / guarded retention | Existing behavior | Facts stay attached to their historical binding until retained state is pruned | Only existing end/absence/retention rules apply |

Composition tests must combine: two equal-badge requests; a child question plus lead Stop; error→retry→settled; request→focus→tool return; generic traffic plus a retained request/result pair; async Post-only publication and replies on both sides of Stop; two same-titled questions with independent consumer dismissals; compact attempt→success; fresh ack with cached facts; malformed one-pool state; and an old reader with a new snapshot. Passing isolated hook tests does not discharge these combinations.

## Operational invariants and probes

| Contract | Mechanism or probe | Failure response |
|---|---|---|
| Private, bounded data | Allowlist before persistence/logging; fixed UTF-8 limits; no prompts, titles, arguments, answers, URLs, error text, outputs, headers, or transcripts | Reject invalid observation; do not echo payload. Synthetic secret sentinel must be absent from records, stdout, stderr, and diagnostics. |
| Non-controlling hooks | Existing quiet/default and strict modes; no provider decision object; callbacks enqueue without changing native values | A stdout/control-result or UI mutation is a gate failure. Do not register the observer live. |
| No hot-path process work | Getter/formatter do no IO; poll adds one selected snapshot read and bounded pure reduction; no new process or timer | IO/process-count assertions fail the unit. Do not work around with a polling CLI. |
| Retention bound | Per-pool count/compact-byte limits, independent floors and one validated snapshot replacement; bounded raw file read | Any bound violation, wrong-pool placement, cross-pool eviction or at-floor resurrection fails the unit. |
| Callback cost | U6 measures frozen baseline and candidate Rust binaries on the same synthetic 100-event sequence and 20-child burst, including full snapshots | Proposed local regression budget: candidate sequential p95 and burst wall time must each be no more than 2× baseline in two consecutive alternating runs. Above it, optimize within the architecture; do not waive or claim a cross-machine SLO. |
| Lua cost | U6 measures 20 synthetic pane views with 128-entry snapshots and repeated unchanged polls; record timings and operation counts | No per-entry subprocess, no historical directory walk, no new write on unchanged polls. Any such operation fails regardless of elapsed time. Report timing; no unsupported GUI latency promise. |
| Real integration | U4's installed runner probe and actual queued Rust child; U7's native contact matrix | Typecheck-only is not a pass. Missing native emission stays unverified and blocks only that capability claim/activation. |
| Wrapper-free identity | Metadata-only controlled-process probe of provider callback ancestry/generation, correlated to the actual origin, not just current pane occupant | If same-session resume, delayed callback, or PID/exec reuse cannot be distinguished, do not enable that source. Keep verified-claim mode; report unsupported proof. |

The current performance harness compares retired Python to Rust. U6 must extend or replace only its test inputs to compare a frozen current Rust baseline against the candidate; do not revive a Python production dependency or use equal artifact hashes as two implementations. Measurements must use clean synthetic environments, never inherited process-environment dumps.

## Implementation units

Build order: **U1 → U2 → U3 → U4 → U5 → U6 → U7 → U8**. U7's read-only contact preparation can run earlier; its capability verdict consumes U2/U4/U5 evidence. Implementation is serial because the units share provider and reader files. Independent research/probes may run in parallel. Each unit is an end-to-end exercisable slice, not a layer-only task.

### U1. Preserve tool observations through the writer and reader

- **Goal:** a synthetic native tool preflight/result/error reaches an exact binding's typed bounded snapshot and Lua view without changing its existing badge.
- **Requirements:** R1–R3, R7–R10, R13; H02–H04, H18 foundations.
- **Dependencies:** None.
- **Files:** Create `src/observations.rs`, `tests/fixtures/lifecycle/observations.json`; modify `src/lib.rs`, `src/main.rs`, `src/providers.rs`, `src/protocol.rs`, `src/lifecycle.rs`, `src/records.rs`, `src/maintenance.rs`, `protocol/v2.json`, `plugin/protocol.lua`, `plugin/reader.lua`, `plugin/runtime.lua`; test `tests/rust/lifecycle_spec.rs`, `tests/fixtures/v2/check.py`, `tests/fixtures/v2/protocol-cases.json`, `tests/auto_clear_spec.lua`.
- **Approach:** define the complete 14-kind union, required-if-question mode refinement, and two-pool reducer first, then exercise the tool variants through existing entrypoints. Register the new filename for preservation immediately; full pruning exercise is U6. Keep observation-only and existing display actions distinct.
- **Patterns to follow:** `src/providers.rs:35` action dispatch; `src/records.rs:459` validated read and `:498` atomic replacement; `src/lifecycle.rs:500` semantic activity equality; `plugin/reader.lua` exact-scope per-record recovery. Recheck line positions at contact.
- **Test scenarios:** *Happy:* exact claimed binding + preflight/post → retained identity and narrow outcome. *Edge:* no native ID → distinct local receipts; blocked preflight → no fabricated execution. *Error:* nested null, object-shaped arrays, forged child digest, wrong pool, oversized UTF-8, future snapshot → reject/preserve. *Concurrency:* older/equal conflicting key, per-pool at-floor receipt, missing tool identity, class-changing retry, full equal-time group → specified reducer result with the other pool unchanged. *Mode parity:* both modes on both question phases round-trip; missing/null/unknown/forbidden modes and incompatible provider/tool tuples reject. *Compatibility:* unchanged legacy activity event ID and six return values; question mode never changes badge policy.
- **Verification:** every union kind and pool-placement case validates/round-trips in Rust/Lua/Python, bounded reads stop before overflow, and the real Rust CLI tool fixture yields the expected Lua lifecycle facet and unchanged badge.
- **Proven through (planned verification path):** existing temporary state/pty/clock seams plus real child CLI execution; no live provider.
- **Runtime evidence:** unverified — build the U1 candidate and run the tool-observation fixture through the production CLI, then the Lua fixture reader. Source inspection alone does not prove the new path.
- **Checkpoint:** auto — tool-observation end-to-end fixture, shared protocol parity, and legacy regressions pass; continue.

### U2. Expose requests beside exact badge acknowledgement

- **Goal:** the complete 16-field base getter plus lifecycle facet exposes request evidence after focus dismissal, with exact narrowly named outcome relations.
- **Requirements:** R1–R3, R5, R7–R10, R13; H08–H12, H15–H18.
- **Dependencies:** U1.
- **Files:** Modify `src/providers.rs`, `src/lifecycle.rs`, `plugin/reader.lua`, `plugin/runtime.lua`, `plugin/overlays.lua`, `tests/fixtures/providers/claude.json`, `tests/fixtures/providers/codex.json`, `tests/fixtures/lifecycle/observations.json`; test `tests/rust/lifecycle_spec.rs`, `tests/auto_clear_spec.lua`, `tests/wezterm_protocol_smoke.lua`.
- **Approach:** add structured question, approval, auto-denial, elicitation and approved notice parsing. Classify blocking/nonblocking question mode and derive publication IDs separately from exact result relations at poll time. Valid async Post-only evidence must be exposed immediately. Absorb the older consumer plan's U1 projection into the explicit 16-field allowlist, preserving nested-copy and nil/false rules. Expose the existing acknowledgement as badge evidence; do not add a seen file or change acknowledgement selection.
- **Patterns to follow:** `plugin/reader.lua:284` exact ack filtering; `plugin/overlays.lua:397` focused-pane acknowledgement; `plugin/runtime.lua:544` detached public allowlist. The earlier consumer U1 is transferred here; its writer/provenance U5 remains separate.
- **Test scenarios:** *Happy:* request→focus→matched result → hidden badge, retained request, dismissal ID, and tool-result relation. *Edge:* two request IDs with same badge, ID-less permission, result-before-request → no collapse or time-based matching. *Error:* wrong target, sibling child, invalid correlation → no relation. *Concurrency:* new fact with old badge/ack → fresh facet without redisplay; question result before request → request pool immediately. *Projection:* both display-priority orders, review-only and quiet bindings, uncertainty and copied address/puppet → correct base fields; no raw internal fields. *Async cases:* Post-only and ID-less publication, Pre/Post reorder, Stop on either side of publication, replies on either side of Stop, mismatched modes, and generic async messages follow the new matrix. *Negative:* elicitation selection ≠ delivery; URL notice adds no previously absent badge; arbitrary Pi tool name is not an approved question classifier.
- **Verification:** API-only consumer fixture can distinguish request attempt, nonblocking publication, blocking tool return and badge dismissal without an `answered` claim; nested mutation cannot alter cached state.
- **Proven through (planned verification path):** production Rust request fixtures, Lua focus callback seam, detached public getter, installed-WezTerm protocol/getter smoke.
- **Runtime evidence:** unverified — run the composed request/focus/result fixture through the implemented writer and installed WezTerm; current focus code proves only the pre-existing badge mechanism. Codex0.154 source routing is verified at the pinned commit; the combined native-hook test is U7.
- **Checkpoint:** auto — exact-relation, focus, cache-refresh, and getter smoke exercises pass; continue.

### U3. Preserve submission and run outcomes without ending the session

- **Goal:** prompt submission, Stop, attempt error and interruption become distinguishable facts while current stopping behavior stays intact.
- **Requirements:** R1, R2, R4, R7, R10; H01, H05–H07.
- **Dependencies:** U1, U2.
- **Files:** Modify `src/providers.rs`, `src/lifecycle.rs`, `tests/fixtures/providers/claude.json`, `tests/fixtures/providers/codex.json`, `tests/fixtures/providers/pi.json`, `tests/fixtures/lifecycle/observations.json`; test `tests/rust/lifecycle_spec.rs`, `tests/auto_clear_spec.lua`.
- **Approach:** normalize supplied native source/turn/outcome metadata and add observation-only handlers for newly supported callbacks. Keep low-level Pi `agent_end` inert. Pi bridge dispatch is U4; serialized Pi receiver fixtures are exercised here.
- **Patterns to follow:** current provider-specific ParentStop logic and binding-end guards in `src/lifecycle.rs`; exhaustive `ProviderAction::ALL` fixture conventions in `tests/rust/lifecycle_spec.rs`.
- **Test scenarios:** *Happy:* submission→preflight→response-finished yields three narrow facts. *Edge:* intercepted input does not imply run start; Stop continuation context remains metadata. *Error:* Claude StopFailure and Pi assistant error retain failure without session end. *Concurrency:* error→retry→settled and child work plus root Stop preserve each provider's existing count rule. *Negative:* Pi abort does not become Codex user interruption; non-assistant `message_end` and generic message traffic remain out.
- **Verification:** composed lifecycle fixture reports the observed attempt outcome and later settled/finished observation independently, with unchanged binding phase and legacy renderer outputs.
- **Proven through (planned verification path):** production CLI fixture and pure reducer plus Lua view; no native provider emission is claimed.
- **Runtime evidence:** unverified — execute the composed outcome fixture after U3. Installed provider emission remains U7 evidence.
- **Checkpoint:** auto — submission/outcome composition and legacy stop/count fixtures pass; continue.

### U4. Bridge Pi native events into typed observations

- **Goal:** Pi's callback/queue/child-process path retains approved metadata using the existing 0.80.5 baseline API, without runtime version gates.
- **Requirements:** R1–R5, R8–R11; Pi parts of H01–H07. H12 has no native Pi support.
- **Dependencies:** U1–U3.
- **Files:** Modify `pi/index.ts`, `src/providers.rs`, `tests/pi_extension.test.ts`, `tests/pi_node_runtime.mjs`, `tests/fixtures/providers/pi.json`; create `tests/pi_lifecycle_runtime.mjs`; test `tests/rust/lifecycle_spec.rs`.
- **Approach:** extend typed WriterRequest variants; capture native IDs and allowed scalar metadata before enqueueing. Add `input`, `tool_execution_end` and narrow assistant `message_end`; extend existing callbacks. Keep queue/reload/shutdown semantics and peer range. No VERSION import, comparison helper, prerelease/malformed-version policy, UI callbacks, new SDK or dependency.
- **Patterns to follow:** existing queue and Node child dispatch in `pi/index.ts:234`; installed ExtensionRunner registration/dispatch/teardown; `tests/pi_node_runtime.mjs` process checks. Use baseline-exported `ExtensionAPI`/`ExtensionEvent` or callback inference, not newer named type exports.
- **Test scenarios:** *Happy:* installed runner invokes production extension → real Rust child → snapshot → reader. *Baseline:* load/dispatch against pinned 0.80.5 and installed current Pi without a model; an additional module stub omitting VERSION catches accidental coupling. *Edge:* input intercepted after observation remains submission, not run start; generic Pi tool named like a question stays generic. *Error:* writer failure cannot downgrade established v2 state. *Concurrency:* reload and delayed queued work cannot target a replacement binding. *Exclusion:* removed UI/failure callbacks are neither subscribed nor admitted. *Privacy:* prompt, answer and tool-output sentinels never enter writer arguments, diagnostics or files.
- **Verification:** actual callback dispatch proves the implemented bridge; pinned baseline load/dispatch and typecheck pass. Every retained Pi source maps to its correct narrow observation with no version-dependent branch.
- **Proven through (planned verification path):** actual ExtensionRunner and controlled extension-loader calls with synthetic descriptors, context and real Rust child in disposable state. No provider/session constructors or model calls. Test-only baseline material stays in disposable storage; no installed package replacement.
- **Runtime evidence:** source-checked at Pi tag `v0.80.5`, commit `cc62baa442b5c0333923fdfdcc1d7264f445b5b0`: retained subscriptions, context methods, tool IDs and assistant stop reasons exist. The implemented bridge and baseline runtime compatibility remain unverified — execute `tests/pi_lifecycle_runtime.mjs` against the two pinned runtimes. A source/type check alone is not this exercise.
- **Checkpoint:** auto — real bridge, baseline, exclusion and reload exercises pass; continue. If test material is unavailable, continue independent units but do not report baseline runtime compatibility passed.

### U5. Observe context compaction separately from settling

- **Goal:** available compaction attempt and success observations reach consumers without becoming agent completion.
- **Requirements:** R1, R2, R6, R10, R11; H19–H20.
- **Dependencies:** U1, U3, U4.
- **Files:** Modify `src/providers.rs`, `src/lifecycle.rs`, `pi/index.ts`, `tests/fixtures/providers/claude.json`, `tests/fixtures/providers/codex.json`, `tests/fixtures/providers/pi.json`, `tests/fixtures/lifecycle/observations.json`; test `tests/rust/lifecycle_spec.rs`, `tests/pi_extension.test.ts`, `tests/pi_lifecycle_runtime.mjs`, `tests/auto_clear_spec.lua`.
- **Approach:** add PreCompact/PostCompact and Pi `session_before_compact`/`session_compact`. Emit `compaction_attempted` and `compaction_succeeded` in general. Both Pi callbacks exist at 0.80.5; no runtime version branch or compaction failure/abort variant is added.
- **Patterns to follow:** existing compact SessionStart binding update and Pi non-ending reload branch; U1 typed variants and U4 metadata-only queue.
- **Test scenarios:** *Happy:* attempt→success → both facts and no new stop badge. *Edge:* cancelled before hook, absent attempt ID, compact SessionStart metadata update → no invented current phase or rebind. *Error:* malformed allowed metadata → diagnostic without legacy change. *Concurrency:* success before attempt stays separate unless native identity supports a relation; resumed old binding cannot receive newer facts. *Negative:* excluded `session_compact_failed` is ignored/unregistered; context compaction never invokes Attention retention or child clear.
- **Verification:** CLI plus Pi dispatch fixture yields compaction success with unchanged lead, binding and child policy. No schema placeholder or enum value remains for H21.
- **Proven through (planned verification path):** disposable CLI/reader composition and actual Pi runner dispatch with synthetic compaction events, including the baseline compatibility harness.
- **Runtime evidence:** source availability verified at 0.80.5; new runtime path unverified — execute the implemented compaction fixture. This does not require a model-driven compaction.
- **Checkpoint:** auto — compaction composition, baseline dispatch and provider regressions pass; continue.

### U6. Recover and retain bounded facts under failure

- **Goal:** reconnect, cache failure, cleanup and capacity pressure preserve trustworthy bounded facts without degrading core rendering.
- **Requirements:** R2, R8–R10, R12, R13.
- **Dependencies:** U1–U5.
- **Files:** Modify `src/maintenance.rs`, `src/records.rs`, `src/observations.rs`, `plugin/reader.lua`, `plugin/runtime.lua`, `tests/rust/maintenance_spec.rs`, `tests/rust/lifecycle_spec.rs`, `tests/rust/measure.py`, `tests/rust/measure_spec.py`, `tests/auto_clear_spec.lua`, `tests/wezterm_reattach_smoke.lua`; create `tests/fixtures/lifecycle/compatibility.json` and `tests/fixtures/lifecycle/compatibility-provenance.md`.
- **Approach:** include the snapshot in existing audit/prune registries and revalidation, not a new cleanup command. Exercise floor and byte bounds under concurrent writers and crash seams; compare a frozen current reader/binary with the new contract. Measure current Rust versus candidate Rust with synthetic state only.
- **Patterns to follow:** existing equal-timestamp child retention protection and unknown-file preservation in `src/maintenance.rs`; per-record reader recovery; existing same-machine measurement harness's artifact-identity checks.
- **Test scenarios:** *Happy:* fresh reader rehydrates facts from selected binding with unchanged badge. *Edge:* both pools at 64 entries/122,880 bytes, maximal envelope, UTF-8/escape spellings, equal-time groups and evicted partners → specified independent windows. Evict async Pre while retaining Post → publication-only evidence remains available; evict Post → disclose bounded omission, not resolution. *Error:* corrupt/future snapshot, malformed one-pool floor, oversized raw file, unreadable facts with fresh ack/child stop, clock skew → preserve core truth and separate diagnosis. *Concurrency:* separate floor/candidate races, count/byte churn in either pool, class-changing retries, crash or post-rename sync failure, new binding with unreadable snapshot → no cross-pool eviction, duplicate migration, resurrection or cache crossover. *Compatibility:* frozen old core reader handles new additive manifest; old maintenance conservatively preserves unfamiliar file.
- **Verification:** independent pool bounds/floors and whole-snapshot failure matrix pass; doctor/prune recognize valid new files without accepting nested identity corruption; 100-event/20-child regression probe meets its local budget and 20-pane read operation bounds hold.
- **Proven through (planned verification path):** temporary filesystem/clock/failure seams, separate writer processes, frozen git artifacts and disposable installed-WezTerm smoke. No live process-environment scan.
- **Runtime evidence:** unverified — run the U6 compatibility, reattach and measurement fixtures after implementation; previous U1–U7 reports do not prove this new snapshot path.
- **Rollback:** older readers can ignore the sidecar only after the frozen-reader test passes. Keep new files on rollback; older cleanup may refuse them. Do not delete retained evidence to make rollback look clean.
- **Checkpoint:** auto — failure, compatibility, retention and performance exercises pass; continue. Measured regression is a repair obligation, not a reason to waive the bound.

### U7. Verify native coverage and wrapper-free identity limits

- **Goal:** produce a capability report that distinguishes working dispatch from actual native emission and trustworthy execution attribution.
- **Requirements:** R1, R2, R5, R11–R13; H11/H12 uncertainty and admission for all rows.
- **Dependencies:** U2, U4, U5; metadata-only probe preparation can precede them.
- **Files:** Create `tests/lifecycle_contact_probe.mjs`, `tests/fixtures/lifecycle/contact-cases.json`, `docs/reviews/lifecycle-contact-results.md`; modify `tests/provider_contact_hook.py`, `tests/fixtures/providers/claude-contact-settings.json`, `src/lifecycle.rs` only for an admission source proven by the probe.
- **Approach:** contact preparation uses synthetic payloads, controlled ptys/processes and installed runner code. Add a local mock-SSE Codex exercise with trusted disposable PreToolUse/PostToolUse handlers, adapting the pinned upstream async-question test seam. Observe the real hook envelopes through the candidate writer/reader; no paid model or live registration is needed. Do not repurpose `SystemProcessProbe`: it observes presence and its environment-scan path is outside this task's privacy authority. Enable a wrapper-free rich-admission source only if metadata proves the actual execution generation and delayed-callback fence; otherwise retain verified inherited-claim admission.
- **Patterns to follow:** `src/lifecycle.rs:175` launch resolution, with its evidence source made explicit; existing disposable contact harness, after auditing it for raw payload/environment logging.
- **Test scenarios:** *Happy:* exact inherited launch and current binding → accepted; controlled metadata proof, if obtainable → accepted only for that source. *Edge:* same-session resume, nested agent, tty reuse, PID reuse, exec/reparenting, mux host and delayed callback → old execution cannot borrow newest claim. *Error:* inaccessible metadata or missing native IDs → held capability, no heuristic fallback. *Synthetic hook integration:* call `request_user_input_async` through Codex with a local mock backend; match trusted native hooks, assert exact tool identity and accepted-publication response, later final/Stop, and no answer in Post. Also exercise normal user-input delivery before/after Stop without fabricating original-call correlation. A combined UI test must show a pending question alongside separate queued input without treating the question as the queue-drain blocker. *Live contact:* real-provider question/cancel coverage and actual user choices require separate consent; absence of a dedicated signal is not a universal negative.
- **Verification:** report each H-row/provider path as synthetic, installed-dispatch, native-contact, unsupported, or unverified with artifact identity; execution identity has a separate pass/fail/unknown column. No-wrapper support is not checked off merely because a record appeared.
- **Proven through (planned verification path):** controlled process/pty metadata stand-ins and installed local code. Native provider paths may need operator consent and a disposable session; the stand-ins cannot prove those paths fired.
- **Runtime evidence:** unverified — execute the metadata-only identity probe and implemented observer against controlled origins. The 0.154.0 handlers and hook registry were source-verified at `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`, not runtime-tested. Adapt the checked-in `core/tests/suite/request_user_input_async.rs` local SSE case and its pre-build hook/trust setup; verify the exact fixture API at contact rather than relying on remembered helper names. The synthetic native-hook combination and real-provider question/cancel coverage remain unexecuted.
- **Checkpoint:** gate — the synthetic native-hook exercise and local probes are automatic and need no live-provider consent. Run those probes and publish the capability report. Continue paths whose local proof passes. Missing or failed identity proof holds only wrapper-free rich admission and its activation. Missing native-contact consent holds only those runs and their coverage claims. Unknown: continue synthetic verification, documentation, and U8; block live registration, paid calls, and unproven identity enablement. The human contribution is specific consent before external effects; the agent cannot grant it. No independent safe unit waits for that consent.

### U8. Publish the consumer contract and complete the local gate

- **Goal:** a consumer example demonstrates the requested granularity, and a single repeatable gate checks the complete approved observation scope.
- **Requirements:** R1, R8–R13.
- **Dependencies:** U1–U6 and U7's report; U7's operator-dependent paths may remain explicitly held.
- **Files:** Create or merge `docs/consumer-guide.md`, owning the older consumer plan's U2 documentation; modify `README.md`, `docs/record-contract.md`, `examples/hook.sh`, `tests/gate.sh`, `tests/wezterm_protocol_smoke.lua`; create `tests/fixtures/lifecycle/consumer.lua`, `tests/fixtures/lifecycle/check-coverage.mjs`; update `docs/reviews/lifecycle-contact-results.md` with final evidence.
- **Approach:** exercise the existing planned consumer fixture through blocking request/focus/return, nonblocking publication across Stop, consumer-local tint/dismissal, error/retry/settled, and generic traffic beside request evidence. The example selects a symbolic follow-up appearance from `question_mode=nonblocking` plus publication IDs in the current active binding. It demonstrates a consumer choice, not a new WezTerm pane-color API or default renderer. Document bounded evidence and missing native coverage at the field where users consume it. Command-owned examples remain inert installation examples, not live config writes.
- **Patterns to follow:** source-backed hook map catalog coverage checks; existing fail-fast `tests/gate.sh`; the older consumer plan's public-fact decision rubric. Include independent activity/review, consumer-owned ranking, uncertainty, exact acknowledgement, manual rendering versus polling controls, and the `bindings --json` identity/liveness boundary. Preserve the already-correct Rust-first producer wording; do not claim unfinished puppet ingestion or Relay migration.
- **Test scenarios:** *Happy:* actual getter supplies the specified consumer cases from production records. *Edge:* no snapshot, cached snapshot, evicted evidence, unverified hook path → clearly different from a false negative. *Compatibility:* prior getter fields, six-value API, formatter, v1 projection, Pi bridge and shell tests remain green. *Consumer policy:* available publication can tint while lead activity thinks or stops. Explicit consumer dismissal clears the displayed publication's tint. Binding replacement/end drops the old display scope, then reevaluates any current-binding evidence. Loss of usable evidence, including missing/cached/invalid data or eviction, reports unknown rather than an all-clear. Generic input/focus/Stop does not resolve a question. Dismissal uses full address/launch/binding plus exact displayed publication IDs, never snapshot ID or pane ID alone; it is private in-memory example state and not persisted by Attention. Dismiss Q1, then receive ordinary traffic/Stop and Q2 → Q1 stays dismissed, Q2 can tint, other consumers and records stay unchanged. *Coverage:* every active H-ID has a named production-path fixture and all manifest kinds plus question-mode/phase/publication-only combinations have parity rows; excluded payload branches produce no new writes.
- **Verification:** source manifest/typed union/coverage registry agree; Rust, LuaJIT, Python parity, Bun, Node, TypeScript and installed-WezTerm checks pass in one chained local gate. The report separately lists native contact and activation not done; it never labels an unverified path passed.
- **Proven through (planned verification path):** real CLI-generated fixtures consumed by the getter and example, plus the repository gate in disposable state.
- **Runtime evidence:** unverified — run the new composed consumer fixture and complete gate after implementation. No runtime implementation suite was claimed rerun by this planning document.
- **Checkpoint:** auto — complete local gate and consumer exercise pass; hand back code, evidence and uncovered decisions without staging, commits, live configuration changes, or claiming the U7 held paths complete.

### Consumer appearance contract

The example in U8 provides the concrete downstream use requested by the user: choose a different pane appearance when a published nonblocking follow-up needs that consumer's attention. Its input is the copied lifecycle facet, not terminal text or provider output. The example only selects a symbolic appearance; the real consumer owns how its UI applies color.

Consumers reread the cached getter on their existing refresh/poll cadence after Attention's poll. They must not wait for a changed badge event ID: publication can change while the badge remains identical. This plan adds neither a change-subscription API nor extra bundled redraws for fact-only updates.

The example's dismissal set is scoped to exact displayed publication observation IDs and full binding identity. It does not write `lifecycle.json`, `ack.json`, a seen record, or the shared cache. Unrelated snapshot rewrites cannot undo dismissal. Another consumer may choose another policy from the same unchanged facts. The example's in-memory dismissal is not a persistent cross-GUI acknowledgement promise.

“Follow-up appearance” means observed publication awaiting consumer dismissal, not verified current waiting or a verified unanswered question. If a consumer needs exact live answer status, it requires stronger provider correlation than these hooks expose. The plan does not hide that gap behind a boolean.

## Scope boundaries

In scope: the 18 retained rows; typed observation storage with request/general budgets and floors; blocking/nonblocking question mode, publication-only evidence and a consumer-owned appearance example; exact native relations; existing badge acknowledgement and the complete cached getter; baseline Pi bridge; compatibility, maintenance, privacy/performance proofs; bounded identity/contact investigation; shared consumer docs and local gate.

Not in scope: a provider harness, screen scraping, input interception, permission decisions, prompt/answer capture, SDK/app-server migration, replay/changes API, unbounded event history, new historical binding catalogue, notification delivery service, Relay implementation, title-policy expansion, model/cwd/task/team metadata, Bash parsing changes, shell wrappers, new focus writers, universal pending/answered state, or live installation. Existing separate binding/child files, guarded retention, raw monotonic ordering, v1 compatibility and rendered count policy are retained.

**Cut candidates decided:** H13/H14 UI observations and H21 compaction failure are deferred by the approved scope revision; Pi input stays because it is baseline submission evidence; H12 explicitly has no native Pi leg. Keep activity fields stable, reuse existing acknowledgement, and do not create a generic event framework, per-request seen system or full-facts CLI. The deferred native capabilities are recorded in [BACKLOG.md](BACKLOG.md).

### Deferred to follow-up work

[BACKLOG.md](BACKLOG.md) records Pi UI observations, Pi compaction failure, root API-error badge policy, and child-approval badge policy, with evidence and reopening criteria. H21 is deferred to avoid newer-version support machinery, not because its failure signal is weak. Badge follow-ups must define aggregation, retry and acknowledgement behavior; promoting a child event to lead activity is not a complete fix. Puppet writer, bootstrap Relay and live activation remain in the [earlier consumer plan](2026-09-08-001-feat-attention-view-consumers-plan.md). No live work is authorized here.

## System-wide impact and disconfirming evidence

| Area | Consequence / control |
|---|---|
| Interaction | Native callback → typed adapter → guarded legacy action plus bounded snapshot → existing Lua poll → additive getter. Only the existing activity branch reaches the bundled badge formatter. |
| Errors | Identity rejection prevents all misattributed effects. Rich-shape/storage failure reports an independent diagnostic while an independently valid legacy action can remain effective. Raw payloads never become diagnostic text. |
| State lifecycle | Atomic snapshot replacement is the sole new durable unit. Multi-file crash states are visible and retryable where IDs permit; no exactly-once claim. Floor and evidence deletion are inseparable. |
| API parity | The manifest, Rust union, Pi transport and Lua/Python boundary validators have explicit parity tests. Existing `get_attention`, `type`, review, child count, owner filtering and `bindings --json` remain unchanged. |
| Upgrade / rollback | Additive sidecar avoids weakening old strict activity parsing. Old maintenance may conservatively block pruning unknown files; U6 freezes and proves the actual compatibility boundary. |
| Concurrent work | This plan's U2 owns the shared getter; U8 owns the shared guide/README changes. Older U5/U3/U4 retain puppet writer/Relay/activation and consume these results. Check current files and preserve concurrent edits; no cross-repo edits. |

Evidence against stronger designs is material: permission hooks lack universal request/result IDs; tool results can be transformed or expose tool-specific failure; `agent_end` is below Pi's final continuation boundary; current `commit_with` is not a transaction across files; tty/process presence is not execution-generation identity. The historical Pi UI inversion supports keeping that capability deferred. A single flat 128-entry window also lets generic tool churn evict requests; the new independent budgets directly address that failure.

### Bug-trace cross-check

| Motivating case | Contract / test | Expected result |
|---|---|---|
| Generic tool traffic evicts an unanswered question or its result | Independent pool budgets; `general_overflow_preserves_request_pool_and_floor` | Request evidence unchanged under general count and byte pressure. |
| Async publication is followed by Stop or ordinary input | Publication-only RequestEvidence plus `async_publication_survives_stop_in_both_orders` | Consumer can retain its follow-up appearance without claiming exact answer state. |
| Result arrives before its question | Static tool class; `question_result_before_request_uses_request_pool` | Result starts in requests; no later migration. |
| One malformed pool erases its floor | Whole-snapshot validation; `malformed_pool_never_resets_its_floor` | Snapshot invalid/cached as a whole; core badge data remains independent. |
| Lua accepts an object as an empty array or allocates an oversized file | Bounded raw parser; `nested_snapshot_shape_parity_in_installed_wezterm`, `snapshot_read_stops_at_byte_limit` | Reject wrong collection shape; stop reading at cap plus sentinel. |
| Focus is mistaken for answer receipt | Existing exact badge ack; `facts_survive_acknowledgement_without_redisplay` | Badge hides; request evidence remains; no answered claim. |
| API error or child permission does not change the badge | Existing policy preserved; backlog badge lanes | No silent behavior change. Claude StopFailure may leave prior thinking; Pi may later settle. Child permission remains child presence. |
| Native identity or question/cancel coverage is assumed from green fixtures | U7 report and dependent hold | Native coverage remains unverified until contact; no wrapper-free enablement by assumption. |

Historical reconnect/cache reports are not newly reproduced bugs on this HEAD. This plan does not claim to fix them again. The quoted external review did not include its referenced four fixes, and a scoped document search found no identifiable set. This revision closes the explicit cases above; it does not claim to apply an unseen review.

## Build execution contract

**Closed decisions:** explicit blocking/nonblocking question mode; async publication separate from answer evidence; consumer-owned follow-up appearance and scoped display dismissal; no answer-text parser; one snapshot with 64-entry/122,880-byte request and general pools, independent floors and no borrowing; strict manifest/14-kind union; current display/focus policy; no per-request seen/answered claim; exact identity; baseline Pi API with no VERSION machinery; active H01–H12 and H15–H20; U2/U8 own getter/docs; Later and live installation out.

**Builder autonomy:** implement in this checkout and branch, preserving concurrent edits; choose private helper names/test organization within the naming ledger; repair failures within the declared contracts; improve local test seams without new production frameworks. Verify current base and whether work already landed before editing. Record plan-silent local decisions and continue when reversible and contract-preserving. A builder summary is a claim; handoff includes actual Git state, artifact identities, test results and a “decisions the spec did not cover” list.

### Verify at contact

| Assumption | Check | Safe fallback |
|---|---|---|
| Current branch still has the inspected Rust shapes | Inspect HEAD/diff and affected functions before edits; fetch/check current base under the repo's normal policy | If already implemented, verify instead of duplicate. Preserve unrelated edits and reconcile only shared surfaces. |
| New manifest additions leave old core readers usable | U6 frozen old parser/reader with new manifest and new snapshot | Preserve old shapes. If additive compatibility fails, hold schema emission and request a scoped contract revision; do not silently bump/bypass validators. Independent adapter fixtures can continue. |
| Codex async hook routing and receipt meaning | Pin 0.154.0 source and run U7's local mock-SSE tool/native-hook combination; distinguish async tools from hooks configured to execute asynchronously | Source routing is already verified, but runtime coverage is not. If the success envelope differs, retain exact diagnosis and no publication claim until the adapter/fixture agrees; do not parse terminal text as fallback. |
| Native fields have the documented meaning | Pinned payload definitions and installed contact fixtures for each selector | Missing optional descriptive field stays absent. Unsupported source/value is diagnosed; no invented correlation. A required meaning change needs plan revision, not a cast. |
| Retained Pi API exists at the declared minimum | Source/type checks at 0.80.5 already pass; U4 runs pinned baseline loader/dispatch plus installed runtime | Keep the 0.80.5 peer range; use baseline-exported types. If runtime fails, repair the adapter without reintroducing excluded features or a version parser; do not claim the check passed. |
| Wrapper-free callback belongs to exact current execution | Metadata-only identity probe including delayed old callback and same-session resumes | Keep rich admission restricted to a verified inherited claim; hold that source and activation, continue remaining units. Never read full process environments to improve the proof. |
| Native hook stdout must be empty or a neutral JSON object | Inspect installed provider's exact hook contract and run disposable no-model harness where possible | Preserve existing quiet hook behavior and do not register an unverified permission path. If neutral output differs by source, revise only with evidence; never return a policy result. |
| Baseline performance probe is comparable | Distinct frozen/candidate Rust artifact hashes, identical synthetic data and alternating runs | Repair the measurement before judging performance. Hardware results are not remote-runner predictions. |

**Expected gate map:** U1 adds 14-kind/pool/parser parity and tool pipeline; U2 adds complete getter/relation/focus; U3 adds attempt/stop composition; U4 adds actual Pi bridge/baseline/exclusions; U5 adds compaction attempt/success; U6 adds pool isolation/failure/retention/frozen compatibility/performance; U7 classifies evidence and held paths; U8 chains all checks. No permitted failing local assertions. A contact case marked `unverified: consent missing` or `unsupported: identity proof failed` is a coverage result, never a passed runtime test.

**Authority boundaries:** this plan grants no execution by itself. On a later build request, normal local edits, compilation, disposable test state, synthetic ptys/processes and installed-runtime harnesses are in scope. No real credentials or secret values may be read, printed, received or written. No environment/argv dump of real processes, live provider or shell configuration edit, provider call/spend, screen/input injection into user panes, staging, commit, push or deployment is included. Use synthetic payloads and isolated state instead. Audit existing contact/gate scripts before running them for those effects; a test filename does not authorize them.

**Concrete stop conditions:** stop the dependent action when a verified current contract would require changing existing badge behavior, schema compatibility fails with no plan-preserving encoding, the new rich-observation admission accepts an old execution as current, or the only remaining proof requires forbidden secrets/live effects. Continue every independent authorized unit. Stop the turn for user direction only when no such work remains; report the exact failing case and the smallest contract/authority change required. Slow tests, an absent model session, or a useful optional live rehearsal are not reasons to stop safe local work.

### Human inventory

| Contribution | Role and resolution | Work permitted without it |
|---|---|---|
| Request build | Local implementation authority: not granted by the plan-revision request | Revise and verify planning artifacts only. A later build request starts implementation under this contract. |
| Native Claude/Codex/Pi contact that starts a provider session or can incur spend | Consent: not granted. A later grant must name disposable session, payload, maximum calls/spend and allowed input effects | All synthetic/installed-runner tests, complete local implementation and gate. Native emission claims remain held. |
| Ordinary approval/question/cancel interaction, if needed to test native emission | Assistance and possibly consent: not supplied; must use a controlled prompt without real data | Synthetic action/result fixtures prove transport and reduction, not that installed native hooks fire for that choice. |
| Live hooks, shell sourcing, bootstrap Relay or deployment | Consent: explicitly outside this plan | Produce inert command-owned examples and a handoff; do not activate. |

No unbounded taste loop or repeated checkpoint approval is required. Automated local evidence determines U1–U6 and U8. U7's unresolved human contribution is authority for external effects, not permission to inspect safe source code.

## Risks and dependencies

| Risk | Required control |
|---|---|
| Request/general policy drifts between languages | One Rust placement owner; manifest limits; exhaustive cross-language kind/class/boundary fixtures. |
| New nested parser accepts ambiguous collections or oversized input | Bound reads before decode; preserve raw container kinds; installed-WezTerm negative fixtures. |
| Older runtime or old reader differs from source assumptions | U4 baseline dispatch and U6 frozen-artifact tests; no typecheck-only completion claim. |
| Async publication is mistaken for an unanswered/answered state | Expose mode and publication IDs, not a live pending boolean; no title/timing answer matching; consumer appearance remains policy. |
| Native callbacks cannot establish execution identity | Hold only unproved rich-admission source and activation; verified-claim work continues. |
| A missing result is treated as still waiting or resolved | Bounded evidence, per-pool floors and no current-waiting/answered field. |
| Two builders overwrite the getter or guide | Explicit ownership transfer in both plans; inspect current diff before edits. |

## Evidence and review state

Planning inspected current Rust/Lua/Pi code, pinned Pi 0.80.5 source, shared getter ownership and retention patterns. The Codex question fold also read exact release `rust-v0.154.0` (`6b9826e3aa83b1a5947db50f4332cb9c65f1b340`) through a Luna investigation and parent verification of the handlers, hook registry, UI heading, answer submission and queue predicate. The existing provider action fixture test passed during cut-list verification; the complete implementation gate was not rerun. The historical Pi UI-order probe is retained only as deferral evidence, not an active build obligation. New native callbacks, baseline runtime compatibility and wrapper-free execution identity remain unverified until their named build exercises run.

Sources: the [historical hook map](../reviews/2026-09-08-attention-hook-map.md) and adjacent native catalog; [record contract](../record-contract.md); [consumer plan](2026-09-08-001-feat-attention-view-consumers-plan.md); [request-evidence synthesis](../../.research/synthesis-attention-request-evidence-2026-09-08.md); current Rust/Lua/Pi files named in the units. Baseline API evidence is local `pi-mono` tag `v0.80.5`, commit `cc62baa442b5c0333923fdfdcc1d7264f445b5b0`: extension types/overloads, root exports and assistant stop reasons. Historical UI evidence ran at installed Pi 0.84.4 using `/tmp/attention-pi-ui-order.EXTwLu/probe.mjs`; that temporary file is not a builder dependency.


The Codex source locators below are immutable build inputs, not a dependency on a session-private temporary directory:

- [Async question handler](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/core/src/tools/handlers/request_user_input_async.rs#L80): validated questions, item publication, immediate accepted result and default tool-hook payloads.
- [Native tool-hook dispatch](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/core/src/tools/registry.rs#L567): Pre before handler and Post after successful result.
- [Async answer framing](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/context-fragments/src/answered_question.rs#L11) and [submission](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/tui/src/chatwidget/questions.rs#L95): quoted/truncated title plus ordinary input, without the originating call ID.
- [Queue-heading condition](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/tui/src/bottom_pane/pending_input_preview.rs#L146): queued messages or pending async questions. The heading cannot replace structured observation.
- [Synthetic upstream async-question tests](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/core/tests/suite/request_user_input_async.rs#L312): local mock-SSE seam to extend with native hook capture. Inspected, not executed here.

Self-review checks question-mode/phase coverage, standalone publication, reply-before/after-Stop cases, consumer tint/dismissal isolation, and the exact 18 H-rows and nine Pi rows, all 14 kinds, independent budgets/floors, whole-generation validation, static result classification, strict raw parsing, actor/correlation identity, acknowledgement independence, partial-file failure and transferred getter/docs ownership. The HTML is regenerated from this Markdown; no separate implementation authority is created. `/program` remains optional; `/build` reads this plan and performs its own preflight.
