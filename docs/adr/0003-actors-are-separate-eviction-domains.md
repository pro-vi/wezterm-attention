# ADR 0003: The lead and its sub-agents keep lifecycle evidence in separate files

- **Status:** Proposed
- **Date:** 2026-09-28

## Context

A binding's lifecycle observations lived in one `lifecycle.json`, whose
`general` and `requests` pools each keep the 64 newest observations. The pools
split observations by kind, not by who made them, so a busy sub-agent's tool
calls evicted the lead's newest evidence. One pane was measured holding 64 of 64
`general` entries from sub-agents and none from the lead, and the newest lead
observation left in the file was about 58 minutes older than the lead's last
hook. A consumer that dates the lead's last request from these observations,
such as a prompt-cache countdown, read that stale time.

## Decision

The writer keeps the lead's observations in `lifecycle.json` and its
sub-agents' in `children-lifecycle.json` beside it (kind
`child_lifecycle_snapshot`), which has the same shape, pools and limits; a lead
observation in the children's file makes it invalid. A hook writes one of the
two, and a lead observation never reads the children's file. Sub-agent
observations already in `lifecycle.json` are not moved; readers settle them
against the children's file. Both readers assemble one lifecycle facet from the
two files. Observations from the children's file have the pools
`child_general` and `child_requests`, each file's floors appear as
`lead_<pool>` and `child_<pool>`, and `general` and `requests` stay the later
floor of either file, so a consumer reading them keeps the meaning "some
evidence of that pool was evicted". Neither the record nor the manifest schema
changes.

Rejected:

- Four pools inside `lifecycle.json`: it changes the `lifecycle_snapshot` shape,
  so every older reader rejects the file.
- One file per sub-agent: a poll would read as many files as a session has
  sub-agents, which [ADR 0002](0002-subagent-counted-until-it-ends.md) moved
  away from for presence.
- Append-only per-actor logs: readers in two languages would need a read-time
  fold of the reducer's rules, torn-line handling and a disk bound.

## Consequences and revisit

The split isolates the lead's evidence; it does not make it fresh. The newest
lead observation shown can still be older than the lead's last hook when the
writer refuses a repeat, a fenced or an evicted observation, or times out on
the lock. Sub-agents share one file and can still evict each other's evidence.
A poll reads two files per pane, and a pane now holds up to twice as many
observations: the lead's and its children's. A poll that must parse both full
files costs about as much more as the extra observations it holds. Measured
for one full pane on an M5 Max inside WezTerm 20260905-195314-b99b1ca2, with
both files rewritten before each poll, it took 20.5 to 23.5 ms against 9.2 ms
before the split when the old file held mostly the lead's observations (2.24
times), 14.4 ms when it held an even mix (1.55 times) and 19.6 ms when it held
only sub-agents' (1.20 times), since the old reader hashed every sub-agent id
on each read. A poll where only one of the two files changed took 11 to 15 ms.
`snapshot_id` identifies `lifecycle.json` only. A consumer that asks only about
the lead reads `lead_requests` or `lead_general`; one that reads `general` or
`requests` gets no benefit from the split until it switches. A plugin from
before the split shows none of the sub-agent observations written after it,
and a sweep from before it keeps a binding that holds the new file.

Revisit if a consumer needs retention per sub-agent, if a consumer that cannot
switch keys is misled by the aggregate floors, or if a record schema change is
made for another reason, which would allow folding the two files into one
snapshot.

Enforced by `more_child_observations_than_a_pool_holds_leave_the_leads_newest_in_place`,
`a_leads_observation_goes_to_lifecycle_json_and_a_childs_to_its_own_file` and
`a_corrupt_childrens_file_rejects_only_the_childrens_observations` in the Rust
lifecycle suite; `rust_and_installed_lua_share_relation_cases_and_retention_floors`,
which runs every two-file case of `two_file_cases` through the Rust and the
installed Lua reader; and the fixture cases `children-snapshot` and
`lead-in-children-snapshot` in `tests/fixtures/lifecycle/observations.json`,
which the Rust, Lua and Python validators all run.
