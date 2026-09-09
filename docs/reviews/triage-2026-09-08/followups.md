# Follow-up and pending work

## Automatic launch correlation

Candidate: C5. Owner: attention-v2 launch identity, coordinated with bootstrap's provider activation.

**Problem:** automatic SessionStart claiming is still a gated stand-in. The installed macOS tty test opens `/dev/tty` but rejects its identity. Current Claude documentation also states that command hooks run without a controlling terminal. Opening stdin or `/dev/tty` is not a complete way to identify the pane.

**Affected invariant:** every callback must identify the correct pane and process generation before changing its records. A provider session ID plus a new observation time does not distinguish every old callback from a new process resuming that same session.

**Failure sequence to rule out:** an agent generation ends or is replaced; a later process resumes the same provider session; an old callback arrives without a generation token. Looking up the latest launch by session/tty can assign the old callback to the new launch. A delayed SessionStart needs the same scrutiny.

**Why this triage does not implement it:** the pasted recommendation defines new cross-provider identity semantics. The user requested verification, not live provider activation or a replacement launch mechanism. The current Rust port keeps automatic minting disabled by default.

**Complete work:** define a provider-supported correlation route and how it reaches every later callback; preserve explicit shell claims; prove nested agents, same-session resumes, socket replacement and ordinary zsh launching. Do not assume a reference terminal's PTY-spawn token is available to a WezTerm plugin.

**Acceptance tests:** real Claude, Codex and Pi callback spawn paths with piped stdin; unavailable controlling terminal; exact pane attribution; no inherited launch; mismatched inherited launch; same-session replacement with late Stop and SessionStart; child/nested callbacks; socket rebirth; failure without fallback state corruption.

**Containment:** keep `WEZTERM_ATTENTION_ENABLE_SELF_CLAIM` disabled by default. The configured writer must diagnose unsupported claiming. Do not advertise full provider migration from the stand-in tests.

**Merge impact:** NON-BLOCK for the current default-gated Rust implementation. Automatic provider activation is blocked on this work. This is not permission to enable the gate or modify live hooks.

## Closed PermissionRequest evidence

C7 asked whether the installed Claude version needs neutral `{}` output for a particular permission-hook path. Current docs distinguish no-decision behavior from parse failure; an empty object supplies no permission decision.

**Builder follow-up, 2026-09-08:** installed Claude reports 2.1.266. After the synthetic isolated invocation, one separately authorized interactive contact used a disposable PermissionRequest hook with exit 0 and zero stdout. Claude displayed the normal permission prompt; the requested disposable command was refused and did not run. This refutes the claimed need for `{}` on the tested provider version. No stdout implementation change or live hook registration change was made.

## Pi dispatch verification

Candidate: C17. Owner: Pi adapter/test runtime boundary.

**Observed:** the repository's prescribed Bun command returns 23 passes and 7 failures across 30 tests. The first v2 writer-dispatch case times out at five seconds. The isolated Node dispatch smoke also fails to produce the generated writer's calls log.

**Controls:** invoking a generated script through `/bin/sh` completed in a small control while one direct Node invocation timed out. The stable repository shim completed `--help`. A configured native executable returning nonzero produced one Pi warning and no v1 fallback files. These observations do not yet identify the cause of the suite failure.

**Required work:** distinguish fixture creation/execution, host runtime behavior and adapter process completion. Restore the existing queue, shutdown and no-downgrade proofs; do not remove failing assertions, increase timeouts blindly or rewrite the queue before the cause is known.

**Acceptance:** the prescribed Bun suite and Node dispatch smoke pass on the current source; configured missing/nonzero/diagnosed writers never fall back; real writer input and exit handling remain bounded as required by the existing contract.

**Merge impact:** BLOCK a fresh Pi/full-gate certification until the test failure is resolved or its environmental cause is demonstrated with a passing supported run. This triage does not assert that the live provider has the same failure.

**Builder follow-up, 2026-09-08:** the failure did not reproduce on the unchanged Pi source hashes. The exact prescribed Bun command passed three consecutive runs at 30/30, Node dispatch passed 3/3, TypeScript passed, and `sh tests/gate.sh` exited 0. No Pi code or timeout changed. The original failure is retained as transient host/runtime evidence with no proven exact cause; it no longer blocks current certification.
