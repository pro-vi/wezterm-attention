# Input review

The following user-pasted review predates the Rust rewrite. It is preserved as the input claim set, not current evidence.

## Original review

The review is done. Research synthesis is saved at `.research/synthesis-attention-v2-hooks-and-read-api-2026-09-06.md` (untracked; `.research/` is not in this repo's `.gitignore`). Below is the review of `docs/reviews/2026-09-06-attention-v2-modules.md`.

## Verdict

The module map is sound as a responsibility split, and it is mostly faithful to the uncommitted Python/Lua code. Two facts about reality are missing from it, and they change what the "open decisions" table should say. First, nothing in v2 runs on your machine today: the live Claude and Codex hooks are bootstrap's own bun scripts writing v1 flat markers, your shell is zsh with the v1 `WEZTERM_PANE` precmd, and neither v2 shell file is sourced. Second, the spec's reconnect flow describes the exact behavior that the two open inbox notes from 2026-09-05 show is broken. Everything below is ordered by how much it should change the rewrite of the Rust plan.

Baseline evidence: the non-GUI suites pass (79 Python cases, 35 protocol rows plus 10 eligibility rows, 102 Lua cases). The WezTerm smoke was not run.

## Findings against the three references

**1. The spec has no continuously observable fallback, and every reference has one.** herdr removed the exact Claude hook set we register (`Stop`, `PermissionRequest`, `SubagentStop`, `SessionEnd`) and kept only `SessionStart` for resume identity. Its docs give the reason: session hooks miss permission-approval results and Escape interrupts, so the state machine drifts with nothing to re-anchor it. herdr re-anchors from the screen and the foreground process group. luvus re-anchors from the screen plus a lease TTL plus two consecutive process scans. orca stays hook-only but decays any non-done state after 30 minutes and reconciles on PTY exit. Our only re-anchors are the 30-minute `thinking` TTL and focus acknowledgement. The spec should say that plainly under "Availability and health", and it should name the cheap signal we already have and do not use: the shell prompt returning. `hooks publish` runs on every precmd, which is precisely herdr's "foreground returned to the shell" trigger. Recording "prompt observed after launch X" as ending evidence would make the two-observation process sweep a rare path instead of the only path. The known false positive is Ctrl-Z; the spec's existing "new work after stop can reactivate" rule covers it.

**2. The reconnect flow codifies the two open bugs.** The `runtime.lua` boundary says "one bounded reconnect publication request is the explicit exception". The 2026-09-05 inbox notes measured that this single request fires before the GUI has attached most panes, and that under a Dock-launched GUI it fails outright because `shutil.which("wezterm")` finds nothing on the launchd PATH. So `wezterm.rs` must own executable discovery with a fallback to WezTerm's own executable directory, and `runtime.lua` must retry with backoff until a poll sees no unpublished pane, not latch on spawn. herdr re-publishes a stable blocker every 800 ms because it does not trust even its own push path; orca deliberately leaves its endpoint file in place on shutdown. Repeat publishing is the norm, not an exception.

**3. Launch integration is the biggest open decision, and the spec understates it.** On your actual machine no launch claim exists, so every v2 hook would be rejected with `claim_stale` the moment bootstrap's hooks start calling the CLI. None of the three references claims from the shell. orca stamps a launch token at PTY spawn and lets a tokened, non-replay `SessionStart` re-mint the fence. herdr resets the sequence baseline when the process exits. Both do this from the agent side, not the shell side. The spec's "specify the parent-shell claim/export handshake" keeps the shell as the only minting path, which is why zsh is a problem at all. Fork for the rewrite:

- **A · SessionStart self-claim (recommended).** When `hooks event <provider> SessionStart` arrives with no `WEZTERM_ATTENTION_LAUNCH_ID`, the CLI mints the launch itself and validates the pane through the controlling terminal (`/dev/tty`) instead of stdin, which is a pipe inside a hook. Stale-callback protection then rests on `binding_id = hash(provider, session, launch)` plus monotonic order, which already rejects an old session's late callback. The shell claim becomes an optional refinement, and the zsh limitation stops mattering. Unverified: that a Claude or Codex hook subprocess can open `/dev/tty`. Mark it verify-at-contact.
- **B · Shell claim only.** Keep the spec as written, solve zsh separately, and accept that a forgotten `wezterm_attention_claim &&` silently drops the whole session.

**4. Wire equivalence: keep the manifest executed, and add the test luvus lacks.** luvus publishes JSON Schema files that no validator ever runs, keeps four hand-maintained copies of its contract, and is bound to reality only by a live conformance run. Our `protocol/v2.json` is already read at runtime by both Python and Lua, and the shared fixture file is checked by both. That is stronger than luvus, and the Rust plan's line "retire protocol/v2.json as the authority" would throw it away. Recommend: `include_str!` the manifest in Rust, and add one test asserting every enum and limit in the manifest equals the Rust constant, plus round-tripping every fixture row through the serde types. No code generation needed. This closes the "generation or exhaustive contract fixtures" question in the spec.

**5. Hook contract details worth adding to `main.rs`.** All three references guarantee the hook never blocks the agent, as we do. orca adds two things we do not state. It prints `{}` on stdout before anything else, because its authors found Claude permission hooks fail closed on empty stdout. And it treats "no stdout on success" as a contract, because `SessionStart` stdout is injected into the agent's context. Our CLI already prints nothing on success and only stderr on rejection. Write both down as promises, and verify the `{}` point against the installed Claude Code.

**6. The Pi adapter violates the spec's own downgrade rule.** The spec says rejection of an established new-protocol claim must not silently downgrade to v1. In `pi/index.ts`, `invokeWriter` returns false on any non-zero exit, and `enqueueWriter` then writes a v1 marker. `hooks event` exits 0 on an ignored event, so that case is fine, but exit 3 (state dir unavailable, permissions) silently falls back. The adapter needs to separate "no checkout root" from "root present, writer failed".

**7. Module split lesson from herdr.** herdr keeps detection policy in a pure 556-line module with table-driven tests, while arbitration accreted into a 5,886-line struct that grew a field per race it hit. luvus's dispatch file is the same shape. Our `lifecycle.rs` contract is pure, which is right. But the spec's flow says "command code" does lock, reread, decide, apply. The Python has 14 lock sites across 7 command paths doing that choreography by hand. Put one `commit(scope, decide)` function in `records.rs` that owns lock, reread, decide, apply, and compat output, so `main.rs` never touches a lock.

**8. Subagent count has no precedent in herdr or luvus; orca supports it.** Both herdr and luvus suppress child events and carry a negative test asserting child events never reach the parent. orca keeps a live roster and holds the pane "working" while any child works. Our shipped `+N` and "Claude root Stop does not clear children" match orca. The spec's `providers.rs` proof "child events never become lead activity" is the right invariant. Add the negative test as an explicit fixture case so the Rust port cannot lose it.

**9. Acknowledgement matches the best of the references, with one open question.** herdr stores `seen` on the view struct, derives "done" at read time, and never lets a blocker be acknowledged. orca stores no acknowledgement in the record and instead flags a resumed session's idle landing so badges do not fire. Ours acknowledges one exact activity event id, and `SessionStart` produces a binding with no activity, so a resume never shows a spurious stop. Good by construction. The open question is herdr's rule that `blocked` is not ackable: today focusing a pane clears `notify`, so if you look at a permission prompt and switch away without answering, the tab goes quiet while the agent is still waiting. Decide whether `notify` should clear on focus or only on the next provider event.

## Smaller corrections to the document itself

- `docs/record-contract.md` omits the launch-level `activity.json` and `ack.json` that the spec diagram shows. The code writes them (`apply_activity` targets the launch when no binding exists). The contract doc is stale, not the spec.
- The "Claude/Codex registration" row should say the v2 command has zero live callers and that activation means changing bootstrap's hook scripts, not `~/.claude/settings.json` commands as the README implies.
- The "Automatic launch claiming" row should say the user runs zsh, so the Bash automatic path protects nobody on this machine today. Preserving Bash convenience is still right, but it is not the live path.
- `protocol.lua` keeps "digest checks". The plugin runs a pure-Lua SHA-256 on every child record it reads, every poll, to verify `agent_key`. The Rust plan wanted to drop that. The spec should list it under open decisions with a measurement, or decide that Rust verifies at write and Lua trusts the filename.
- The absence of a `changes` API is supported by the references: herdr keeps a 512-event ring but does not expose replay on the wire, and only luvus ships a cursor. Keeping this cut is fine.
- The time section is correct. Python uses `CLOCK_MONOTONIC_RAW`; Rust must call the same clock through libc, since `Instant` exposes no raw value. Add that the coordinate is per boot and that socket-incarnation scoping is what makes cross-process comparison safe.

---

**MISSION** · `attention-v2-modules review` — `the Rust plan can be rewritten from a module spec whose contracts match how the code and the live setup actually behave`

**TURN** · Read the spec, the Rust plan, the prior review, the record contract, the uncommitted v2 code, the live hook and shell setup, and ran the non-GUI suites. Three Explore agents read herdr, orca and luvus at the pinned commits. Synthesis saved to `.research/`; no repo files changed.

- [x] **READ SPEC AND REALITY** — spec, plan, code, live hooks, shell, inbox notes read; baseline suites green
- [x] **RESEARCH HERDR ORCA LUVUS** — three agent reports in, synthesis written
- [ ] **FORK — launch integration** ← *now*
    - **A ·** `SESSIONSTART SELF-CLAIM` — CLI mints the launch when no shell id is present, validates via `/dev/tty`; removes the zsh problem; verify `/dev/tty` reachability from a hook first — *recommended*
    - **B ·** `SHELL CLAIM ONLY` — keep the spec's handshake, solve zsh separately; forgotten claims drop sessions silently
- [ ] **REVISE SPEC** — spec updated with findings 1 to 9 and the small corrections; `record-contract.md` layout matches the diagram
- [ ] **REWRITE RUST PLAN** — *goal:* the plan marked needs-revision is replaced by one whose units cite the revised spec and carry the added contract tests

## Update during triage

The user then supplied: “its since been rewritten in rust.” Verification was restarted against the Rust working tree on 2026-09-08.

## Additional current verification finding

This item was discovered by triage and was not part of the user-pasted review: the current prescribed Bun test command finishes with 23 passed and 7 failed across 30 tests, beginning with a writer-dispatch timeout. An isolated Node dispatch smoke also fails to produce its expected calls log. The actual cause remains unestablished. It is normalized as C17.
