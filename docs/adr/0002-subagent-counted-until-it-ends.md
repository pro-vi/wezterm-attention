# ADR 0002: A sub-agent stays counted until a hook shows it ended

- **Status:** Accepted
- **Date:** 2026-09-27
- **Amended:** 2026-09-29 — a Claude lead `Stop` also ends the sub-agents it no longer lists

## Context

Builds before this one wrote one record per sub-agent under `agents/`, and
readers hid a sub-agent ten minutes after its last event. A sub-agent that runs
one long command sends no hook while it runs, so it left its tab while it was
still working. Records of stopped sub-agents were kept as ordering fences
against late events, so the plugin's poll read more files the more sub-agents a
session had run; a retention floor and compaction were added to bound that.

Claude Code 2.1.283 and Codex at source commit `985cf47a4` run a command hook to
completion before the agent goes on, unless the hook is registered with
`async: true`. One sub-agent's events then reach the writer in the order they
happened, and no older event of a sub-agent arrives after its stop. A stopped
sub-agent needs no fence.

## Decision

One `children.json` (kind `child_presence_set`) per binding holds exactly the
sub-agents running now. The writer adds a sub-agent at its `SubagentStart`, or
at a tool call or permission request that names its `agent_type`, and removes it
at its `SubagentStop`, at a Codex parent `Stop` ordered after its last event, at
a Claude lead `Stop` whose `background_tasks` no longer lists it, or at the
binding's end. A lead `Stop` spares a sub-agent with an event stamped after the
`Stop` began, and a `Stop` with no readable list ends nothing. Nothing removes a
sub-agent for being quiet. The set keeps
no stopped sub-agents, has no entry cap, and no size limit beyond the general
`max_json_bytes`, which the writer checks before it writes. Readers count its
live entries and apply one counting rule of their own, the binding's end: nothing counts
while the end ends the binding or the set has not applied it. A count that
cannot be read, because the set or the end record cannot, is drawn as unknown
(`+?`, or `+N?` beside other panes' counts), never as zero.

Rejected: per-sub-agent records with a presence TTL, a retention floor and
compaction, the design this replaces. The TTL hides sub-agents that are still
running, and any bound on the kept history is a number standing in for "this
sub-agent ended", which the hooks report directly.

Also rejected, for the Claude rule: reusing the Codex parent clear, which is
skipped when a `Stop` repeats the published one and so drops the reconcile for
exactly that `Stop`; ending a sub-agent at its own `StopFailure`, which acts at
the moment of death but covers API errors only; and a command that clears a
binding's sub-agents by hand, until something shows Claude Code lists a dead one.

## Consequences and revisit

A Claude sub-agent whose end is never reported goes at the next lead `Stop`
that does not list it. One that Claude Code still lists, or that runs under a
Claude Code sending no list, and an interrupted Codex sub-agent, stay counted
until the session ends; no command removes them by hand. Nothing is recorded of
the sub-agents a lead `Stop` ends, so a list whose ids stop matching would end
every sub-agent at each `Stop` with only the opt-in contact check to say so.
The count rests on hooks staying synchronous and on Codex parents stopping only
after their sub-agents: none of 11 Codex CLI 0.157.1 sessions with sub-agents
showed sub-agent work after the parent's `Stop`. A sub-agent that works or stops after the latest parent `Stop`
removed it is reported as `child_active_after_parent_clear`; one that stays quiet
past its parent's next `Stop` is not, since the set keeps only the latest
parent stop's removals. Old per-sub-agent files are no longer read;
their kinds stay declared so that sweep still recognises and removes them. The
`children` facet of `attention inspect` changed shape.

Revisit if a provider delivers hooks asynchronously by default, if
`child_active_after_parent_clear` shows up in normal Codex use, or if Codex
starts reporting its running sub-agents directly as Claude Code now does, which
could replace its parent-stop rule. For the Claude rule, revisit if a
sub-agent the API ended mid-run stays listed (only one that failed at its start
has been seen to leave the list), which calls for the clear command; if the
contact check fails; or if a wrong removal has to be found by something other
than that check, which calls for recording what a `Stop` ends.

Enforced by `a_quiet_running_child_stays_counted`,
`a_codex_parent_stop_ends_the_children_it_covers`,
`children_of_an_ended_lifetime_are_never_counted`,
`a_set_written_before_the_end_counts_nothing_after_a_resume`,
`a_child_back_after_a_codex_parent_stop_is_reported_whichever_event_brings_it`,
`a_claude_stop_ends_a_child_the_provider_no_longer_lists`,
`a_claude_stop_keeps_every_child_the_provider_lists`,
`a_claude_stop_without_a_usable_list_ends_nothing`,
`a_claude_stop_keeps_a_child_that_worked_after_it`,
`a_claude_stop_applies_beside_a_child_set_it_cannot_change` and
`readme_hook_blocks_register_exactly_the_described_rows` in the Rust
integration suites, and the shared children coverage cases in
`tests/fixtures/v2/protocol-cases.json`, which the Rust reader, the Lua reader
and `tests/fixtures/v2/check.py` all run.
