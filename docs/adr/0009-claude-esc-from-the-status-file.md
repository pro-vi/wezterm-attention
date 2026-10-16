# ADR 0009: A Claude turn stopped with Esc is read from Claude Code's status file, by the turn follower

- **Status:** Accepted
- **Date:** 2026-10-15

## Context

Claude Code runs no hook when the user presses Esc to stop a turn, so the
`thinking` its prompt wrote stayed on the tab until the next turn ended, and an
Esc at an approval prompt left `notify`. Checked against Claude Code 2.1.283
and again against 2.1.300.

Claude Code keeps a private, undocumented status file for each process it runs,
`sessions/<pid>.json` in its configuration directory. As of Claude Code 2.1.300
its `status` goes from `busy` or `waiting` to `idle` or `shell` within 0.1 s of
an Esc, and to `idle` after a normal turn only once every `Stop` hook has run.

Release 1.2.0 already reads another provider's own file for the one end its
hooks miss: a Codex turn that ends on a model error is read from the session
rollout by a follower process the prompt hook starts.

## Decision

- **One turn follower for every provider.** `src/turn_follower.rs` holds the
  follow request, the per-binding lock, the liveness and currency checks and
  the poll loop; the request names its provider, and the binding, the turn key
  and the turn-end test derive from it. Each provider's file has one reader,
  `src/codex_rollout.rs` or `src/claude_status.rs`, the only code that knows
  that file's path, fields and values.
- **The Claude prompt hook starts a follower for its prompt** when the hook
  proved its host, since the file is named by that process. The follower
  records a lead `user_interrupt` with `source_event` `session_status` and
  clears the activity, through `record_claude_turn_interrupt` under the launch
  and claim locks, only after it read the turn working and then a newer reading
  of no turn, and only while the prompt is the lead's latest and no lead turn
  end came after it.
- **Doubt writes nothing.** A file the reader does not understand, or that is
  not this session's and process's, ends in today's behaviour, never in a
  record. `attention doctor`'s `claude_status` probe reports such a file with
  the Claude Code version it names.
- **A follower's source is a manifest turn-end source no hook declares**
  (`FOLLOWER_SOURCES`), so a hook cannot claim it, and the Rust, Lua and Python
  validators read it from `protocol/v2.json` alone.

## Rationale

A second follower for Claude alone would have kept two ways to start, lock and
admit a follower, and copied the places that named Codex outright. Making the
existing one name its provider keeps one lock and one admission check for both.
The status file showed every Esc measured, including one before any output,
which as of Claude Code 2.1.283 leaves no line in the session transcript.
Failing closed means a change to the file can only bring back the stuck
`thinking`, and doctor shows when that happened.

## Consequences

Positive:

- Esc during a command, before any output, at an approval prompt or while a
  background command runs clears the tab within about a second. Checked once
  each against Claude Code 2.1.301.

Negative:

- One sleeping process per open Claude turn with a proved host, reading a few
  small files once a second.
- A turn that ended normally but whose `Stop` was not recorded is recorded as
  an interrupt; the file cannot tell the two apart.
- No hook consumer runs for this end.
- A plugin from 1.3.0 or earlier reads a lifecycle file holding this end as
  invalid until the plugin is updated and the configuration reloaded.
- Linux, and hooks that name no agent, keep the stuck `thinking`.

## Revisit Triggers

- Claude Code gains a hook for a turn the user stopped: record from it and
  drop the Claude follower.
- `attention doctor` reports `claude_status` findings after a Claude Code
  update.
- Claude Code documents the status file, or stops writing it.

## References

- `src/claude_status.rs`, `src/turn_follower.rs`, `record_claude_turn_interrupt`
  in `src/lifecycle.rs`, `claude_status_probe` in `src/maintenance.rs`
- [Accepted limitations: A Claude turn stopped with Esc is recorded only from its status file](../accepted-limitations.md#a-claude-turn-stopped-with-esc-is-recorded-only-from-its-status-file)
- `tests/rust/lifecycle_spec/claude_esc_turn_end.rs`,
  `tests/rust/lifecycle_spec/claude_status.rs`, `tests/python/claude_esc_probe.py`
