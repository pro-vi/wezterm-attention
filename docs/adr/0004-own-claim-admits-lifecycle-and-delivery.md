# ADR 0004: An agent's own pane claim admits lifecycle facts and consumer delivery

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

On macOS an agent that no shell claimed for claims its pane at its first
session start, and each later event proves itself against that claim: the
hook's direct parent is the claim's owner (the same pid, start time and boot
session), on the terminal and socket incarnation the claim recorded. Such
events wrote the pane's binding, activity, children, end, review and clear
records, but no lifecycle observations, and they reached no `--consumer`
executable; only an event with an inherited shell launch id did. The rule came
from the probe in [lifecycle contact results](../reviews/lifecycle-contact-results.md#execution-identity-is-a-separate-verdict):
an `exec` keeps pid, parent, executable and start time, so those fields cannot
tell one run of a program from the next. A consumer of lifecycle facts or
deliveries saw nothing for an agent started without a claim prefix.

## Decision

An event resolved through its agent's own claim keeps lifecycle observations
and is admitted for delivery on the terms an inherited event is. Under the
launch and claim locks, `ResolvedLaunch::lapsed` must find the claim unchanged
and the host proof confirmed again. The lifecycle write needs the event's
binding to be the launch's current binding, and delivery also needs that
binding's provider and session id to equal the event's. `attention mark` still
needs an inherited launch id, a pane holding a shell claim still refuses events
without its id, and Linux has no self-claim.

Rejected:

- Keeping the rule, so every launch carries a shell-claim prefix: a pane with a
  leftover shell claim then refuses any agent started without its id for the
  pane's lifetime.
- Saying in the delivery or the lifecycle facet how the launch was claimed: no
  reader branches on it.
- Versioning the delivery contract: delivery first appears in 1.0.0, which was
  unreleased when this was decided.

## Consequences and revisit

The probe's gap is not closed, and the inherited path never closed it: an
inherited launch id is an environment variable and survives `exec` as the
process fields do. In both paths the provider session and the launch's current
binding decide which session a fact belongs to. A consumer run for a
self-claimed event inherits no launch id, so `attention mark` from inside it is
refused. Delivery admission confirms the agent process once more per event,
after the write's own fence did: a fixed handful of system calls.

Revisit if a program that execs into another agent is found to reuse the first
agent's provider session id, if a consumer needs to tell a shell claim's
delivery from an agent's own, or if the host proof drops one of its checks.

Enforced by `an_agent_s_own_claim_keeps_lifecycle_facts_and_delivers_its_events`
and `a_session_its_agent_switched_away_from_keeps_no_lifecycle_fact_and_reaches_no_consumer`
in the self-claim suite, and by
`an_agent_gone_after_its_event_resolved_keeps_no_lifecycle_fact_and_reaches_no_consumer`
and `an_agent_s_claim_taken_over_after_resolution_refuses_its_event` in the
claim-fence suite.
