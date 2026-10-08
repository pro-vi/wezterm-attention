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
| `frame` | The frame of the pane's activity, which `get_attention` returns second: the one its writer stored, as `attention mark --frame` does, or, for a `thinking` pane whose writer stored none, one from the clock, a new one each second. `nil` when the review flag outranks the activity |
| `activity_type`, `event_id`, `source` | Eligible lead activity, independently of review |
| `turn_end_held`, `turn_end_quiet` | Whether the shown turn end is held by a [hold check](#pre-write-hold-checks), or carries a quiet note; both false when neither applies |
| `review` | Owner-scoped review presence |
| `subagents`, `subagents_uncertain` | How many of the pane's sub-agents are running, and whether that count could not be read; `subagents` is 0 when it could not |
| `provider`, `binding_id`, `binding_phase` | Validated provider binding; a quiet binding can still identify its provider |
| `address`, `launch_id`, `marker_id` | Full realm/incarnation/pane address, launch, and the scalar pane id `pane_marker_id` returns |
| `pane_presence`, `reader_confidence`, `binding_health` | Read assessment; preserve uncertainty instead of treating it as absence |
| `lifecycle` | Optional bounded observations and derived request evidence |

Nil remains nil and false remains false. Internal records, cache keys, formatter state, deadlines, and root diagnostics are private. The scalar `get_attention` returns `type` and `frame` alone; read the other fields from the view.

The view's `binding_health` comes from the plugin's own read of the pane's records, not from the rule `bindings` and `inspect` share, and it can differ from theirs for the same binding. It never says `conflicted`, since the plugin does not look for the session at other pane addresses. Any diagnostic from its read, an unreadable review file included, makes it `invalid`, or `future_schema` when one of them is.

Controller ownership and permissions belong to consumers. Key application-owned policy by the full address, launch and binding identity from `get_attention_view` or `PaneFacts`; use `on_view_change` to refresh custom presentation. Attention records and views contain no controller-ownership flag, and the bundled renderer does not filter activity by controller ownership.

## Lifecycle availability

| `lifecycle.availability` | Interpretation |
|---|---|
| `available` | A current lifecycle file of the selected scope was read and validated |
| `cached` | An unavailable read reused the same scoped, previously validated file |
| `absent` | Neither lifecycle file exists at the selected scope |
| `unavailable` | The read failed and there is no matching usable cache |
| `invalid` | A successful read returned malformed or contradictory data |
| `unsupported` | The record declares a future schema |

Missing, invalid, unsupported, and unavailable data are not an empty pending-request list. A successfully read invalid/future file never falls back to older cached facts. Optional lifecycle failure does not invalidate an otherwise valid activity badge. The next hook that adds an observation moves an invalid file aside and starts a new one, whose retention floors sit just below that observation: what the old file held then reads as evicted.

The facet contains `snapshot_id`, a flat `observations` array, `requests`, `retention_floors`, at most eight diagnostics, and an exact `badge_acknowledgement` when one matches the stored activity. Its observations are the lead's that the pools of `lifecycle.json` still hold and the children's that the pools of `children-lifecycle.json` still hold, not the binding's whole history. Each observation names its pool: `requests` or `general` for a lead observation from `lifecycle.json`, `child_requests` or `child_general` for one from `children-lifecycle.json`.

`retention_floors` has one key per file and pool: `lead_requests` and `lead_general` for `lifecycle.json`, `child_requests` and `child_general` for the children's file. A key is present once that file's pool has evicted evidence, and no key covers both files: check both keys of a pool to ask whether either file evicted some of it. In a `lifecycle.json` that still holds children's observations from before children had their own file, a lead floor may mark the eviction of children's observations, whether it was set before that change or by a later lead observation. `snapshot_id` identifies `lifecycle.json` only: a change to the children's file keeps it, so it is not a revision of the whole facet.

The facet is built from both files, and `lifecycle.json` decides first. While it is `unavailable`, `invalid` or `unsupported`, `availability` says so and no children's evidence is shown. Otherwise `availability` is its status, and a children's file that cannot be used is left out and reported in the diagnostics. Without a `lifecycle.json`, the children's file's status is the facet's, so a binding whose only lifecycle evidence is its children's is `available`. The plugin shows a file it could not read again as the copy it read last, with a diagnostic, which makes `availability` `cached` when that file is `lifecycle.json`; `attention inspect` reads once and has no such copy.

## Which observations end a turn

No one kind marks the end of a turn. Read it per provider, from observations whose `actor.kind` is `lead`; a sub-agent's observations do not end the lead's turn.

| Provider | Kind | Native event |
|---|---|---|
| Claude | `response_finished` | `Stop` |
| Claude | `attempt_outcome` | `StopFailure`, which Claude Code 2.1.284 sends instead of `Stop` when an API error ended the turn |
| Codex | `response_finished` | `Stop` |
| Codex | `user_interrupt` | `Interrupt`; Codex (source at commit `985cf47a4`) runs no `Stop` after one |
| Pi | `run_settled` | `agent_settled`, which Pi 0.85.1 sends once no automatic retry, compaction or queued message will continue the run |

Pi's `attempt_outcome` is not a turn end. It comes from a `message_end` whose assistant message errored or was aborted. Pi still sends `agent_settled` when the run ends, and after an error it can retry first; the turn ends at that `run_settled`. The two `attempt_outcome` producers differ in `source_event`, `StopFailure` or `message_end`.

A `Stop` hook can block the stop, and the agent then keeps working in the same turn without a new prompt; its next `response_finished` carries `stop_hook_active: true`. Take the latest turn-end observation, not the first.

Some turns end with no observation. A Claude turn the user stops with Esc sends no event (see [accepted limitations](accepted-limitations.md#pressing-esc-in-claude-code-leaves-thinking-on-the-tab)). A Codex turn that ends on an API error runs neither `Stop` nor `Interrupt` (see [accepted limitations](accepted-limitations.md#a-codex-turn-that-ends-on-an-api-error-leaves-thinking-on-the-tab)).

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

The runnable [consumer example](../examples/follow-up.lua) chooses a symbolic appearance:

- `follow_up`: an available nonblocking publication has not been dismissed by this consumer.
- `base`: no retained publication calls for a new appearance under this policy; this is not a verified all-clear.
- `unknown`: missing/uncertain data, an inactive binding, or lost evidence prevents that decision.

```lua
local consumer = dofile("/your/checkout/examples/follow-up.lua").new()
local appearance = consumer.appearance(attention.get_attention_view(pane))
-- Your UI maps appearance to its own color or notification.
-- When the user dismisses what this consumer displayed:
consumer.dismiss()
```

One consumer from `new()` follows one pane scope: a view from another address, launch or binding resets it. For more than one pane, use `for_windows()`, which keeps one consumer per window and scope; pass its `on_view_change` to `apply_to_config` and read `appearance(window_id, scope)`.

Dismissal uses only the last displayed publication IDs in the full address/launch/binding scope. A Q2 that arrives later is not dismissed by a click rendered for Q1. No shared record, acknowledgement, or other consumer is changed. This is bounded private memory, not durable cross-GUI acknowledgement. Binding changes reset the scope; ended bindings do not retain a visible follow-up tint.

You can choose a different policy for approval, review, or outcome facts. Keep activity, review and child count independent; do not reuse `type` as if it were all three. Rank panes using your own purpose, but retain read confidence and full identity with the selected result.

## A marker records an event, not a session

Attention writes a pane's activity when a provider callback arrives: a prompt submitted, a tool about to run, a turn stopped, a notification raised. Nothing writes one because an agent is running. `SessionStart` for Claude and Codex, and `session_start` for Pi, write a binding record and no activity at all.

So a pane can run an agent and have no activity record: its turn ended and prompt return cleared the watermark, the human looked at the tab and acknowledged it, the record aged past its TTL, or the agent has produced nothing since it started. The absence of a marker means "no standing event here". It never means "no agent here", and a consumer that reads it as an inventory will undercount.

Bindings answer a different question and are the closest thing here to a session list, but they are not a process list either. A session whose provider hooks were registered after it started never sent `SessionStart`, so it has no binding and Attention holds no record of it whatsoever. One consumer measured this on 2026-09-19 across 42 live panes: process inspection found 27 panes running an agent, marker files 17, bindings 14. No pane had a marker that process inspection missed, and the provider never disagreed where both could see it.

A consumer that needs to know which panes are running an agent must inspect processes itself, and can use Attention to say what those agents are doing. Attention reports what was announced to it; it cannot report what was not.

## Storage and retention

A binding keeps the lead's observations in `lifecycle.json` and its children's in `children-lifecycle.json`, each with separate request/general pools. Each pool has at most 64 observations, 122,880 compact UTF-8 bytes, and its own monotonic retention floor. One observation's native fields are at most 2,048 bytes, and the whole observation, with the `turn_end` that Attention adds to a turn end, at most 18,432 bytes; each raw file is capped at 262,144 bytes and nine container levels.

Children's traffic cannot evict the lead's evidence, however much there is: the lead's observations leave only when later lead observations displace them. That is isolation, not freshness. The newest lead observation shown can still be older than the lead's last hook, because the writer refuses, for example, an observation that repeats one it holds, one at or below its pool's floor, one older than everything its full pool keeps, or one it cannot write before the lock times out. Children share their file's pools, so a busy child can evict another child's evidence.

Children's observations that a writer put in `lifecycle.json` before children had their own file stay there, and the file stays valid, until lead observations displace them. Readers leave them out of the facet with no diagnostic, so until then such a file shows fewer lead observations than its pool holds. An observation id is unique within its file; readers do not compare ids across the two files.

An observation that its own insertion would evict is reported `rejected`, not `confirmed`, and a pool already holding 64 observations at one timestamp refuses another at that timestamp. Generic tool traffic cannot evict request evidence. Request traffic can still evict older requests or their outcomes. Whole equal-timestamp groups are removed with that pool's floor in one atomic snapshot replacement. There is no replay cursor, complete history promise, lifecycle TTL, or universal `pending_count`.

Activity and the two lifecycle files are separate files, not a multi-file transaction. A crash may leave newer activity with older facts, and a poll can read the two lifecycle files at different moments. Failures report incomplete work; retries re-read actual records. Do not join files by timestamp or assume a shared snapshot ID.

The v2 records are the only format the plugin reads. 0.6 read one small JSON file per pane id at the top of the state directory, `<id>`, with an `<id>.ack` beside it, and untagged builds also wrote `<id>.agents` and `<id>.review`; nothing reads or collects those files now. Use matching current Attention writers and readers; intermediate development builds are not supported compatibility targets.

## Polling, rendering, and process queries

`renderer="manual"` gives your formatter ownership of rendering; it does not disable polling. Use `auto_poll=false` only when your integration calls `attention.poll(window, ...)` itself. Getters and formatters do not launch a CLI to refresh data. The plugin runs the `attention` command in four places: a poll of the focused window runs it once for each new `stop` or `notify` activity it acknowledges; a review-key press runs it once for each pane it flags or clears; the GUI runs `attention tab-source` to learn its own mux identity, until it has an answer or its retries run out; and a poll that finds panes of a mux domain that have not published their identity runs `attention hooks publish --socket` in the background, with retries.

`attention bindings --json` is the CLI boundary for validated binding identity and liveness assessment. It is not a lifecycle replay or full-facts API, and it does not return a ready-made resume command. Build provider argv from its closed provider/session fields, and do not treat an uncertain record as proof of a live process. Queries (`bindings`, `tabs`, `inspect`, `doctor`, `sweep`) and `hooks describe` return the JSON envelope by default on both terminals and pipes; `--json` remains accepted. Check both `status` and `complete`; `bindings` returns every matching row unless `--limit` caps them, and one that `--limit` truncated sets `complete=false` and also says so on stderr (`returned N of M; raise or drop --limit`) while stdout stays JSON. Exit codes are listed under [Exit codes and envelopes](#exit-codes-and-envelopes). On `bindings`, `complete` is about the rows: it is false when rows were dropped, when a state directory could not be read (a `state_permissions` diagnostic, "state directory could not be read", with a `path` context), and in `--socket` mode also on any diagnostic: a probe that did not answer, a selected record or directory that could not be read, or a `binding_conflict`. Other diagnostics do not make a realm-wide answer incomplete. Every diagnostic is listed, however many there are. A record diagnostic carries a `path` relative to the state root, and a presence diagnostic carries `realm_id`, `incarnation_id` and `pane_id`. A `binding_conflict` diagnostic carries `provider`, `provider_session_id` and `addresses`, the pane addresses that hold that session. Every command reads the server behind a recorded incarnation by one rule, set out in the [record contract](record-contract.md#trust-boundary). A pane whose server is shown to have exited (its `gui-sock-<pid>` GUI process is gone, or no process carries the pane) reads `verified_absent`, with no diagnostic. A pane whose socket file no longer exists, with nothing to show the server gone, reads `unavailable` with a `socket_gone` diagnostic ("mux socket no longer exists"); one whose socket path now holds a different socket reads `unavailable` with `incarnation_changed`; one whose socket still carries the incarnation but fails its listing and refuses a connection reads `unavailable` with `socket_refused` ("mux socket refuses connections"). A refusal is not an exit: a live server whose accept queue is full, or which stopped accepting, refuses too. Neither is `probe_unavailable`: no probe failed. `bindings` gives one such diagnostic per pane, with `realm_id`, `incarnation_id` and `pane_id`; `inspect` answers such a scope with `scope_relation` `unavailable` and the same code in its `scope` facet, and `bindings --socket` on a path with no socket there fails with `socket_gone`. A pane listing that fails or times out on a socket that still carries the incarnation and does not refuse is `realm_unavailable`, with the listing's own message. The socket is read again after the look at the pane: when another server took it meanwhile, what the look found is not used, and the pane reads as it would had the change come first.

A realm-wide `bindings` applies `--realm` and `--provider` before it asks any socket, and asks the remaining sockets in parallel. A row outside the filter that shares a provider session with a returned row is still assessed, so a conflict across realms still shows. `result.timing_ms` says where the call's wall time went, in whole milliseconds: `pane_list` inside `wezterm cli list` (for a realm-wide call, the wall time of the parallel batch), `process_list` inside the process probe, `records` in finding and reading the records. It is on every `bindings` and `inspect` answer that carries a `result`; log it next to a slow call and the phase is named. An envelope printed for an error has an empty `result` object, so it has no timing.

Live Claude/Codex registration, shell setup, activation of any application that consumes these facts, and provider-paid contact remain separate operator work. Rich admission (lifecycle observations and consumer delivery) needs a claimed launch: a shell's claim the agent inherited, or on macOS the agent's own claim, which its process proves again under the writer's locks at every event. A terminal match alone admits nothing.

## Discover bindings for an existing socket

```sh
attention bindings --socket /absolute/path/to/mux.sock --json
attention hooks publish --socket /absolute/path/to/mux.sock --json
```

Socket-selected discovery resolves the socket before enumerating its exact realm and incarnation, then checks the original path again after reading. Stable responses add `result.scope` with exactly `realm_id` and `incarnation_id`, including when `rows` is empty. Existing `rows`, `scanned`, `returned`, `truncated`, `--provider`, `--limit` and `--all` retain their meanings. `--socket` conflicts with `--realm`. The rows are that server's, but a rival binding of the same provider session is looked for across the whole store, as `inspect` does, so a row's `binding_health` reads the same in both; records outside the selected server that cannot be read are not reported. Both find those rivals through the session index described in the [record contract](record-contract.md#identity-and-paths), so their cost does not grow with the bindings of other sessions once the index is complete.

Selected-record or directory failures, detected identity rotation, socket resolution or required mux/process probe failures, and a `binding_conflict` on a returned row make the answer incomplete; see [Exit codes and envelopes](#exit-codes-and-envelopes). Identity failure or rotation supplies no usable scope or rows. Empty bindings do not prove that no agents exist.

The socket query creates no state directories, takes no writer locks, and performs no publication, acknowledgement or maintenance. Its WezTerm pane query explicitly supplies `--no-auto-start`. Queries without `--socket` retain the existing response shape.

For publication, `hooks publish --socket <PATH>` takes an existing socket path and is the only selector. Publication without it retains pane publication and prompt-return behavior. `bindings --realm <ID>` and `sweep --realm <ID>` are a different option that takes a 64-character realm identifier, not a path.

Use `attention bindings --fields address,provider,current` to select top-level
row fields. Field selection does not change query
work, row limits, scope, diagnostics, completeness or exit codes. `address` stays a
whole object. Optional fields that were absent stay absent, so a selected row can
be `{}`. Without `--fields`, rows are unchanged.
See `bindings --help` for accepted names. Unknown names, empty comma components,
nested paths and wildcards are usage errors.

## Read the drawn tab order

```sh
attention tabs
```

A GUI window attached to a mux server mirrors the server's tabs under its own numbers, and those are the numbers the tab bar prints. They are not the order of `wezterm cli list`, and no derivation from it recovers them. The tab bar therefore publishes what it drew, one file per identified GUI source and window at `<state root>/tabs/<incarnation id>-<window id>.json`, and `attention tabs` returns them in the ordinary envelope: `result.windows` holds one entry per window with `window_id`, `source`, `published_at_ms` and `tabs`, and each tab carries `number`, the whole `text` the bar drew, and `marker_ids` — the IDs the plugin already uses for those panes, already translated out of the window's local numbering. The `text` leaves out two things the bar draws: a spinner is published at its first frame, so the file is not rewritten every second, and the plugin's `⚠ rebuild attention` warning, which names no tab, is left out. A pane a launch has claimed is `v2:<realm_id>:<incarnation_id>:<pane_id>`; a pane no launch has claimed is its canonical decimal pane id. A pane appears once a poll has identified it.

The window entries are sorted by window ID and then source identity (legacy first on a tie); the tabs inside one are in the order the bar draws them, which is the point of the file. Every tab is listed, including tabs holding no agent, which is why this is a separate command from `bindings`.

**There is no freshness contract.** The file is written when a window's composed list changes, so `published_at_ms` is when the bar last drew something different, not when anything checked. Nothing refreshes it while the bar is idle. When a window closes, the next poll of another window in the same WezTerm process removes its file. A window whose whole WezTerm process has exited leaves its last file behind. `attention sweep` removes a tab-order file only when it names no tab, or when every pane it names is verified absent. The GUI's own local panes read verified absent once its `gui-sock-<pid>` process is gone, so a file naming only claimed local panes is collected. Panes of a mux server the GUI was attached to are usually still running, and a file naming them stays. A pane no launch has claimed is named by a bare decimal id, and a file naming one is kept, because such an id names no realm to ask; one plain shell pane keeps its window's file. A file that changed while sweep was deciding is kept too (reason `changed`). See [accepted limitations](accepted-limitations.md#what-sweep-leaves-behind). Use this to describe tabs and to resolve an ordinal — "the second `api` tab" — where a wrong answer is visible to the person who asked. To act on a tab, ask the GUI: inside WezTerm, `mux_window:tabs_with_info()` returns the same order live.

What publishes is the `format-tab-title` handler the plugin registers, which exists in the default `renderer="tab"` mode and not in `renderer="manual"`. A manual renderer draws its own tabs and publishes none of them, so a consumer of a manually rendered window sees the same absence as one whose publisher is not running.

Every setup publishes, including a plain local WezTerm where the drawn number equals the derived one. A consumer cannot tell a simple setup from a publisher that is not running, because the file is absent in both, and deriving the number is right in one case and wrong in the other.

A file that cannot be read, declares a later schema, is filed under a window it does not name, or carries a field this schema does not have is reported as a diagnostic and left out; the windows that did read are still returned, and `complete` is false. A symlinked `tabs/` directory is refused as a whole (`record_invalid`, "tab publication directory is a symlink"), and sweep never deletes through it.


The plugin captures its own GUI socket identity outside the renderer. Schema 2 records that `source` as a canonical socket path plus realm and incarnation IDs. Schema-1 decimal filenames remain readable with `source: null`; the plugin keeps publishing that legacy form if its identity helper is unavailable. Existing legacy files are not guessed into the new namespace or removed on upgrade; a window's own legacy file is removed once the same process writes a sourced file for that window. Source incarnation plus window ID identifies a new publication; the window number alone does not. `wezterm.plugin.update_all()` updates the plugin before you can rebuild the command, so rerun `scripts/install-cli.sh` right after it; until then the plugin keeps publishing whatever its identity helper allows, which can be the legacy form.

Every `attention tabs` invocation also returns `window_check` on each window. It
checks the recorded GUI socket incarnation before and after one no-auto-start
inventory query per source. It never substitutes the caller's socket or a remote
pane's mux, and it never changes publication files.

| `window_check.status` | Meaning |
|---|---|
| `present` | At least one pane was listed for this window in the publishing GUI's mux. |
| `not_listed` | That inventory listed no panes for this window. An empty or transitional window is not ruled out. |
| `unavailable` | The source is unrecorded, changed, gone, unavailable, or returned an invalid inventory; `reason` distinguishes these cases (`source_unrecorded`, `source_changed`, `socket_gone`, `probe_unavailable`, `inventory_invalid`). `socket_gone` is returned in two cases: the recorded socket file no longer exists, whether or not the GUI process still runs, or the inventory failed and the socket's `gui-sock-<pid>` process no longer exists, as `bindings` reads that GUI's panes absent. Only the second shows the GUI exited. Any other inventory that failed is `probe_unavailable`, even when the socket refuses: a refusal does not show the GUI gone. |

`checked_at_ms` is the completion time of that check, not a freshness promise.
The saved `published_at_ms`, text and order are unchanged. A successful check
proves neither current tab order nor visibility in the current workspace; validate
any action against the GUI when acting. Legacy files return
`unavailable/source_unrecorded`. Failed checks stay on their individual windows:
`complete` and the top-level diagnostics still describe publication-read coverage.
Check `window_check.status` on the window you intend to use. All parsed windows
remain in the response, including not-listed and unavailable ones.

## Public consumer contracts

Attention records use schema **3**. CLI envelopes and `HookDelivery` use schema **1**; source-identified tab publications use their independent schema **2** (legacy schema **1** remains readable), package metadata uses manifest schema **2**, and pane identity uses wire version **2**. These identify separate data formats, not supported product editions. A published fact that is not a v2 record — one with no pane address to be validated against and no ordering fence — carries its own schema field and is versioned on its own, rather than entering `protocol/v2.json`: the manifest's `record_schema` governs the addressed record tree, and coupling anything else to it would make an unrelated record change refuse a valid file. Use a fresh Attention state root and fresh supported launches for activation; existing state is not automatically migrated or deleted. `attention sweep --json` previews retention and `attention sweep --apply` applies it, making up a fresh operation id and reporting it in `result.operation_id`; the [record contract](record-contract.md#trust-boundary) says what a run after an interrupted one does. Ship the manifest with its matching Rust/Lua readers. Source capabilities do not establish live registration or activation.

### Exit codes and envelopes

Every JSON envelope has `schema`, `command`, `status`, `complete`, `result` and `diagnostics`. Each diagnostic has `code`, `message` and `context`, and a usage error's also has `help`, which names the command's `--help`. An envelope printed for an error has `complete=false`, `status` `usage_error` or `unavailable`, and an empty `result` object, `{}`; to tell an error from an answer, read `status`, not whether `result` is there. Every JSON document the CLI prints escapes U+0080–U+009F as `\u0080`-style escapes, which decode to the same value, so a C1 control character never reaches a terminal raw.

The query commands (`bindings`, `tabs`, `inspect`, `doctor`, `sweep`) exit:

| Exit | Meaning |
|---|---|
| 0 | `complete=true` |
| 1 | The answer is incomplete (`complete=false`), or the command failed |
| 2 | The command line of a non-hook command is wrong: an unknown flag, a bad value, a conflicting option |

No command exits 3. Diagnostics alone never change the exit code: a complete answer that carries diagnostics exits 0, so `attention bindings && …` takes the success branch whenever the rows are complete. A `bindings` answer that `--limit` truncated is incomplete and exits 1. For `tabs`, every diagnostic is a window left out of the answer, so any diagnostic there makes it incomplete. `doctor` and `sweep` list every finding and detail, and exit 0 with findings beside a complete report; an unavailable probe makes the report incomplete and exits 1. So does a `sweep --apply` step that failed: a removal or write that did not happen, or a record it could not read to decide on. A pane whose socket is gone, replaced or refusing, with nothing to show its server gone, is kept history: a finding, not an unavailable probe, so it leaves `doctor` and `sweep` complete. They report it once per code for the run, as one `socket_gone`, `incarnation_changed` or `socket_refused` diagnostic whose `context.incarnations` lists each incarnation it holds with `realm_id`, `incarnation_id`, `path` (its directory, relative to the state root) and `pane_count`, so a long history adds one diagnostic per code, not one per pane. A mux whose socket still carries the incarnation, does not refuse, and whose pane listing fails or times out (`realm_unavailable`) is an unavailable probe in both, also when the only pane asked is one a tab-order file names: the report is incomplete and exits 1. `inspect` is the exception to the rule on diagnostics: its answer is complete only when the scope matched and its diagnostics list is empty, so one unreadable review file makes a matched answer incomplete and exit 1, while the row's `binding_health` stays `valid`. `hooks publish` says `complete=false` and exits 1 when it skipped a publication, and says `complete=true` and exits 0 otherwise. The envelope `hooks event` writes to stderr, with `--debug` or `--consumer` or after a failed hold or quiet check, says `complete=false` when the event was ignored, conflicted or only partly applied, a consumer did not complete (an event skipped on purpose dispatches none), or a check failed: exactly when `--strict` makes it exit 1. One hook command prints `complete` without following this rule: `hooks claim --json` says `complete=false` while its publication is pending and still exits 0, because the claim was made. Every error other than a usage error exits 1, including `mark`, `hooks claim` and `hooks publish`. The launcher `bin/attention`, when the Rust binary has not been built, exits 1 for every command except `hooks`.

Without `--json`, `mark` and `hooks publish` (unless `--quiet`) print only the status word on stdout, and each diagnostic on stderr as `attention: <code>: <message>`, so a script reading the word reads what it always did. A closed stdout, as in `attention bindings | head -1`, is ignored and the command keeps its own exit code. So is a stderr nobody can write to.

Hook commands never exit 2, because Claude Code and Codex read exit 2 as "block": a prompt is dropped, a tool is denied, or a Stop hook makes the agent loop on the error text.

- `hooks event` exits 0 with the reason on stderr, or 1 under `--strict`. That includes a command line it cannot parse, such as a misspelled or removed flag, and a malformed `--consumer` or `--consumer-timeout-ms`.
- An event the integration knows and records nothing for on purpose, such as Claude Code's `idle_prompt` notification, is `skipped`, not a failure. Without `--consumer` it prints nothing unless `--debug` is given, and exits 0 under `--strict`; with `--consumer` its consumers are not dispatched (`no_admitted_scope`), and `--strict` exits 0 there too. The [record contract](record-contract.md#ordering-and-wall-age) lists these events.
- A usage error of `hooks describe`, `hooks claim` or `hooks publish`, or an unknown `hooks` subcommand, exits 1. With `--json`, the envelope's `command` names the subcommand.
- `bin/attention` without the binary exits 0 for any `hooks …` command, or 1 with `--strict`.

### Compatibility

**What changes a schema number.** Within CLI envelope schema 1, a release may add a field to any object, and a value to any set except these closed ones: `status`, `scope_relation`, `pane_presence`, `binding_phase`, `binding_health`, `reader_confidence`, `provider`, `actor.kind`, and a record's or the lifecycle's `availability`. A consumer ignores a field it does not know and treats an unknown value in any other set, such as a diagnostic's `code` or an observation's `kind`, as unrecognized. A new value in a closed set raises the schema number. So does a removed or renamed field, or a field whose meaning changes.

**The product version.** `attention --version` and `hooks describe`'s `writer_version` follow semantic versioning from 1.0.0. A release that adds to a public contract raises the minor version, and one that raises any schema number or wire version raises the major version. A release with only fixes raises the patch. The version changes only in a release commit, so a build between releases reports the last release's version. `attention --version` also names the build's commit; `writer_version` does not. A consumer that needs an unreleased change pins the commit.

**Socket identity.** The `realm_id` and `incarnation_id` in every pane address, and in the `result.scope` of a `bindings --socket` answer, can be computed without a subprocess. Both are lowercase hexadecimal SHA-256 digests. `realm_id` is the digest of the socket's canonical path (symlinks resolved) as UTF-8. `incarnation_id` is the digest of four parts, in order: the `realm_id` text, and the socket file's device number, inode number and change time in nanoseconds (`st_ctime` × 10⁹ + `st_ctime_nsec`), those three written in decimal. Each part is fed as its byte length, an 8-byte big-endian integer, followed by its UTF-8 bytes. Attention refuses a socket path that is not absolute or not UTF-8, a file that is not a socket or is not owned by the effective user, and a negative change time. A change to this recipe raises the wire version.

**Observation kinds.** As of 1.0.0, Attention writes these `kind` values: `prompt_submitted`, `tool_preflight`, `tool_result`, `approval_requested`, `automatic_denial`, `response_finished`, `run_settled`, `attempt_outcome`, `user_interrupt`, `elicitation_requested`, `elicitation_action_selected`, `notice`, `compaction_attempted` and `compaction_succeeded`. The set is open; a release that adds one lists it in the changelog.

**Diagnostic codes.** As of 1.0.0, Attention emits these `code` values: `bad_usage`, `binding_conflict`, `child_active_after_parent_clear`, `claim_stale`, `clock_skew`, `future_schema`, `identity_unpublished`, `incarnation_changed`, `integration_version_mismatch`, `outside_pane`, `probe_unavailable`, `realm_unavailable`, `record_invalid`, `self_claim_parent_unverified`, `session_detached`, `socket_gone`, `socket_refused`, `state_permissions` and `unsafe_tty`. The set is open, like the observation kinds. A build lists the codes it knows in `attention doctor`'s `result.diagnostic_codes`; they are the `enums.diagnostic_codes` of the `protocol/v2.json` it was built from.

### Registration description

```sh
attention hooks describe --provider claude --json
attention hooks describe --provider codex --json
attention hooks describe --provider pi --json
```

`result` contains `manifest_schema`, `wire_version`, `record_schema`, `writer_version`, `provider` and `native_hooks`. Each hook has `native_event`, `arguments` and `registration` (`register` or `ignored`). Register a Claude Code or Codex row as one shell command that sets `WEZTERM_ATTENTION_HOST_PID=$PPID` and then `exec`s the resolved executable with the row's `arguments`, as the README's blocks do: a Stop row supplies `["hooks","event","claude","Stop"]`, registered as `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Stop`. A hook registered as the executable and its arguments alone carries no host pid, so on macOS every session start of an agent that no shell claimed for is refused (`self_claim_parent_unverified`) and the agent never claims its pane. Forward original callback JSON unchanged.

Ignored rows are not installation registrations. Pi additionally supplies `extension_entrypoint="pi/index.ts"`; its custom native bus name is `wezterm-attention:mark`, forwarded as the `bus` writer argument. Non-Pi results omit `extension_entrypoint`. A row says what to register, not that it is registered or active on this machine.

### Transient hook delivery

```sh
export ATTENTION_REPLY_FILE=/absolute/application-data/reply.json
WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude Stop \
  --consumer /absolute/checkout/examples/reply-sink.mjs \
  --consumer-timeout-ms 1000 --include-reply
```

Repeat `--consumer` for multiple executables. Each gets the explicit positive, representable deadline, covering stdin writing and process completion. Total hook time includes each consumer's budget. Paths are absolute executable paths, with no shell command syntax or executable arguments. Normal process environment inheritance remains available to application-owned executables; the delivery JSON contains no environment dump. The sample sink uses Node through its shebang, so Node must be on the configured PATH.

`HookDelivery` contains `schema`, a fresh `delivery_id`, `scope`, `action`, `provider`, `provider_session_id`, `source_event`, `actor`, optional `correlation` and `observation_id`, `persistence`, `reply`, and `prompt`. Scope contains the admitted `address`, `launch_id`, and a binding `target` (`kind="binding"`, `binding_id`). `action` uses the existing provider-action vocabulary and distinguishes binding, activity, parent Stop, child presence, end, review, clear and observation-only operations. It is not a controller command or permission.

Identity is captured inside the same native application path. Delivery requires a claimed launch, as for lifecycle observations above, and a matching current provider binding. A consumer run for an event admitted through the agent's own pane claim inherits no `WEZTERM_ATTENTION_LAUNCH_ID`, so `attention mark` called from it is refused. The label remains that admitted source even if a newer occupant appears before a consumer acts. No executable runs inside an Attention writer lock.

Persistence reports three independent fields:

| Field | What is covered |
|---|---|
| `native_state` | All native record effects selected by `action`, excluding lifecycle output: binding/current pointer, activity, the child presence set, binding end, review or clear/removal as applicable |
| `activity` | The lead activity or activity-clear subset, when selected |
| `lifecycle` | This callback's requested lifecycle observation |

Each field is `not_requested`, `confirmed`, `rejected` or `unconfirmed`. Confirmed does not require new bytes when the required state already matches. Unconfirmed does not establish that no writes happened. A rejected or unconfirmed requested effect suppresses delivery. Lifecycle preparation/write failure can coexist with confirmed native effects. `observation_id` appears only when the native application confirms writing that observation; it is never a provider request ID or controller token. Optional correlation is omitted when absent.

`reply` and `prompt` always appear. Both use `HookContent`: `availability` is `not_requested`, `available`, `absent`, `unsupported`, `invalid` or `too_large`. **Only available has `text`.** Each flag is independent: omitting `--include-prompt` yields `prompt={"availability":"not_requested"}`, even when `--include-reply` is set. A missing native field means absent. A Codex `last_assistant_message` of `null` also means absent: Codex sends null when a turn has no final text. Any other null, including a null `prompt`, or another JSON type means invalid. An empty string is available, and Unicode/newlines are preserved exactly.

- `--include-reply`: admitted lead Claude/Codex `Stop`, from `last_assistant_message`.
- `--include-prompt`: admitted lead Claude/Codex `UserPromptSubmit`, from `prompt`.
- Other events, child actors and Pi return unsupported for requested content. Pi's bundled extension does not register executable consumers or forward input text.

For a submit callback carrying `"prompt":"Check 中文\n"`, these are the exact content fields when both flags are set:

```json
{"prompt":{"availability":"available","text":"Check 中文\n"},"reply":{"availability":"unsupported"}}
```

Register a consumer for submit callbacks using:

```sh
WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event claude UserPromptSubmit \
  --consumer /absolute/application/prompt-consumer \
  --consumer-timeout-ms 1000 --include-prompt
```

The provider supplies callback JSON on stdin; the executable receives the full scoped delivery. Use `codex` for its equivalent callback. A lead submit starts the turn's `thinking` activity, so `native_state`, `activity` and `lifecycle` are all confirmed. A child actor's submit stays observation-only: `lifecycle` is confirmed while the other two are not_requested. Exact means the decoded provider callback string, not original keystrokes, complete multimodal input, or proof that the model processed it. Consumers own markers, correlation, receipts and acceptance decisions. A missed delivery leaves content unavailable for recovery from Attention records.

The whole delivery must fit Attention's hook JSON byte limit. On overflow, available content becomes too_large with text omitted, never truncated; all other availability values stay intact. If metadata alone still exceeds the bound, delivery is not_dispatched. Prompt/reply bodies enter no Attention record, diagnostic or GUI cache. Oversized native stdin is rejected before application; delivery too_large describes admitted content whose serialized envelope exceeds the bound.

| Consumer stage | Precise meaning |
|---|---|
| `not_dispatched` | No child attempted; `reason` names missing admission, rejected/unconfirmed requested persistence, or an oversized envelope |
| `not_started` | Kernel startup failed; no consumer program executed and no child exit code is invented |
| `completed` | All stdin bytes were written and the direct child exited zero; application effects are still not known |
| `failed` | Started child failed or its wait failed; an exit code is included only when actually observed |
| `stdin_failed` | Input delivery failed or the child exited before input completed; effects can have occurred |
| `timed_out` | The stdin/completion deadline expired; Attention terminated/reaped its direct child |

`effect` is `none` for not-dispatched/not-started and `possible` for every started child, including completed. `reason` is omitted outside not-dispatched; `exit_code` is omitted when unavailable. Descendants are not supervised. Delivery is attempted once per configured executable, in order; later consumers still run after failure. Repeated invocations can produce distinct delivery IDs. There is no durable queue, automatic retry or exactly-once promise.

Consumer outcomes and native persistence are reported on structured stderr; child stdout/stderr are discarded. Hook stdout stays reserved for the provider. Strict mode fails on consumer failure, except that an event skipped on purpose owes its consumers nothing; non-strict mode remains provider-friendly. Malformed consumer arguments fail before native application. Without consumers, existing stdout/exit/debug behavior remains unchanged.

A consumer that exits zero after deciding to do nothing has no channel to say why, and that is deliberate rather than an oversight. Its stderr is not passed through under `--debug`, because the hook's stderr there is a single JSON document a reader parses, and interleaved child output would stop it parsing. Its stderr is not captured into the `consumers[]` entry either, because the delivery envelope carries prompt and reply content, and a consumer that echoes any of it would return that content to a channel the provider may log. Write reasons to a log file the consumer owns, or to a path passed in its own configuration; do not assume anyone sees stderr.

#### Pre-write hold checks

`attention hooks event claude Stop --hold-check jev=/absolute/application/stop-hold` declares an application program that may attach one hold note before native persistence. Repeat either check flag for multiple unique lowercase names. Hold checks run first, then quiet checks, preserving order within each flag. It accepts an absolute executable, without arguments or shell syntax. Consumers still run after persistence and receive the final decision.

A check runs only for a lead Claude Stop with a non-empty native `background_tasks` array, available reply and a verified current scope. A non-empty array remains eligible even if child reconciliation cannot read its task ids. Other events, child actors, missing/unreadable/empty arrays and unavailable reply launch no check. Nor does a Stop that keeps a sub-agent counted while it waits on a permission prompt (`child_waiting`). The reply is requested for this point independently of `--include-reply` for consumers.

Stdin is one JSON object, newline and EOF:

```json
{
  "schema": 1,
  "phase": "before_turn_end",
  "check_id": "11111111-1111-4111-8111-111111111111",
  "check_name": "jev",
  "scope": {
    "address": {
      "realm_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "incarnation_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "pane_id": "42"
    },
    "launch_id": "22222222-2222-4222-8222-222222222222",
    "target": {"kind": "binding", "binding_id": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}
  },
  "provider": "claude",
  "provider_session_id": "synthetic-session",
  "source_event": "Stop",
  "actor": {"kind": "lead"},
  "observed_mono_ns": "00000000000100000000",
  "reply": {"availability": "available", "text": "The build is running."},
  "background_tasks": [{"id": "job-1", "type": "shell", "status": "running", "description": "Run the build"}]
}
```

The array's JSON elements and fields are forwarded unchanged. There is no persistence claim: the scope was checked before execution and is checked again before writing. Input must fit the existing hook byte bound; oversized input supplies no hold. Replies and task arrays remain transient in Attention. Applications own any logging they perform.

An unheld program prints nothing (whitespace-only output is also accepted) and exits zero. A held program prints one JSON object and exits zero:

```json
{"hold": true, "answer": "waiting_on_own_work"}
```

Spaces between JSON tokens and a terminal newline are accepted. Only `hold=true` and an `answer` token matching `[a-z][a-z0-9_]*`, within the safe-label byte limit, are accepted. The token is application-owned. Unknown or duplicate keys, extra documents, false hold, wrong types, malformed/oversized output, incomplete input/output, nonzero exit or timeout supply no note. A line printed before a hang or failed exit does not count. Raw stdout/stderr are never recorded or included in diagnostics.

Programs share a 2000 ms monotonic budget covering launch, stdin, stdout EOF and direct-child exit. Each gets the remaining budget. Attention terminates its invocation-owned process group and reaps its direct child on failure/timeout. Descendants that escape that group are not contained. Later failures cannot cancel an earlier accepted note; later programs still run while budget remains. No retry or queue is provided.

The budget does not bound the entire hook. Preparation and final application each take launch and claim locks with up to 2 s per lock; ordinary filesystem/process work and cleanup have no whole-hook deadline. Each configured consumer adds its own deadline. With a 2000 ms check budget and one 1000 ms consumer, up to 3000 ms is reserved for executable work, before Attention's work. A provider hook timeout of 5 s can therefore expire before the complete hook finishes, including under lock contention.

Each lead native turn-end observation carries an Attention-written `turn_end`:

```json
{
  "status": "recorded",
  "held": true,
  "hold_checks": [{
    "name": "jev",
    "executable": "/absolute/application/stop-hold",
    "check_id": "11111111-1111-4111-8111-111111111111",
    "stage": "completed",
    "elapsed_ms": 240,
    "exit_code": 0,
    "note": {"hold": true, "answer": "waiting_on_own_work"}
  }]
}
```

`recorded` confirms the scoped lead activity/clear effects before the lifecycle write. `superseded` means newer native state displaced that turn end and has `held=false`; a program's unapplied note remains only in its audit entry. `unconfirmed` carries no `held` claim. Child reconciliation is independent. Binding/claim loss writes nothing for a new occupant and leaves outcomes in the hook report.

Stages are `evidence_changed`, `not_dispatched`, `not_started`, `completed`, `failed`, `stdin_failed`, `stdout_failed`, `timed_out`, `invalid_output` and `output_too_large`. Only completed can have a note. `reason` appears only for not_dispatched: `not_applicable`, `no_current_scope`, `reply_unavailable`, `input_too_large`, `total_deadline`, `child_waiting` or `history_unavailable`. Under `--strict`, any entry other than `completed`, `evidence_changed`, or `not_dispatched` with `not_applicable`, `no_current_scope`, `child_waiting` or `history_unavailable`, exits 1. Bypassed entries have zero elapsed time and no check_id. Attempted entries retain the input's check_id. Exit codes appear only when observed. Paths identify the invoked path, not executable bytes/version. Every configured entry appears in order; no configured checks gives an empty array. The hook prints these entries on stderr in its envelope with `--debug` or `--consumer`, or when an entry would make `--strict` exit 1.

Public inspection and the GUI lifecycle view retain this observation. `HookDelivery.turn_end`, when present, is copied from the exact stored observation named by its observation_id. It is absent for non-turn-end callbacks. Native correlated replay retains the stored observation ID and decision, and can repeat delivery.

A finish-sound reader accepts a lead turn-end observation with `turn_end.status=recorded`, `held=false`, and no validated quiet note, then deduplicates full address/launch/binding scope plus observation_id. A held observation is also marked seen. Do not use activity.event_id, which can repeat across distinct Stops, or the glyph, which review/other panes can mask. The application chooses whether failed/interrupted turn ends deserve its sound. Old unannotated observations establish a baseline rather than new playback. Retention floors indicate missing coverage; no exactly-once playback or durable sound queue is promised.

Held activity retains raw `type=stop` and accepted `hold_notes`. The GUI exposes `activity_type=stop`, effective `type=thinking`, and `turn_end_held=true`; its fixed `indicators.held` defaults to `⏾`, with thinking color and priority. Published tab text uses the same indicator. Automatic acknowledgement excludes held activity even under custom auto_clear. While a child of the pane's agent waits for permission, the GUI shows the lead's `stop`, held or not, as `type` `notify` with `turn_end_held` false, and focusing the pane acknowledges nothing; the held or finished presentation returns when the wait ends. A later applicable unheld Stop restores ordinary finished presentation, and any newer activity, such as the next prompt's, replaces a held one.

These optional fields require matching updated Rust/Lua readers and writers. Existing unannotated records remain readable by the new readers. Old strict readers reject annotated records; an old writer can move an annotated snapshot aside. Drain old hooks and update matching readers/writers before activation. Downgrading annotated state needs separate review; do not delete user state as an upgrade step.

`--quiet-check NAME=/absolute/executable` uses the same runner for lead Claude
and Codex `Stop` events. It does not require a reply or background task. Its
input adds `lifecycle`, the public lead lifecycle view frozen under the writer
locks, and `prospective_observation`, the native observation about to be
recorded. The prospective observation has its ID and observation time, but no
write time or `turn_end`: neither fact exists yet. The scope and provider
session identify the window. Missing or unreadable history, a native replay,
or an event at or after this Stop's observation time supplies no check input
(`history_unavailable`).

A quiet-check executable may return exactly
`{"quiet":true,"answer":"controller_submitted"}` or empty output. A hold-check
executable may return only a hold note; the two outputs cannot be combined in
one note. Quiet means the application has proved that this completion should
not call the human. It is not pane ownership, acceptance proof, or proof that
a controller has read the reply. The application supplies its own evidence.

Before applying a quiet note, Attention rechecks the same lifecycle snapshot
ID under the writer locks. A changed or unreadable snapshot discards the note
and records `stage=evidence_changed`. Before applying either note, it also
rechecks retained child permission waits. A waiting child or unreadable child
state discards the note. The audit retains execution timing and exit facts,
but no applicable note. A newer recorded lead event also fences an older
Stop even when its visible activity was deduplicated. Native correlated
replays retain the stored decision without replacing later activity or
clearing later children.

Quiet notes use the existing `hold_notes` map and `hold_checks` audit. They do
not set `held`; no additional persisted boolean mirrors the note. The GUI
exposes `turn_end_quiet=true` and uses `indicators.quiet` (default `↪`) with
`colors.quiet` (default `#122033`). Other panes' ordinary attention outranks a
quiet completion. Held presentation and waiting permissions take precedence.
Quiet completion can be acknowledged like an ordinary Stop. A later human
prompt and completion receive ordinary presentation unless independently
proved quiet. All checks share the existing 2000 ms budget.
### Scoped headless inspection

```sh
printf '%s\n' "$SCOPE_JSON" | attention inspect --scope - --json
```

Obtain `$SCOPE_JSON` from a selected public bindings row: `{ "address": row.address, "launch_id": row.launch_id, "binding_id": row.binding_id }`. All identity components are canonical. Unknown fields are rejected. `binding_id` can be omitted or null to leave that expectation unspecified; canonical serialization omits it. Address and launch remain required. A changed launch/binding is reported, never automatically followed. `complete` on a bindings answer describes that set, not the row: read `pane_presence`, `binding_health`, `reader_confidence` and `current` on the row, then inspect it. `current` is true for the binding that the pane's claim and its launch's current-binding record name, also after that binding has ended or its pane is gone, so a reader that wants live agents also requires `binding_phase` `active` and `pane_presence` `present`. An incomplete bindings listing does not make a present row unusable.

`result` is `PaneFacts`: requested `scope`, `scope_relation` (`matched`, `launch_changed`, `binding_changed`, `unavailable`), `binding`, `pane_presence`, `reader_confidence`, `binding_health`, `activity`, `binding_end`, `children`, `review`, `lifecycle` and facet-owned diagnostics. `binding` is a validated existing `BindingRow`, or null when no matched metadata is available. Null alone does not establish an unbound pane. `binding_health` follows the same rule as a `bindings` row: it rests on the binding, end, claim and current-binding records, and is `conflicted` when the same provider session is live at another pane address. A scope that is no longer current reads `valid` with `reader_confidence` `unconfirmed`. `reader_confidence` is `confirmed` exactly when the row is the pane's current binding and the pane is present; an unavailable activity or child presence set is reported in its own facet and does not lower it. Binding health is not a permission claim. Presence and provider binding phase remain independent: an ended provider can be in a present pane. Lifecycle validity does not by itself change the base identity/read-confidence axes; inspect its own availability and the envelope completeness.

Activity and binding-end facets contain `availability`, optional `record`, and `diagnostics`. Activity is present, absent, cleared, expired, unavailable, invalid or unsupported. Present/expired retain the validated activity record with native type/source/target, event ID, timestamps and supplied TTL/frame/label. Cleared retains the applicable clear record, whose stored fields need not include a write timestamp. Absent or failed reads omit `record`. Badge acknowledgement cannot hide this raw activity facet. Effective binding-end records retain reason, event/timestamps, the optional `binding_event_id` of the binding event they end, and optional Attention maintenance `operation_id`; an end of an earlier binding of the same id is absent. That operation ID is not a controller dispatch token.

The review collection contains availability, `count`, `evidence` and diagnostics. A successful empty collection can have availability present and count zero. Degraded reads may retain independently validated evidence.

The children facet contains `availability`, `count`, `waiting`, `coverage` and diagnostics. It is read from the binding's one `children.json`, which holds the sub-agents running now ([record contract](record-contract.md#child-presence)). `availability` says how that read went: present, absent, invalid, unsupported or unavailable. `count` is the number of running sub-agents, and `waiting` how many of them asked for permission and have done nothing since. `coverage` says what the count means. It is decided in this order, since which children count depends on the end: `ended`; `none`; a failed read of the end record (`invalid`, `unsupported` or `unavailable`, by how it failed); a failed read of the set (the same three); and otherwise `known`:

| `coverage` | When | `count` |
|---|---|---|
| `ended` | The binding's end record ends the binding | 0 |
| `none` | The provider records no sub-agents (Pi) | 0 |
| `invalid` | The binding's end record, or the set, fails validation, or the set names another provider than its binding | 0 |
| `unsupported` | The end record or the set declares a future schema | 0 |
| `unavailable` | The end record or the set could not be read | 0 |
| `known` | The set was read and names its binding's provider, or there is none | The sub-agents the set holds; 0 when there is no set, or when the binding has an end the set has not applied |

Only `ended`, `none` and `known` give a count to rely on. Under the other three a zero means unknown, not none; the plugin draws those as `+?`. When the binding record itself, or the pane's current-binding pointer, cannot be read, `inspect` reports every facet `unavailable`. When the file cannot be opened, the plugin reuses a binding record it read before and goes on counting; otherwise it shows no count and reports `binding_health` `invalid`, or `future_schema` when the record declares a newer schema. `inspect` has no earlier read to fall back on, where the plugin keeps counting its last successful read of the same file. A scope with no selected binding reads `known` with availability absent. `known` is what the command recorded, not what runs: a sub-agent whose end was never reported stays counted, and a Codex sub-agent still working after its parent's stop is not counted until its next event ([accepted limitations](accepted-limitations.md#the-sub-agent-count-depends-on-how-claude-code-and-codex-send-hooks)).

Lifecycle uses the existing GUI observation/request/relation/floor names. Headless availability is available, absent, unavailable, invalid or unsupported; there is no cached fallback. Missing snapshot IDs, correlations, optional native fields and floors are omitted. Arrays remain arrays when empty. Each file's two pool floors are preserved when present. Optional badge acknowledgement is separate from raw activity and native relations.

The reader checks claim/current pointer before and after assembly and rechecks socket identity. Rotation discards the assembled facts and answers `scope_relation` `unavailable`, unless the scope's server is shown to have exited, which reads the pane `verified_absent`. A stable binding does not make independently written files one atomic snapshot. Inspection does not create directories, write acknowledgements, prune records or start WezTerm. `result.timing_ms` has the same `pane_list`, `process_list` and `records` fields as `bindings`. Complete valid reads, including explicit absence, exit 0; changed/degraded evidence exits 1; invalid scope/arguments exit 2. Complete means this requested response was represented, not complete history or task success.

### GUI view callback

```lua
attention.apply_to_config(config, {
  settled_title_fallback = false,
  on_view_change = function(change)
    -- Update application-owned presentation. Return promptly.
  end,
})
```

Initial/updated messages contain `kind`, GUI-local `window_id`, full `scope` and detached `view`. The view has every field `get_attention_view` returns except `frame`. Scope has address, launch and a launch or binding target. Scope-lost messages contain only `kind`, `window_id` and `previous_scope`. Window context is not source identity or control permission. An unpublished pane, or one no launch has claimed, never gets a fabricated v2 scope.

Each window has its own baseline, and messages are not ordered across windows: a pane moved from one window to another is `scope_lost` in one and `initial` in the other, in whichever order the two windows poll. A window counts as closed only when it is gone from both `wezterm.gui.gui_windows()` and `wezterm.mux.all_windows()`, so switching workspaces does not report the hidden panes as lost. A confirmed source replacement emits loss before initial; ending the same binding is an update. Fresh target selection can establish a degraded new binding view. Unavailable target selection retains the last established scope only as degraded context; it does not restore old facts. Lifecycle-only, confidence and floor changes count; spinner animation alone does not. Delivery follows cache refresh and configured acknowledgement, including in unfocused windows.

Registration is once per module. A repeated `apply_to_config` is named once in the WezTerm log and does not replace the callback. A successful normal configuration reload starts a fresh module baseline; a window override event alone does not. Callbacks are cooperative: exceptions and reentrant delivery are contained, but an infinite callback cannot be preempted. An error is logged as `on_view_change failed: <error>; future polls remain enabled`, once per distinct error and for at most 16 distinct errors. Schedule expensive work outside the poll. With title fallback disabled, polls do no process-title sampling, title-state comparison or title-only redraw/advice. Default rendering and review/acknowledgement remain supported.

### Executable application recipes

- `examples/reply-sink.mjs` stores exact supplied content and its full source scope in an application-owned file. It does not prove the last received delivery is the newest/current reply; a resolver must check identity and own its ordering/idempotency policy.
- `examples/follow-up.lua` consumes normalized publication facts and per-window callbacks. Local dismissal updates presentation immediately and does not answer a provider question. It is optional: `examples/wezterm.lua` loads it only when the file exists beside it.
- `node examples/checkpoint.mjs /absolute/attention /absolute/socket /absolute/wezterm /absolute/checkpoint.json [LIMIT]` brackets topology with two complete socket binding queries. Failure, incomplete evidence, duplicate current associations or socket rotation preserves the old checkpoint. Missing association stays null/unknown. Topology uses explicit `--no-auto-start`; no identity hashing or private record paths are copied. The two query durations are reported in milliseconds.
- `node examples/inspect.mjs /absolute/attention /absolute/socket [LIMIT]` performs bounded discovery followed by exact-scope inspections. Incomplete discovery stops before inspection; degraded or changed scope fails rather than following another occupant.

The Node process recipes use 10-second subprocess deadlines and an 8 MiB output budget. These are example I/O budgets, not agent-state thresholds. They retain failure instead of inferring absence. Checkpoint replacement uses a private temporary file plus rename; no cross-file atomicity or disk-durability guarantee is added. Tests supply synthetic topology/content and production CLI responses. None of these examples installs hooks, chooses a live profile, accesses the clipboard or grants controller permission.