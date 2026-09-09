# Hook review triage after the Rust rewrite

The pasted review was partly stale. The initial triage reproduced a prescribed Bun run with **23 passes and 7 failures**, while six earlier mechanisms were already resolved in the Rust-era code. The builder follow-up on the same source hashes passed that command three consecutive times at 30/30, passed Node dispatch and TypeScript, and completed the full gate. No Pi correction was supported or applied. Documentation was refreshed; one launch-design item remains deferred. A single authorized Claude 2.1.266 interaction refuted the remaining stdout claim.

This is verification and adjudication only. No runtime code, module spec, provider settings or live configuration was changed. The artifacts in this directory are the output of the triage, not an implementation plan.

## Scope and evidence freshness

- Checked on 2026-09-08 against the working tree at HEAD `4d3d0a560e23cdc5e260a045361d5395270d7461`. The Rust rewrite is uncommitted, so HEAD alone does not identify the inspected implementation.
- The remote main lookup returned `d9d3fe326397594b2e29d1aec2cf84b9982439ff`. Nothing was fetched into or reset in the worktree. No staged diff exists.
- The reference clones still match the reported short commits: herdr `4dd9aa5`, orca `75e5c996`, luvus `3d7e9a1`. Reference architecture is context, not proof of a WezTerm integration.
- The current shim selects Rust. Redacted inspection of the user hook configurations and their helpers still found Bun v1 producers. The inspected zsh configuration contains the v1 prompt publisher. This does not claim to enumerate every running process, profile or personal shell override.
- Candidate severity/confidence priors were assigned during normalization, not supplied by the reviewer. `raw-review.md` preserves the source claims and later Rust update. No missing runtime observations were invented.

## Initial triage results

Fact status below applies to the **current working tree**. “Refuted: resolved” does not mean the finding was false before the Rust port.

| ID | Review mechanism | Current result | Disposition / merge impact |
|---|---|---|---|
| C1 | Live deployment and document freshness | Partial: Rust shim selected, inspected provider helpers remain v1; module documentation still needs current source/status links | ACCEPT / NON-BLOCK |
| C2 | Hook gaps and prompt-return ending | Partial: Rust already clears lead activity on prompt return; it preserves children and does not write an end record | DOCUMENT / NON-BLOCK |
| C3 | One-shot reconnect publication | Refuted: resolved by stable-pane observations, retry backoff and per-window ownership | REJECT / NON-BLOCK |
| C4 | Dock PATH executable discovery | Refuted: resolved by GUI executable-directory handoff and Rust fallback resolver | REJECT / NON-BLOCK |
| C5 | SessionStart self-claim as the launch solution | Partial: launch integration remains incomplete; tty and process-generation assumptions are not established by the proposed shortcut | DEFER / NON-BLOCK while default-gated |
| C6 | Retiring the executed manifest | Refuted: Rust embeds and executes the manifest; parity and unknown-type tests pass | REJECT / NON-BLOCK |
| C7 | Permission hooks need `{}` stdout | Refuted: exit 0 with empty stdout preserved the normal Claude 2.1.266 permission prompt | REJECT / NON-BLOCK |
| C8 | Quiet successful hook output | Confirmed and implemented/tested; retain it | DOCUMENT / NON-BLOCK |
| C9 | Pi falls back after writer failure | Refuted: only an unconfigured root falls back; configured failures and exit-zero diagnostics do not | REJECT / NON-BLOCK |
| C10 | Repeated command-owned lock choreography | Refuted: records owns commit/read/decide/apply/post-apply and scoped lock variants | REJECT / NON-BLOCK |
| C11 | Child callbacks must not overwrite lead activity | Partial: correct invariant, already represented in fixtures and child dispatch/tests; stronger parent-byte assertions are optional further coverage | DOCUMENT / NON-BLOCK |
| C12 | Focus hides an unanswered notify | Confirmed shipped policy, not a newly established bug; `auto_clear` can exclude notify | DOCUMENT / NON-BLOCK |
| C13 | Missing launch-level paths in the contract | Refuted: launch activity and acknowledgement paths are now documented | REJECT / NON-BLOCK |
| C14 | Repeated Lua digest cost | Partial: work exists and was measured; that does not justify trusting unchecked filenames | DOCUMENT / NON-BLOCK |
| C15 | No changes/replay API | Confirmed scope; snapshots are not a replay queue | DOCUMENT / NON-BLOCK |
| C16 | Shared raw monotonic clock | Confirmed: Rust calls CLOCK_MONOTONIC_RAW, separately from Unix wall age | DOCUMENT / NON-BLOCK |
| C17 | Pi dispatch verification | Initial confirmed test failure: generated writer dispatch timed out; the later builder follow-up passed repeatedly without a source change | ACCEPT initially / current certification clear |

The live-registration and zsh corrections belong to C1 rather than duplicate candidates. The two reconnect mechanisms are separate C3/C4. The two event-specific stdout concerns are separate C7/C8. C1–C16 cover the supplied review; C17 is the additional current verification finding. Every candidate remains visible.

## Decisions that still matter

**The initial Pi failure is preserved but no longer blocks current certification.** The first triage run timed out in v2 writer dispatch and then lost queued calls. On unchanged source hashes, the prescribed Bun suite later passed three times, the Node smoke passed, and a fresh-target full gate passed. The exact transient host/runtime cause was not captured, so the report preserves both observations without claiming a product fix. See [Pi dispatch follow-up](followups.md#pi-dispatch-verification).

**Do not enable self-claim from the old recommendation.** The fresh macOS test rejects `/dev/tty` as pane identity. Current Claude documentation says command hooks have no controlling terminal. The proposed shortcut needs a different correlation design, not only proof that a file can be opened. The scoped follow-up is in [followups.md](followups.md#automatic-launch-correlation). [Claude hook contract](https://code.claude.com/docs/en/hooks#hook-input-and-output)

**Prompt return is weaker than ending.** Rust writes a lead activity-clear watermark. It does not clear children or end the binding. A later provider event can reactivate activity. An interrupt that stays inside the agent does not return a shell prompt, so this is not continuous proof of the provider's state.

**Notify acknowledgement needs a user policy choice.** Current focus acknowledgement preserves the provider record but suppresses the alert. Stopping that suppression for every notify would also change general/manual notifications. There is no approval in this review to change the default.

**The `{}` workaround is not needed for Claude 2.1.266.** In one authorized interactive contact, an exit-0 PermissionRequest hook wrote no stdout and Claude displayed its normal permission prompt. The command was refused and never ran. This agrees with the current vendor contract: no JSON means no hook decision, while `{}` also contains no allow/deny decision. [PermissionRequest contract](https://code.claude.com/docs/en/hooks#permissionrequest)

## Initial verification run

- Rust: **86 passed**. The nested controlling-tty child test is part of its parent test and is not counted twice.
- Bun/Pi: the initial prescribed command had 23 passed, 7 failed across 30 tests. Environment-isolated reruns also failed, with some downstream counts changing after the first timeout. The later passing evidence is recorded under Builder follow-up below.
- Node/Pi: the isolated smoke failed with missing generated-writer `calls.log`. The direct nonzero native-writer control passed and created no fallback state.
- LuaJIT: **111 passed, 0 failed** with the non-secret working-directory value restored.
- Installed WezTerm protocol/formatter smoke: **35 parser rows, 10 eligibility rows**, UTC formatting and formatter checks passed.
- Full `/gate`, clippy, TypeScript and a visible GUI reattach rehearsal were not run in this triage. This is not a fresh full-build certification.

The first environment-isolated Lua run omitted `PWD`, which the relative test loader uses to resolve the checkout root. It produced 20 failures. The rerun supplied the correct `PWD` and passed. That first instrument failure is retained in the evidence; it is not reported as a product regression.

An early progress update incorrectly repeated the previous 29-test Bun success before the full initial log was checked. That claim was corrected during triage; the later builder follow-up supersedes the initial failure only for current certification and preserves the original evidence.

The installed-WezTerm Lua microbenchmark hashed 1,000 synthetic short child IDs in **58.770 and 59.982 CPU ms** across two config evaluations. This includes loop/assertion overhead and is not total poll latency or a live-load measurement. Preserve identity validation; use actual poll profiling before deciding whether validated-byte caching is worthwhile.

Parallel reviewers became unavailable. Verification continued locally. The default Python lacked `jsonschema`; validation uses the supplied dependency-free review-schema validator instead of installing a package.

## Artifacts and completion state

- [Original input](raw-review.md)
- [Normalized candidates](candidates.json)
- [Adjudication records](adjudications.json)
- [Deferred launch work and closed stdout probe](followups.md)
- [Validation and evidence summary](verification.json)

All 17 records are schema-validated and their IDs match the candidates. Every record now has a terminal disposition. At initial triage, C17 was a confirmed verification failure with an unresolved cause; the builder follow-up below clears the current certification block without rewriting that historical adjudication.

## Builder follow-up (2026-09-08)

C17 no longer reproduces on the unchanged Pi source hashes. The prescribed Bun command passed three consecutive runs at 30/30, the Node dispatch smoke passed 3/3, TypeScript passed, and a fresh-target full repository gate exited 0 with 83 Rust tests. No Pi code or test correction was justified or applied. The original 23/7 evidence remains part of the record; its exact transient host/runtime cause was not captured. An earlier builder gate reused a shared Cargo target containing three tests from another disposable source tree and is excluded. Fresh certification is green on the current source.

C1 documentation was refreshed. The module spec now links current Rust source and distinguishes implemented/selected Rust from the still-V1 inspected bootstrap provider helpers and zsh setup. README labels direct flat-file writes V1 compatibility. The record contract states that implementation is not activation. The module-spec HTML was regenerated from Markdown.

C7 is closed. After the synthetic check, one separately authorized Claude 2.1.266 interaction invoked a disposable PermissionRequest hook that exited 0 with zero stdout bytes. Claude then displayed the normal permission prompt. The command was refused, its target file remained absent, and no live hook registration changed. No blanket stdout workaround was applied. See [builder follow-up evidence](evidence/builder-followup.md) and the [sanitized live-contact record](evidence/claude-permission-live-contact.log).
