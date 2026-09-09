# Builder follow-up evidence

Checked 2026-09-08 against HEAD `4d3d0a560e23cdc5e260a045361d5395270d7461`. After `git fetch origin`, `origin/main` remained its direct parent. The index was empty and the mixed working tree was preserved.

## C17: Pi dispatch

No Pi source or test change was made. The prescribed command passed three consecutive runs on the hashes recorded by the handoff:

| Check | Result |
|---|---|
| prescribed Bun run 1 | 30 passed, 0 failed; dispatch case 390.28 ms |
| prescribed Bun run 2 | 30 passed, 0 failed; dispatch case 141.43 ms |
| prescribed Bun run 3 | 30 passed, 0 failed; dispatch case 132.20 ms |
| `node tests/pi_node_runtime.mjs` | 3/3 writer calls passed |
| `bun run typecheck` | exit 0 |
| fresh-target `sh tests/gate.sh` | exit 0; 83 Rust, 111 Lua, 30 Pi, Node/runtime and installed-WezTerm smoke passed |

The earlier 23/7 run is retained in `bun-prescribed.log`. Its first generated writer timed out and several later children produced no calls log. The same source now completes repeatedly, so no queue, fallback or timeout change is supported. The exact historical host/runtime cause was not captured; the current certification blocker is cleared by the repeatable passing run, not by claiming a product fix.

One initial builder gate reused the checkout's shared Cargo `target/` and reported 86 Rust tests, including three tests compiled from a separate disposable I1/I2 build whose source was not present here. That result is invalid for this checkout. The final gate used an empty `/tmp/attention-handoff-target.zXqsmN`, rebuilt from current source, and reported the correct 83 Rust tests.

## C7: Claude PermissionRequest stdout

The installed Claude version reported `2.1.266`. A synthetic invocation of the installed bootstrap `permission_request.ts`, with an isolated temporary HOME and no provider process, exited 0 and wrote zero stdout bytes. An initial instrument attempt used a nonexistent hard-coded Bun path and exited 127; it is not evidence about the hook.

Current official Claude hook documentation says command-hook JSON is processed only when exit 0 emits JSON, and that omitting a decision or exiting 0 without JSON leaves the normal permission flow in place. `{}` contains no allow/deny decision.

After separate user authorization, one real Claude 2.1.266 interaction used a disposable PermissionRequest hook that consumed stdin, wrote no stdout, recorded a fixed invocation marker, and exited 0. Claude displayed its normal permission prompt for a `touch` command in the disposable directory. The request was refused. The hook marker existed and the command target did not exist before or after refusal. The debug log contained no parse-error, invalid-JSON or hook-failure match. No live hook registration changed.

Source: <https://code.claude.com/docs/en/hooks#permissionrequest>

## Documentation

- `docs/reviews/2026-09-06-attention-v2-modules.md` now describes implemented Rust/Lua modules, current source paths, required V1 compatibility, disabled automatic self-claim, and the separate provider activation boundary.
- `README.md` now labels direct flat-file writes as V1 compatibility and names `bin/attention` as the V2 producer boundary.
- `docs/record-contract.md` now distinguishes the implemented writer contract from inspected live hook/shell activation.
- `docs/reviews/2026-09-06-attention-v2-modules.html` was regenerated from Markdown with `bun docs/reviews/render-review.mjs`.

## Decisions the spec did not cover

None were made. Automatic launch correlation and notification acknowledgement remain unchanged. C7 is closed as a refuted `{}` workaround on Claude 2.1.266.
