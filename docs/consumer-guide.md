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

`attention bindings --json` is the CLI boundary for validated binding identity and liveness assessment. It is not a lifecycle replay or full-facts API, and it does not return a ready-made resume command. Build provider argv from its closed provider/session fields, and do not treat an uncertain record as proof of a live process. Read commands (`bindings`, `inspect`, `hooks describe`) now return the existing JSON envelope by default on both terminals and pipes; `--json` remains accepted. This replaces their former bare status output. Check both `status` and `complete`; a truncated binding query can exit zero with `complete=false`. Hook stdout/exit behavior and mutating-command output defaults remain unchanged.

Live Claude/Codex registration, shell setup, bootstrap Relay activation, and provider-paid contact remain separate operator work. An inherited launch claim is required for rich admission; tty presence alone is not an execution-generation proof.

## Discover bindings for an existing socket

```sh
attention bindings --socket /absolute/path/to/mux.sock --json
attention hooks publish --socket /absolute/path/to/mux.sock --json
```

Socket-selected discovery resolves the socket before enumerating its exact realm and incarnation, then checks the original path again after reading. Stable responses add `result.scope` with exactly `realm_id` and `incarnation_id`, including when `rows` is empty. Existing `rows`, `scanned`, `returned`, `truncated`, `--provider`, `--limit` and `--all` retain their meanings. `--socket` conflicts with `--realm`.

Complete stable reads exit 0. Selected-record or directory failures and detected identity rotation exit 1. Invalid arguments exit 2. Socket resolution or required mux/process probe failures exit 3. Degraded socket responses and truncation set `complete=false`. Identity failure or rotation supplies no usable scope or rows. Empty bindings do not prove that no agents exist.

The socket query creates no state directories, takes no writer locks, and performs no publication, acknowledgement or maintenance. Its WezTerm pane query explicitly supplies `--no-auto-start`. Queries without `--socket` retain the existing response shape.

For publication, `hooks publish --socket <PATH>` is preferred. The path-valued `hooks publish --realm <PATH>` alias remains supported; using both is rejected. Publication without either selector retains pane publication and prompt-return behavior. `bindings --realm <ID>` and `sweep --realm <ID>` still take realm identifiers.

## Public consumer contracts

CLI envelopes and `HookDelivery` use consumer schema **1**. Package metadata uses manifest schema **2**; record schema **2** and wire version **2** are unchanged. Ship the manifest with its matching Rust/Lua readers. The following interfaces are source capabilities, not evidence that a machine has registered or activated them.

### Registration description

```sh
attention hooks describe --provider claude --json
attention hooks describe --provider codex --json
attention hooks describe --provider pi --json
```

`result` contains `manifest_schema`, `wire_version`, `record_schema`, `writer_version`, `provider` and `native_hooks`. Each hook has `native_event`, `arguments`, `requires_launch_identity`, `registration` (`register` or `ignored`) and `evidence` references. Prepend the resolved executable to `arguments`; a Stop row supplies `["hooks","event","claude","Stop"]`, for example. Forward original callback JSON unchanged.

`requires_launch_identity=true` qualifies **rich facts and executable consumer delivery**. It does not say every legacy callback requires inherited identity. Ignored rows are not installation registrations. Pi additionally supplies `extension_entrypoint="pi/index.ts"`; its custom native bus name is `wezterm-attention:mark`, forwarded as the `bus` writer argument. Non-Pi results omit `extension_entrypoint`. Parser coverage, synthetic fixtures, native contact and activation remain separate; the evidence references do not assert activation.

### Transient hook delivery

```sh
export ATTENTION_REPLY_FILE=/absolute/application-data/reply.json
attention hooks event claude Stop \
  --consumer /absolute/checkout/examples/reply-sink.mjs \
  --consumer-timeout-ms 1000 --include-reply
```

Repeat `--consumer` for multiple executables. Each gets the explicit positive, representable deadline, covering stdin writing and process completion. Total hook time includes each consumer's budget. Paths are absolute executable paths, with no shell command syntax or executable arguments. Normal process environment inheritance remains available to application-owned executables; the delivery JSON contains no environment dump. The sample sink uses Node through its shebang, so Node must be on the configured PATH.

`HookDelivery` contains `schema`, a fresh `delivery_id`, `scope`, `action`, `provider`, `provider_session_id`, `source_event`, `actor`, optional `correlation` and `observation_id`, `persistence`, and `reply`. Scope contains the admitted `address`, `launch_id`, and a binding `target` (`kind="binding"`, `binding_id`). `action` uses the existing provider-action vocabulary and distinguishes binding, activity, parent Stop, child presence, end, review, clear and observation-only operations. It is not a controller command or permission.

Identity is captured inside the same native application path. Delivery requires inherited launch identity and a matching provider binding. Tty-only recovery does not qualify. The label remains that admitted source even if a newer occupant appears before a consumer acts. No executable runs inside an Attention writer lock.

Persistence reports four independent fields:

| Field | What is covered |
|---|---|
| `native_state` | All native record effects selected by `action`, excluding lifecycle and flat compatibility output: binding/current pointer, activity, parent child-clear, child presence, binding end, review or clear/removal as applicable |
| `activity` | The lead activity or activity-clear subset, when selected |
| `compatibility` | Selected V1 flat projection reconciliation, including a valid already-satisfied state; a fenced reconciliation is rejected |
| `lifecycle` | This callback's requested lifecycle observation |

Each field is `not_requested`, `confirmed`, `rejected` or `unconfirmed`. Confirmed does not require new bytes when the required state already matches. Unconfirmed does not establish that no writes happened. A rejected or unconfirmed requested effect suppresses delivery. Lifecycle preparation/write failure can coexist with confirmed native and compatibility effects. `observation_id` appears only when the native application confirms writing that observation; it is never a provider request ID or controller token. Optional correlation is omitted when absent.

`reply.availability` is `not_requested`, `available`, `absent`, `unsupported`, `invalid` or `too_large`. **Only available has `text`.** Exact decoded text, including an empty string, Unicode and newlines, is available only for admitted lead Claude/Codex Stop callbacks with a string `last_assistant_message`. Missing field means absent; null or another JSON type means invalid. Other events/actors and Pi are unsupported. The whole delivery must fit Attention's hook JSON byte limit; oversized text is omitted with `too_large`, never truncated. Reply bodies enter no Attention record, diagnostic or GUI cache. The existing hook-input size limit still applies: an oversized native payload is rejected before application. Delivery `too_large` covers admitted content whose serialized delivery exceeds the bound.

| Consumer stage | Precise meaning |
|---|---|
| `not_dispatched` | No child attempted; `reason` names missing admission, rejected/unconfirmed requested persistence, or an oversized envelope |
| `not_started` | Kernel startup failed; no consumer program executed and no child exit code is invented |
| `completed` | All stdin bytes were written and the direct child exited zero; application effects are still not known |
| `failed` | Started child failed or its wait failed; an exit code is included only when actually observed |
| `stdin_failed` | Input delivery failed or the child exited before input completed; effects can have occurred |
| `timed_out` | The stdin/completion deadline expired; Attention terminated/reaped its direct child |

`effect` is `none` for not-dispatched/not-started and `possible` for every started child, including completed. `reason` is omitted outside not-dispatched; `exit_code` is omitted when unavailable. Descendants are not supervised. Delivery is attempted once per configured executable, in order; later consumers still run after failure. Repeated invocations can produce distinct delivery IDs. There is no durable queue, automatic retry or exactly-once promise.

Consumer outcomes and native persistence are reported on structured stderr; child stdout/stderr are discarded. Hook stdout stays reserved for the provider. Strict mode fails on consumer failure; non-strict mode remains provider-friendly. Malformed consumer arguments fail before native application. Without consumers, existing stdout/exit/debug behavior remains unchanged.

### Scoped headless inspection

```sh
printf '%s\n' "$SCOPE_JSON" | attention inspect --scope - --json
```

Obtain `$SCOPE_JSON` from a selected public bindings row: `{ "address": row.address, "launch_id": row.launch_id, "binding_id": row.binding_id }`. All identity components are canonical. Unknown fields are rejected. `binding_id` can be omitted or null to leave that expectation unspecified; canonical serialization omits it. Address and launch remain required. A changed launch/binding is reported, never automatically followed.

`result` is `PaneFacts`: requested `scope`, `scope_relation` (`matched`, `launch_changed`, `binding_changed`, `unavailable`), `binding`, `pane_presence`, `reader_confidence`, `binding_health`, `activity`, `binding_end`, `children`, `review`, `lifecycle` and facet-owned diagnostics. `binding` is a validated existing `BindingRow`, or null when no matched metadata is available. Null alone does not establish an unbound pane. Binding health is local evidence, not a global uniqueness or permission claim. Presence and provider binding phase remain independent: an ended provider can be in a present pane. Lifecycle validity does not by itself change the base identity/read-confidence axes; inspect its own availability and the envelope completeness.

Activity and binding-end facets contain `availability`, optional `record`, and `diagnostics`. Activity is present, absent, cleared, expired, unavailable, invalid or unsupported. Present/expired retain the validated activity record with native type/source/target, event ID, timestamps and supplied TTL/frame/label. Cleared retains the applicable clear record, whose stored fields need not include a write timestamp. Absent or failed reads omit `record`. Badge acknowledgement cannot hide this raw activity facet. Effective binding-end records retain reason, event/timestamps and optional Attention maintenance `operation_id`; older inapplicable ends are absent. That operation ID is not a controller dispatch token.

Children and review collections contain availability, eligible `count`, `evidence`, `coverage="eligible_records"` and diagnostics. A successful empty collection can have availability present and count zero. Degraded reads may retain independently validated evidence. Child eligibility follows existing clear/floor/TTL rules; zero eligible records never proves every child process exited. Children with known selection fences additionally expose `eligibility`: the protocol `ttl_ms` and optional `clear_mono_ns`/`floor_mono_ns`. Review and unbound/unresolved child collections omit eligibility. Missing fence fields are not absence evidence when collection availability is degraded.

Lifecycle uses the existing GUI observation/request/relation/floor names and `coverage="bounded_window"`. Headless availability is available, absent, unavailable, invalid or unsupported; there is no cached fallback. Missing snapshot IDs, correlations, optional native fields and floors are omitted. Arrays remain arrays when empty. Both pool floors are preserved when present. Optional badge acknowledgement is separate from raw activity and native relations.

The reader checks claim/current pointer before and after assembly and rechecks socket identity. Rotation discards the assembled facts. A stable binding does not make independently written files one atomic snapshot. Inspection does not create directories, write acknowledgements, prune records or start WezTerm. Complete valid reads, including explicit absence, exit 0; changed/degraded evidence exits 1; invalid scope/arguments exit 2. Complete means this requested response was represented, not complete history or task success.

### GUI view callback

```lua
attention.apply_to_config(config, {
  settled_title_fallback = false,
  on_view_change = function(change)
    -- Update application-owned presentation. Return promptly.
  end,
})
```

Initial/updated messages contain `kind`, GUI-local `window_id`, full `scope` and detached `view`. Scope has address, launch and a launch or binding target. Scope-lost messages contain only `kind`, `window_id` and `previous_scope`. Window context is not source identity or control permission. Unpublished/V1 input never fabricates V2 scope.

Each window has its own baseline. A confirmed source replacement emits loss before initial; ending the same binding is an update. Fresh target selection can establish a degraded new binding view. Unavailable target selection retains the last established scope only as degraded context; it does not restore old facts. Lifecycle-only, confidence and floor changes count; spinner animation alone does not. Delivery follows cache refresh and configured acknowledgement, including in unfocused windows.

Registration is once per module. Repeated apply does not replace the callback. A successful normal configuration reload starts a fresh module baseline; a window override event alone does not. Callbacks are cooperative: exceptions and reentrant delivery are contained, but an infinite callback cannot be preempted. Schedule expensive work outside the poll. With title fallback disabled, polls do no process-title sampling, title-state comparison or title-only redraw/advice. Default rendering and review/acknowledgement remain supported.

### Executable application recipes

- `examples/reply-sink.mjs` stores exact supplied content and its full source scope in an application-owned file. It does not prove the last received delivery is the newest/current reply; a resolver must check identity and own its ordering/idempotency policy.
- `examples/follow-up.lua` consumes normalized publication facts and per-window callbacks. Local dismissal updates presentation immediately and does not answer a provider question. Copy it beside `examples/wezterm.lua` when using that configuration.
- `node examples/checkpoint.mjs /absolute/attention /absolute/socket /absolute/wezterm /absolute/checkpoint.json [LIMIT]` brackets topology with two complete socket binding queries. Failure, incomplete evidence, duplicate current associations or socket rotation preserves the old checkpoint. Missing association stays null/unknown. Topology uses explicit `--no-auto-start`; no identity hashing or private record paths are copied. The two query durations are reported in milliseconds.
- `node examples/inspect.mjs /absolute/attention /absolute/socket [LIMIT]` performs bounded discovery followed by exact-scope inspections. Incomplete discovery stops before inspection; degraded or changed scope fails rather than following another occupant.

The Node process recipes use 10-second subprocess deadlines and an 8 MiB output budget. These are example I/O budgets, not agent-state thresholds. They retain failure instead of inferring absence. Checkpoint replacement uses a private temporary file plus rename; no cross-file atomicity or disk-durability guarantee is added. Tests supply synthetic topology/content and production CLI responses. None of these examples installs hooks, chooses a live profile, accesses the clipboard or grants controller permission.