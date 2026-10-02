# ADR 0003: The lead and its sub-agents keep lifecycle evidence in separate files

- **Status:** Accepted
- **Date:** 2026-09-28
- **Amended:** 2026-10-02 — readers leave out the sub-agent observations they find in `lifecycle.json`, and `retention_floors` drops the aggregate `general` and `requests`

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
observations already in `lifecycle.json` are not moved, and the file stays
valid; readers leave them out, with no diagnostic. Both readers assemble one
lifecycle facet from the two files, and its sub-agent observations come only
from the children's file. Observations from the children's file have the pools
`child_general` and `child_requests`, and each file's floors appear as
`lead_<pool>` and `child_<pool>`, with no key for the two files together.
Neither the record nor the manifest schema changes.

Until 2026-10-02 both readers instead settled the sub-agent observations in
`lifecycle.json` against the children's file each time they built the facet of
a binding that had one, showing one copy of an observation found in both files
and reporting copies that disagreed as `record_invalid`. No build since the
split writes a sub-agent observation into `lifecycle.json`. On one state store
on 2026-10-02, 4 of 114 `lifecycle.json` files held 72 sub-agent observations,
the newest written on 2026-09-28, and none of the four had a children's file
beside it. Until the same date `retention_floors` also carried `general` and
`requests`, the later floor of that pool in either file.

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
Sub-agent observations already in `lifecycle.json` take places in the lead's
pools until lead observations displace them, so a binding written across the
split shows fewer lead observations than its pools hold, and a lead floor in it
may mark the eviction of sub-agent observations, set before the split or by a
later lead observation.
A poll reads two files per pane, and a pane now holds up to twice as many
observations: the lead's and its children's. A poll that must parse both full
files costs about as much more as the extra observations it holds. Measured
for one full pane on an M5 Max inside WezTerm 20260905-195314-b99b1ca2, with
both files rewritten before each poll, it took 20.5 to 23.5 ms against 9.2 ms
before the split when the old file held mostly the lead's observations (2.24
times), 14.4 ms when it held an even mix (1.55 times) and 19.6 ms when it held
only sub-agents' (1.20 times), since the old reader hashed every sub-agent id
on each read. A poll where only one of the two files changed took 11 to 15 ms.
These figures were measured while readers still settled the old file's
sub-agent observations against the children's file, and have not been measured
since.
`snapshot_id` identifies `lifecycle.json` only. A consumer that asks only about
the lead reads `lead_requests` or `lead_general`; one that asks whether some
evidence of a pool was evicted from either file reads both keys of that pool.
A plugin from before the split shows none of the sub-agent observations written
after it, and a sweep from before it keeps a binding that holds the new file.

Revisit if a consumer needs retention per sub-agent, or if a record schema
change is made for another reason, which would allow folding the two files into
one snapshot.

Enforced by `more_child_observations_than_a_pool_holds_leave_the_leads_newest_in_place`,
`a_leads_observation_goes_to_lifecycle_json_and_a_childs_to_its_own_file`,
`a_corrupt_childrens_file_is_started_again_and_leaves_the_leads_alone`,
`a_childs_observation_in_lifecycle_json_is_left_out_without_a_diagnostic`,
`the_childrens_observations_come_only_from_the_childrens_file`,
`inspect_reads_an_older_lifecycle_json_without_the_childs_observation_it_holds`
and `each_files_floor_is_named_for_its_file` in the Rust lifecycle suite;
`rust_and_installed_lua_share_relation_cases_and_retention_floors`, which runs
every two-file case of `two_file_cases` through the Rust and the installed Lua
reader; and the fixture cases `children-snapshot` and
`lead-in-children-snapshot` in `tests/fixtures/lifecycle/observations.json`,
which the Rust, Lua and Python validators all run.
