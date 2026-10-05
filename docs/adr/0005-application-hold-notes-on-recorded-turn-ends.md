# ADR 0005: Application hold notes change finish presentation, not native turn ends

- **Status:** Accepted
- **Date:** 2026-10-04
- **Amended:** 2026-10-04 — a child waiting on a permission prompt overrides the held presentation

## Context

A lead Claude `Stop` can carry a non-empty `background_tasks` array. Attention records its `response_finished` observation and `stop` activity. An application can determine from the final message that the agent is waiting on its own work and request that this Stop not receive finish presentation.

The existing `HookDelivery` is built after requested persistence is confirmed. It cannot supply the decision before activity persistence. Attention must retain control of verified identity, native event meaning, ordering and writes while accepting an application-owned interpretation of the message.

Source contact was against `1233695f0a6732e60ba09f8827b97f6c944b57d8`. The implementation is verified with disposable subprocess, record and production Lua tests; live activation is separate.

## Decision

Add repeatable named `--hold-check NAME=/absolute/executable` registrations to `attention hooks event`. Only a lead Claude Stop with a non-empty native task array and available reply runs them. Array presence is independent of the stricter task-id reduction used for child reconciliation. The program's classifier uses the reply only; array presence is only an eligibility gate. Forward the native background_tasks array unchanged to the program's stdin for application-owned logging/evaluation; Attention does not persist it. No transcript is read.

Give each program a separate `HoldCheckInput`: checked scope, native lead Stop facts, original observation order, available reply and unchanged native task array, marked `phase=before_turn_end`. It carries no persistence claim. Run outside writer locks, then revalidate the original claim, host, current provider binding, ordering and clear fences before applying the original event. Never follow a replacement occupant.

Accept empty stdout as no note. Accept one strict `{"hold": true, "answer": "waiting_on_own_work"}` object with an optional terminal newline as a hold; JSON whitespace between tokens is accepted; the answer is a bounded application token, not an Attention classifier enum. Attention attaches at most one note under the configured program name. Unknown fields, duplicate keys, multiple objects, incomplete I/O, nonzero exit and timeout attach no note. Raw program output and message text enter no Attention record or diagnostic.

Run programs in declaration order, with a shared 2000 ms monotonic transport deadline covering launch, stdin, stdout EOF and direct-child exit. Each receives the remaining budget. The agreed Jev request budget is 1500 ms and stays separate; other applications own their internal deadlines; the outer allowance includes process overhead. A later failure cannot cancel an earlier successful hold. Record every configured program's name, invoked path, stage, elapsed time, observed exit code and validated note, including programs not dispatched. No retry queue is added.

Retain native `type=stop` and `kind=response_finished`. Accepted notes become activity `hold_notes`; the corresponding lifecycle observation carries an Attention-written `turn_end` with applicability (`recorded`, `superseded` or `unconfirmed`; unconfirmed has no held claim), held state and execution audit. Confirm native effects before publishing recorded lifecycle metadata. Preserve child reconciliation and exclude the Attention-owned annotation from native replay equality. Native observations displaced by newer activity or clear carry no sound-eligible recorded decision.

Derive held GUI presentation from validated activity notes: raw `activity_type=stop`, effective thinking priority/color, fixed `indicators.held` (default `◑`), and `turn_end_held=true`. Use the same held indicator in published tab text. Automatic acknowledgement refuses held activity even under custom auto-clear settings. Review and multi-pane priority continue to apply. A later applicable unheld Stop restores ordinary `✓`; an explicit scoped clear of the current activity remains available, including for an agent-owned claim. There is no automatic hold-release timer.

A hold assumes the lead's own work will wake it. A child with `status=waiting` in the binding's `children.json` is blocked on a permission prompt and cannot finish without the user, yet Claude Code lists it as `running` in `background_tasks`, so the hold check cannot see it. The held Stop also replaces the `notify` the child published. While the child set holds a waiting child, the reader therefore ignores hold notes: `turn_end_held` is false and the effective type is `notify`, with its indicator, color and priority. Focusing the pane acknowledges nothing, because the shown type is not the stored one and the writer refuses to acknowledge activity with hold notes. When the wait ends, the notes apply again.

Expose the exact stored `turn_end` through lifecycle inspection/view and post-persistence delivery. Annotate existing lead turn ends across providers, while running a check only for the eligible Claude Stop. A sound reader distinguishes recorded unheld from recorded held and superseded, and deduplicates by full source scope and stored observation ID. Badge IDs and glyphs do not identify turn-end receipts. Application sound policy and playback remain outside Attention.

Reserve bounded execution metadata before running programs. Preserve the native 2048-byte allowance, add a 16384-byte annotation allowance, and keep snapshot/pool bounds. The specified nested note grammar requires depth 9. Update Rust, Lua and Python validators together; never truncate native fields or drop native evidence to fit optional audit.

## Rationale

A pre-write application check keeps message judgment with the application while Attention owns native facts and final state. Post-write consumers are too late. Keeping the preceding activity can keep an earlier checkmark. An internal question-matching heuristic duplicates application policy and gives Attention responsibility for message interpretation.

Lifecycle metadata places the hold and applicability decision on the recorded native observation itself. It avoids treating a standing badge as a per-turn log, joining separate records by timestamps, or sounding a delayed Stop that a newer prompt superseded.

## Consequences and revisit

Check failure leaves ordinary Stop behavior available; it cannot override a newer native fence. A waiting child does not change what the CLI records: `hold_notes` and `turn_end.held` stay as the check returned them, so a sound reader stays silent for that Stop; only the GUI presentation yields. A mistaken hold can remain if no later event arrives, so this decision does not promise that every held agent wakes or that a needed call cannot be lost. Polling can miss bounded retained observations. Native persistence can succeed while lifecycle persistence fails; no sound receipt is invented in that case.

The current strict readers reject these annotations. New readers accept existing unannotated state, but annotated state requires matching writers/readers and drained old hook invocations before activation. Amend the current schema in place; add no parallel implementation or state deletion. Downgrading annotated state to old readers requires separate review. Installation and live hook/sound registration are a later action.

Revisit if transport overhead regularly exhausts the outer allowance, native task/wake behavior changes, scheduled wakes receive evidence and become in scope, a sound reader requires durable delivery beyond retained lifecycle history, or mixed-version activation cannot preserve existing state.

## References

- `src/main.rs`: `run_hooks_event`.
- `src/providers.rs`: `parse_provider_event`, `listed_task_ids`, `reply_content`, `parse_run_observation`.
- `src/lifecycle.rs`: `resolve_launch`, `apply_activity`, `plan_activity`, `append_observation`, `apply_observed_outputs`.
- `src/consumer.rs`: `HookDelivery`, `delivery_bytes`, `dispatch`.
- `src/observations.rs`: `LifecycleObservation`, `storage_key_parts`, `semantic`.
- [Consumer guide](../consumer-guide.md): turn-end kinds and transient delivery.
- [Record contract](../record-contract.md): ordering, child presence and trust boundary.
- [ADR 0002](0002-subagent-counted-until-it-ends.md): child reconciliation remains independent.
- [ADR 0004](0004-own-claim-admits-lifecycle-and-delivery.md): existing claim and delivery admission remain required.
