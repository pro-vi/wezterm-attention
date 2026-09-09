# Record contract

`protocol/v2.json` is the machine-readable authority. Writers must call `bin/attention`; examples
and provider hooks must not construct v2 JSON themselves.

This is an implementation contract, not an activation claim. `bin/attention` currently selects Rust, while the bootstrap Claude/Codex helpers inspected on 2026-09-08 still write the V1 compatibility files directly and the inspected zsh configuration does not establish a V2 launch claim.

## Identity and paths

A v2 pane address contains a realm digest, socket-incarnation digest, and canonical server pane ID.
GUI-local pane IDs never name v2 files. Launch IDs, provider bindings, child identities, and review
owners remain separate.

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
          agents-clear.json
          agents-floor.json
          agents/<agent-key>.json
```

The binding record is durable before its pointer. Every accepted transition is lock-guarded and
atomically replaced. Raw child and review IDs never become filenames.

## Ordering and wall age

`observed_mono_ns` orders competing writes and supplies activity-clear, child-clear,
retention-floor, and absence fences. `written_at_unix_ns` is required on activity, child presence,
binding, and binding-end records. It supplies TTL and 30-day retention age.

Exact TTL equality remains eligible. The first ineligible instant is one nanosecond later. Missing,
malformed, unavailable, or negative wall age fails closed: TTL-bearing state is omitted, retention
does not prune it, and diagnostics report `record_invalid`, `probe_unavailable`, or `clock_skew`.

An activity-clear watermark hides activity at or below its monotonic observation. A strictly newer
activity reappears. Child presence behaves the same way across active, stopped, parent-clear, and
retention-floor records. A stopped snapshot is retained because deleting it would discard the
ordering fence.

Prompt return is `hooks publish` from a bound pane. It republishes the pane identity and writes an activity-clear watermark for the current lead activity only. It never clears child presence and never writes `end.json`.

## Compatibility and precedence

Writers maintain flat v1 activity and `.agents` projections during the compatibility period.
Duplicate v2 events repair missing or byte-different projections without refreshing event or wall
timestamps.

The flat activity projection writes `updated_at` in seconds and `updated_at_ms` in milliseconds. The `.agents` projection writes `last_ms` in milliseconds; its `type` is the provider `agent_type` when that contract is present, otherwise the event source.

A valid v2 claim selects v2. Invalid or future v2 is reported and never downgraded to plausible v1.
V1 is read only when no v2 claim exists. The public Lua query remains six values:
`type, frame, source, puppet, subagents, review`.

For a v2 pane, focusing the active pane writes an exact acknowledgement for the displayed activity
event. `Alt+B` writes the `user` owner claim under that pane's full address. Its clear-all action
removes every valid review claim in the active tab through each pane's full address and leaves
activity records unchanged. V1 panes keep their shipped `.ack` and `.review` behavior.

Acknowledgement records are Lua-owned. Rust validates and prunes them during reads and maintenance, but Rust never creates an acknowledgement.

## Consumer boundary

`attention bindings --json` returns validated facts and four independent axes: binding phase, pane
presence, reader confidence, and binding health. It never returns a resume command. Consumers build
their own argv from the closed provider and session ID fields.

JSON responses contain `schema`, `command`, `status`, `complete`, `result`, and `diagnostics`.
Default output is bounded. Use `--all` or `--all-details` only when complete detail is required.

## Trust boundary

Trust means schema-valid, internally addressed, correctly fenced cooperative state. It is not
authentication against another process running as the same user. State directories and files are
private, but a same-UID process can still forge cooperative records.

A current binding is selected by the pane's current claim and then that launch's pointer. A pointer
inside a historical launch cannot make its binding current or confirmed. Doctor validates v2 JSON
records in its file and version scope even when a pane has no binding.

Destructive absence needs two pane-list negatives under different operation IDs at least 60
monotonic seconds apart, plus an identity-scoped process negative for the full socket path and pane
ID. Process-probe failure is unavailable evidence, not absence. Process environments are never
printed or persisted.

A retention floor advances only across complete monotonic-timestamp groups that were already
ineligible under the prior floor. An eligible member blocks the whole equal-timestamp group.
Binding-history caps are calculated separately for each realm.
