# ADR 0002: A sub-agent stays counted until a hook shows it ended

- **Status:** Accepted
- **Date:** 2026-09-27

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
at its `SubagentStop`, at a Codex parent `Stop` ordered after its last event, or
at the binding's end. Nothing removes a sub-agent for being quiet. The set keeps
no stopped sub-agents, has no entry cap, and no size limit beyond the general
`max_json_bytes`, which the writer checks before it writes. Readers count its
live entries and apply one rule of their own, the binding's end: nothing counts
while the end ends the binding or the set has not applied it. A count that
cannot be read, because the set or the end record cannot, is drawn as unknown
(`+?`, or `+N?` beside other panes' counts), never as zero.

Rejected: per-sub-agent records with a presence TTL, a retention floor and
compaction, the design this replaces. The TTL hides sub-agents that are still
running, and any bound on the kept history is a number standing in for "this
sub-agent ended", which the hooks report directly.

## Consequences and revisit

A sub-agent whose end is never reported (a failed or unregistered
`SubagentStop`, an interrupted Codex sub-agent) stays counted until its session
ends; no command removes it by hand. The count rests on hooks staying
synchronous and on Codex parents stopping only after their sub-agents: none of
11 Codex CLI 0.157.1 sessions with sub-agents showed sub-agent work after the
parent's `Stop`. A sub-agent that works or stops after the latest parent `Stop`
removed it is reported as `child_active_after_parent_clear`; one that stays quiet
past its parent's next `Stop` is not, since the set keeps only the latest
parent stop's removals. Old per-sub-agent files are no longer read;
their kinds stay declared so that sweep still recognises and removes them. The
`children` facet of `attention inspect` changed shape.

Revisit if a provider delivers hooks asynchronously by default, if
`child_active_after_parent_clear` shows up in normal Codex use, or if a
provider starts reporting its running sub-agents directly, which could replace
the parent-stop rule.

Enforced by `a_quiet_running_child_stays_counted`,
`a_codex_parent_stop_ends_the_children_it_covers`,
`children_of_an_ended_lifetime_are_never_counted`,
`a_set_written_before_the_end_counts_nothing_after_a_resume`,
`a_child_back_after_a_codex_parent_stop_is_reported_whichever_event_brings_it` and
`readme_hook_blocks_register_exactly_the_described_rows` in the Rust
integration suites, and the shared children coverage cases in
`tests/fixtures/v2/protocol-cases.json`, which the Rust reader, the Lua reader
and `tests/fixtures/v2/check.py` all run.
