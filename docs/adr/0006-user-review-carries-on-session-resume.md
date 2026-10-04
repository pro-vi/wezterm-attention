# ADR 0006: Carry the user's review during session resume

- **Status:** Accepted
- **Date:** 2026-10-04

## Context

A review belongs to a pane, while an agent session can resume in another pane
after its terminal server is replaced. The user flag otherwise stays in the
old pane's records. The session index names every retained binding; it is not
a single current-session pointer.

## Decision

Allow binding registration to carry the `user` review on an incoming `resume`
to a different pane address. The plugin's review key and this checked carry
are the two writers of that slot. Public producer commands still cannot use
the `user` owner.

Select the newest validated retained binding for that provider/session by wall time,
before checking its review or server. An ambiguous newest binding, a newer
unflagged binding, or a binding whose old claim/current-binding pointer no
longer selects it prevents fallback to older flags.

Carry when the old socket is gone, a different socket object serves its path,
or the old GUI process is proven gone. A missing pane on a current server and
a failed listing do not qualify. A ctime-only change on the same socket object
does not qualify either: changing socket permissions can cause it without
ending any panes. Preserve records when the evidence cannot decide.

Serialize registration by provider/session. Order multi-pane lock groups by
full address, retaining launch, claim and owner-review order within a group.
Recheck both claims, source selection and review records under those locks.
The source claim lock excludes concurrent sweep pruning; review locks order
the user's set/clear operations against carry.

After the destination binding is written, durably remove the source user flag,
then write the destination flag. Preserve a user flag already at the
destination. Confirmed registration never retries the transfer, so a later
user clear is not undone. Other owners' reviews and activity are not carried.

## Consequences

The transfer is at most once, not an atomic two-file move. A process stop or
write failure after source consumption can lose the carried flag. This is an
accepted limit; no durable transfer records or automatic retry are added.
Writing the destination before consuming the source was rejected because an
interruption could leave the source available to another resume.

No retained binding/review means no carry. Sweeping old records can therefore
remove the evidence needed for restoration. Plain claimed commands retain
pane-only flags. Providers that defer resume registration restore their flag
only when that registration arrives.

## Revisit

Revisit if restoration must survive an interrupted transfer. That would need
durable transfer state plus a rule that preserves a destination user clear.

## Enforcement

- `binding_mutation`, `ResumeReview`, `consume_review_before_replace`.
- `session_registration_lock`, `session_binding_files`.
- `tests/rust/lifecycle_spec/review_resume.rs` and
  `review_carry_failure_tests::consumed_review_is_not_retried_after_destination_write_failure`.
