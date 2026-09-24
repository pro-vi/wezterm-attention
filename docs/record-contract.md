# Record contract

This document describes the v2 records the `attention` command writes, and how they relate to the older v1 flat markers. "v1" and "v2" name those two formats, not releases of this project. `protocol/v2.json` is the machine-readable authority. Writers must call `bin/attention`; examples
and provider hooks must not construct v2 record JSON themselves.

This is an implementation contract, not an activation claim. `bin/attention` selects the Rust writer, but a hook registered before it was installed keeps doing what it did: a helper that writes flat files keeps writing them, and a zsh configuration that never calls `wezterm_attention_claim` never establishes a launch claim.

## Identity and paths

A v2 pane address contains a realm digest, socket-incarnation digest, and canonical server pane ID.
GUI-local pane IDs never name v2 files. Launch IDs, provider bindings, child identities, and review
owners remain separate.

The state root is `WEZTERM_ATTENTION_DIR` when it is set and non-empty, else
`$XDG_STATE_HOME/wezterm-attention` when `XDG_STATE_HOME` is set, non-empty and absolute, else
`$HOME/.local/state/wezterm-attention`. The Rust writer, the plugin and the Pi extension use this
one order. A relative `WEZTERM_ATTENTION_DIR` is an error to the writer; the plugin and Pi ignore it
with a warning and fall through to the next rule.

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
```

State that is not addressed by a pane lives outside that tree and outside this manifest. The tab bar publishes the order it draws at `tabs/<incarnation id>-<window id>.json`, one file per identified GUI source and window; it names no pane address, carries no pane execution fence and no TTL, so it carries its own `schema` (currently 2) and is versioned separately from `record_schema`. That is the rule for any published fact with no address to validate against: a local schema field, not a manifest entry, because a record-tree change must not refuse a file that has nothing to do with it. `attention tabs` reads them. The process that wrote a file withdraws it when its window closes, and only its own files; a file whose writer has exited is collected by `attention sweep` when every pane it names is verified absent, or when it names no tab at all. See the [consumer guide](consumer-guide.md) for what the order does and does not promise.

The binding record is durable before its pointer. Rust provider/CLI transitions use the appropriate lock scopes and atomic per-file replacement. Lua acknowledgement and review operations validate their targets but use per-file atomic replacement or removal without those Rust locks; a read/check/write sequence is not a cross-writer transaction. Raw child and review IDs never become filenames.


Schema 2 carries `source` with canonical `socket_path`, `realm_id` and `incarnation_id`, derived from the publishing GUI socket. The filename must match its incarnation and window ID. Schema-1 files at `tabs/<window id>.json` remain readable with no known source. Equal window numbers do not associate legacy files with new sources. Cleanup uses the validated file path rather than reconstructing one from a window number.


Window checks are derived per query and never stored in publication files. Their statuses are `present`, `not_listed` and `unavailable`; source identity failure cannot produce `not_listed`. The recorded source namespace is separate from a pane realm, so realm-filtered sweep still leaves tab publications alone.

## Ordering and wall age

`observed_mono_ns` orders competing writes and supplies activity-clear, child-clear,
retention-floor, and absence fences. `written_at_unix_ns` is required on activity, child presence,
binding, and binding-end records. It supplies TTL and 30-day retention age.

Exact TTL equality remains eligible. The first ineligible instant is one nanosecond later. Missing,
malformed, unavailable, or negative wall age fails closed: TTL-bearing state is omitted, retention
does not prune it, and diagnostics report `record_invalid`, `probe_unavailable`, or `clock_skew`.

A lead `UserPromptSubmit` starts the turn's `thinking` activity for Claude and Codex, the way
Pi's `agent_start` does, so the pane is tinted from the prompt rather than from the turn's first
tool call and a turn that calls no tool still shows activity. The first `PreToolUse` of that turn
repeats the same `thinking` and is skipped. A child actor cannot write lead state, so its prompt
stays observation-only. The one exception is a child's `PermissionRequest`: a child blocked on a
permission prompt waits for the user as the lead would, so it publishes lead `notify` activity. It
does not refresh the child's presence record, and its observation still names the child as the
actor.

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

Prompt return is `hooks publish` from a bound pane. It republishes the pane identity and writes an activity-clear watermark for the current lead activity only. It never clears child presence and never writes `end.json`.

## Compatibility and precedence

Attention's writers do not maintain v1 flat marker or `.agents` projections.
The flat format remains permanently supported input: third-party writers and
Pi's fallback may still create `<root>/<pane_id>` and `<pane_id>.agents`, and
the Lua reader keeps accepting them. Writer-owned leftovers from development builds that
projected v2 records into those names are collected with `attention sweep --json` to preview,
then `attention sweep --apply --operation-id "$(uuidgen | tr A-Z a-z)"`.
The operation id must be a canonical lowercase UUID, new for every run. A run
that reuses an earlier run's id is treated as a replay of that run: it ends no
binding and advances no retention floor, and no diagnostic says so, because the
absence rule needs two observations under different ids. Collection follows a unique
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
removes one on a read. An acknowledgement inside a binding directory is removed only with that
directory, when `attention sweep --apply` retention prunes the binding.

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
session at different pane addresses. A binding that has ended, or whose pane is verified absent, is
history and is left out of that comparison: resuming a session in a new pane leaves one behind every
time, and marking the live row conflicted would hide the pane the session now runs in.

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
are refused. One gap remains: a printed identity with the right pane id but another mux's realm is
still believed. On a mux-attached pane the published value is the only identity there is.

A current binding is selected by the pane's current claim and then that launch's pointer. A pointer
inside a historical launch cannot make its binding current or confirmed. Doctor validates v2
records in its file and version scope even when a pane has no binding.

Destructive absence needs two pane-list negatives under different operation IDs at least 60
monotonic seconds apart, plus an identity-scoped process negative for the full socket path and pane
ID. Process-probe failure is unavailable evidence, not absence. One failed process listing answers every pane of that query as unavailable; it is not retried pane by pane, so a query waits on at most one pane listing per mux socket and one process listing. A realm-wide `bindings` lists every socket it knows, so each unresponsive socket adds its own listing deadline and there is no overall one. Process environments are never
printed or persisted.

A retention floor advances only across complete monotonic-timestamp groups that were already
ineligible under the prior floor. An eligible member blocks the whole equal-timestamp group.
Binding-history caps are calculated separately for each realm.
