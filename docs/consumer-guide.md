# Consuming Attention facts

Attention observes native agent callbacks. It does not control the provider, answer questions, or decide how another application should display them. Rust writes validated records; WezTerm's normal poll reads them; consumers copy the cached view.

This document describes the lifecycle implementation. It does not claim live hook registration. See [contact evidence and held paths](reviews/lifecycle-contact-results.md) before relying on a provider callback being active on a machine.

## Read one pane

```lua
local view = attention.get_attention_view(pane)
```

The getter performs no file reads, process launches, clock reads, or relation rebuilding. Nested tables are detached: changing the return value does not change Attention's cache. Read it after Attention's poll on your own refresh cadence; a new lifecycle fact need not change the badge's event ID or trigger a bundled redraw.

Use the full pane object. A server pane ID alone is not globally unique across mux realms.

| Fields | Meaning |
|---|---|
| `type` | Existing display winner after activity/review priority |
| `activity_type`, `event_id`, `source`, `puppet` | Eligible lead activity, independently of review |
| `review`, `subagents` | Owner-scoped review presence and eligible child count |
| `provider`, `binding_id`, `binding_phase` | Validated provider binding; a quiet binding can still identify its provider |
| `address`, `launch_id`, `marker_id` | Full realm/incarnation/pane address, launch, and compatibility marker ID |
| `pane_presence`, `reader_confidence`, `binding_health` | Read assessment; preserve uncertainty instead of treating it as absence |
| `lifecycle` | Optional bounded observations and derived request evidence |

These are sixteen base fields plus the lifecycle facet. Nil remains nil and false remains false. Internal records, cache keys, formatter state, deadlines, and root diagnostics are private. The old `get_attention` six-value API remains unchanged.

`puppet` exposes the value already cached. This change does not implement the separate producer-side provenance work. Consumers choose whether puppet activity matters; the bundled `show_puppet` option retains its own display policy.

## Lifecycle availability

| `lifecycle.availability` | Interpretation |
|---|---|
| `available` | Current selected file was read and validated |
| `cached` | An unavailable read reused the same scoped, previously validated file |
| `absent` | No lifecycle file exists at the selected scope |
| `unavailable` | The read failed and there is no matching usable cache |
| `invalid` | A successful read returned malformed or contradictory data |
| `unsupported` | The record declares a future schema |

Missing, invalid, unsupported, and unavailable data are not an empty pending-request list. A successfully read invalid/future file never falls back to older cached facts. Optional lifecycle failure does not invalidate an otherwise valid activity badge.

The facet contains `snapshot_id`, a flat `observations` array, `requests`, `retention_floors`, `coverage="bounded_window"`, at most eight diagnostics, and an exact `badge_acknowledgement` when one matches the stored activity. Each observation identifies its request/general pool.

## Requests are evidence, not a pending-state service

`requests` groups only supported exact native identities under the same binding and actor. Tool names, question modes and supplied turn scopes must match. Elicitation IDs also require their native `mcp_server_name` namespace. Two servers or sibling children cannot merge merely because they supplied the same ID. Local observation UUIDs are not native tool IDs.

Each group exposes request, result, selection and denial observation IDs, with narrowly named relations: `tool_result_observed`, `automatic_denial_observed`, or `elicitation_action_selected`. ID-less observations stay separate. Result-before-request arrival can still match; receipt order is not native causal order.

Question groups additionally expose:

- `question_mode="blocking"`: Claude `AskUserQuestion` or Codex `request_user_input`. A matched result means the tool returned, not necessarily that a human answered.
- `question_mode="nonblocking"`: Codex `request_user_input_async`. Its admitted successful PostToolUse means the provider accepted the question for publication. `publication_observation_ids` records that event separately from blocking tool-return relations.

A publication can stand alone when PreToolUse is missing, delayed, evicted, or lacks correlation. Preflight alone is only an attempt. A call may contain several questions; publication IDs are not an unanswered-question count. Codex's free-form async message tool and Pi addon names are not automatically questions.

The verified Codex 0.154.0 hook receipt is a JSON string containing `{"accepted":true}`. Attention validates that receipt and discards the output. Stop, focus, ordinary input, and elapsed time do not turn publication into answered. The reply path supplies no original call ID to the native submission hook. Attention does not parse quoted titles, terminal headings, or conversation order to guess an answer.

`badge_acknowledgement` identifies one activity event and target that was dismissed. It does not mark every request seen, read, answered, or resolved. Facts remain available after badge acknowledgement, activity clear/expiry, and lead Stop.

## Choose presentation independently

The runnable [consumer example](../tests/fixtures/lifecycle/consumer.lua) chooses a symbolic appearance:

- `follow_up`: an available nonblocking publication has not been dismissed by this consumer.
- `base`: no retained publication calls for a new appearance under this policy; this is not a verified all-clear.
- `unknown`: missing/uncertain data, an inactive binding, or lost evidence prevents that decision.

```lua
local consumer = dofile("/your/checkout/tests/fixtures/lifecycle/consumer.lua").new()
local appearance = consumer.appearance(attention.get_attention_view(pane))
-- Your UI maps appearance to its own color or notification.
-- When the user dismisses what this consumer displayed:
consumer.dismiss()
```

Dismissal uses only the last displayed publication IDs in the full address/launch/binding scope. A Q2 that arrives later is not dismissed by a click rendered for Q1. No shared record, acknowledgement, or other consumer is changed. This is bounded private memory, not durable cross-GUI acknowledgement. Binding changes reset the scope; ended bindings do not retain a visible follow-up tint.

You can choose a different policy for approval, review, or outcome facts. Keep activity, review and child count independent; do not reuse `type` as if it were all three. Rank panes using your own purpose, but retain read confidence and full identity with the selected result.

## Storage and retention

One binding-scoped `lifecycle.json` contains separate request/general pools. Each has at most 64 observations, 122,880 compact UTF-8 bytes, and its own monotonic retention floor. One observation is at most 2,048 bytes; the raw file is capped at 262,144 bytes and eight container levels.

Generic tool traffic cannot evict request evidence. Request traffic can still evict older requests or their outcomes. Whole equal-timestamp groups are removed with that pool's floor in one atomic snapshot replacement. There is no replay cursor, complete history promise, lifecycle TTL, or universal `pending_count`.

Activity and lifecycle are separate files, not a multi-file transaction. A crash may leave newer activity with older facts. Failures report incomplete work; retries re-read actual records. Do not join files by timestamp or assume a shared snapshot ID.

Older readers can ignore the additive sidecar. Older maintenance preserves unfamiliar files and may refuse to prune those bindings. Do not delete evidence to make rollback appear transparent. See [compatibility provenance](../tests/fixtures/lifecycle/compatibility-provenance.md).

## Polling, rendering, and process queries

`renderer="manual"` gives your formatter ownership of rendering; it does not disable polling. Use `auto_poll=false` only when your integration calls `attention.poll(window, ...)` itself. Getters and formatters do not launch a CLI to refresh data.

`attention bindings --json` is the CLI boundary for validated binding identity and liveness assessment. It is not a lifecycle replay or full-facts API, and it does not return a ready-made resume command. Build provider argv from its closed provider/session fields, and do not treat an uncertain record as proof of a live process.

Live Claude/Codex registration, shell setup, bootstrap Relay activation, and provider-paid contact remain separate operator work. An inherited launch claim is required for rich admission; tty presence alone is not an execution-generation proof.
