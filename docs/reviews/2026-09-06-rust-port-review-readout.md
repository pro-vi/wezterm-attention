---
title: Rust port review readout (U9, U10, U12)
date: 2026-09-06
plan: ../plans/2026-09-06-001-feat-attention-rust-lifecycle-plan.md
raw_reports: ../../.research/rust-port-review-2026-09-06/
status: findings, not yet triaged into fixes
---

# Rust port review readout

Three independent `code-review` runs, one per built unit, each dispatching its own read-only reviewer
workers (Opus and Sonnet only) against disposable state directories. Nothing in the repository was
edited by the reviews. `cargo test` is green (39 tests) and catches none of what follows.

## Read this first

- **Every reviewer is the same model family.** Agreement between workers promoted nothing; only
  executed probes did. Counts of probed findings: U9 all 12 probed or held by probe; U10 18 of 20;
  U12 21 of 26. The unprobed ones are marked in the raw reports.
- **Nothing is live.** `bin/attention:12` still defaults to Python. These are pre-activation parity
  defects in a candidate binary.
- **The plan's tty mechanism is dead on macOS.** Verified by the U10 reviewer and again by me with a
  `pty.fork` probe: `/dev/tty` is a cloning device, `ttyname` returns the literal `/dev/tty` owned by
  root, and a child with closed stdio shows no controlling tty in `ps` or `kinfo_proc`. O5's tty arms
  cannot work for a hook subprocess. The plan's reversal condition 1 applies: self-claim stays off and
  R7 rests on the shell claim. The unit tests did not see this because `FakeTty` overrides both
  `controlling_path` and `fingerprint`.

## Three shared root causes

| Root cause | Where it surfaces | Python does |
|---|---|---|
| `read_record` validates shape only; the record's interior `address`/`launch_id`/`binding_id` is never checked against the path it was read from. `path_matches` exists but is called only by `doctor` on `binding.json` | U9 F1; U10 F3; U12 C1, C4, C5, C7, C8, C9, C12 (15 of 26) | fences every sibling read (`attention.py:1105-1111`, `:1320-1331`, `:2275-2281`) and reports `record_invalid` |
| Decisions computed before the locks are applied inside them unchanged: every sweep closure passed to `commit_nested_with` is `\|_\|` and discards the re-read | U12 C2 (P0); U10 F13 (review file has three writers under three locks) | re-reads under the locks and aborts with `record_invalid "binding changed before sweep apply"` (`:2337-2340`) |
| No pane-presence cache: one `wezterm cli list` per binding row | U9 F3; U12 C-uncached (40 bindings: 3.4 s vs 0.14 s; duplicate diagnostics evict real ones) | one enumeration per query (`:1719-1773`) |

Fixing the first two at the record layer closes more than half of all findings at once.

## Findings that change what the tab shows

| Sev | Unit | Anchor | Failure | Verified by |
|---|---|---|---|---|
| P0 | U10 | `src/lifecycle.rs:1183-1194` | After a resume, no second `end.json` is ever written: `SessionStart(startup) → SessionEnd → SessionStart(resume) → SessionEnd` leaves the binding active forever. Python `attention.py:1470` | reviewer probe |
| P0 | U9 | `src/wezterm.rs:59` | `PaneRow.tty_name` is a non-optional `String`; one pane row with `null` (ssh or tmux panes) fails the whole enumeration, so realm publish and U15's retry loop strand | reviewer probe, confirmed in source |
| P0 | U9/U12 | `src/query.rs:226-267` | A foreign `end.json` flips a live binding to `ended`/`valid`; a pointer naming another launch yields `current: true` | reviewer probe; shared root 1 |
| P0 | U12 | `src/maintenance.rs:315-322`, `:368-385` | Foreign `agents-floor.json` plus a matching `--operation-id` deletes every child including an active in-TTL one, exit 0 | reviewer probe |
| P0 | U12 | `src/maintenance.rs:620-740` | Sweep decisions run unlocked and are applied without re-validation | reviewer flock probe, confirmed in source |
| P1 | U10 | `src/lifecycle.rs:612-629` | Duplicate Codex `Stop` stamps `agents-clear` with the incoming observation, burying a child that arrived after the first Stop. Python `:1587-1590` uses the surviving activity's observation | reviewer probe |
| P1 | U10 | `src/providers.rs:215-216`, `:157-162` | `agent_type` parse failure kills root events; `cwd` capped at 256 bytes instead of 4096, so deep project paths get no state at all | reviewer probe |
| P1 | U10 | `src/main.rs:221-227`, `:326-328`, `:329-349` | Prompt return aborts the identity publication on lock contention; the monotonic observation is captured after stdin is read; `record_invalid` exits 3 without `--strict`; `--debug` writes its envelope to stdout | reviewer probes |
| P1 | U12 | `src/wezterm.rs:375-380` | A socket path containing a space breaks the process negative: live pane reads absent, `end.json` after 60 s | reviewer probe |
| P1 | U12 | `src/maintenance.rs:579-589`, `:794-807`, `:579` | Missing `claim.json` prunes the binding directory; one malformed `claim.json` aborts the whole sweep | reviewer probe |
| P1 | U9 | `src/wezterm.rs:210-219` | Hardcoded bundle fallback executes `/opt/homebrew/bin/wezterm` from a group-writable directory with no ownership check; Python has no fallback | reviewer inspection |
| P1 | U9/U12 | `src/query.rs:122-125` | Missing `incarnation.json` reads as present/confirmed, then as verified absent; Python fails closed | reviewer probe |

The full ranked lists, including the P2 and P3 rows, are in the raw reports.

## Test-instrument gaps (evidence-voiding)

- `tests/gate.sh` runs no `cargo` command and never invokes `tests/tty_input_guard.py`; U9's stated checkpoint is enforced by nothing.
- The protocol-fixture test reads 35 of 58 rows and never invokes `check.py` despite its name; the 13 skipped rows are the path-versus-interior-identity ones, exactly the shared root above.
- Four U12 tests and two U9 tests pass for a weaker reason than their name claims; eleven Python sweep/doctor/bindings tests have no Rust counterpart, so U12's "every Python test has a counterpart" is not met.
- O7's scrape misses `AttentionError::usage`, `record_json` and variable-code call sites, so `bad_usage` is never observed.
- `src/maintenance.rs:225` bakes `env!("CARGO_MANIFEST_DIR")` into the doctor manifest comparison; U14's activation gate would pass vacuously for a binary installed anywhere else.

## Decision items, not patches

- `subagent_presence.source` carries `agent_type`, but schema 2 declares `optional: []`, so O8 and exact Python parity cannot both hold (U10 F15).
- Self-claim and tty resolution: see "Read this first". O5 needs rewriting to two arms unless a macOS controlling-tty mechanism is found for a non-`setsid` child.

## Verified by me, independently of the reviewers

- macOS `/dev/tty` behavior (probe above).
- `PaneRow.tty_name` non-optional (source).
- Sweep closures discard the locked re-read (source).
- `CLAUDE_JOB_DIR` and `CURSOR_AGENT` rejections are ported (`src/providers.rs:237`, `:244`).
- `src/lifecycle.rs` performs no direct file I/O but loads records at seven sites; the plan's "pure" label is not true today.

## Suggested fix order for the builder

1. Record layer: `read_record` takes the expected identity and rejects a mismatch (`record_invalid`); make every sweep closure use the re-read value or abort on change. Re-run all three raw reports' probes; expect roughly half the findings to close.
2. `PaneRow.tty_name: Option<String>`, skip rows without a tty per pane as Python does.
3. The U10 lifecycle divergences: second `end.json`, agents-clear observation, `agent_type` and path caps, observation before stdin, exit codes, `--debug` to stderr, publish never aborted by the prompt-return step.
4. Presence cache in `query.rs`; bounded diagnostics.
5. Test instruments: cargo and tty-guard steps in `tests/gate.sh`, the full 58-row fixture through `check.py`, the eleven missing counterparts.
6. Plan: rewrite O5 per the macOS finding; decide U10 F15.
