# Lifecycle contact evidence

The local U1–U8 implementation is complete. This report separates implemented transport from native emission and live activation. Live provider/shell configuration, paid provider calls and deployment were not changed. Independent code review remains separate from the seven quality lenses below.

## Final local gate

`sh tests/gate.sh` passed on 2026-09-12: 105 regular Rust tests plus the explicitly invoked native Codex terminal test, 116 LuaJIT tests, 30 Bun tests, three Node dispatch checks, current/baseline TypeScript, 35 existing protocol rows, 10 eligibility rows, 29 lifecycle value rows and 7 raw-JSON rows, installed-WezTerm smoke, frozen maintenance/reader compatibility, and the 18-row/40-case coverage check. Exit code: 0.

Rust binary SHA-256: `823350bbf1367ceee2df32319b4764b700c7de62a43ab94898d9335c9d90e7f7`; frozen baseline: `45f64cec20eb6a43631e7746e0fbb5b4921ffc29cd83f7ecb01e14ae8c31b1e6`. Protocol SHA-256: `01ca6779d15feeb9c3692794ef260138625b4ed334981344ae66660379ec527d`. The unchanged 2× local budget passed both alternating rounds: round 1: sequential p95 5.476/3.261 ms, child burst 87.057/72.748 ms (candidate/baseline); round 2: sequential p95 5.567/3.531 ms, child burst 79.13/55.822 ms (candidate/baseline). Raw evidence: `/tmp/attention-commit-gate.hAZHsH/gate.log`. Earlier runs failed; the same binary later passed a targeted retry and two complete gates. The restart is not a proven cause, and these results do not establish a cross-machine latency guarantee. The 20-pane Lua exercise verifies 40 snapshot reads and zero writes across two polls with 128 observations per pane; it is not a visible-GUI latency claim.

The standard gate ran naming, TypeScript safety, compatibility, overbuild, invariance, failure-mode and performance lenses sequentially against the 40-file staged scope. The first six found no required changes. Performance is acceptable with the timing-variability note above. Tests cover current-launch admission, independent request/general floors, strict raw JSON, fresh acknowledgement during cache recovery, consumer-local dismissal, and failure between legacy projection and snapshot replacement. Immutable prepared bytes and exact pool-size reuse retain validation and durability. The permission-write experiment was removed. No broad independent code review was performed.

No visible GUI reattach rehearsal was repeated in this change. Recovery evidence is the cold installed reader plus the cache, identity and existing mux-retry fixtures; the GUI publication mechanism was not changed.

## Evidence levels

- **Synthetic writer**: generated native-shaped inputs pass through the production Rust CLI, strict record contract and assertions. This does not prove a provider emits those inputs.
- **Pi runtime dispatch**: the actual extension loader/runner invokes the production extension with synthetic events, through its real process queue into Rust. Both Pi 0.80.5 and 0.84.4 are exercised without constructing a model session.
- **Codex native, local backend**: installed Codex 0.154.0 executes its actual async-question handler, native hook dispatcher and terminal UI against a loopback mock backend. These are real native callbacks with synthetic content, not paid provider calls.
- **Unverified**: no corresponding native emission/contact run was performed. Do not treat it as a passed test or a universal claim of absence.

## Coverage by row

Every supported cell below has a production-writer case in `tests/fixtures/lifecycle/contact-cases.json`. Its exact 18-row set is checked against the plan and the historical Next set minus H13/H14/H21.

| Row | Claude | Codex | Pi |
|---|---|---|---|
| H01 submission | Synthetic writer | Native initial/queued submission | Runtime dispatch |
| H02 preflight | Synthetic writer | Native async question; other branches synthetic | Runtime dispatch |
| H03 tool result | Synthetic writer | Native async publication; generic branch synthetic | Runtime dispatch |
| H04 tool failure | Synthetic writer | No invented dedicated failure hook | Runtime dispatch |
| H05 response/settled | Synthetic writer | Native Stop | Runtime dispatch |
| H06 attempt failure | Synthetic writer | No native StopFailure claimed | Runtime dispatch |
| H07 interruption/abort | No general signal claimed | Synthetic writer | Runtime dispatch |
| H08 approval request | Synthetic writer | Synthetic writer | Unsupported |
| H09 delayed permission notice | Synthetic writer | Unsupported | Unsupported |
| H10 auto-mode denial | Synthetic writer | Unsupported | Unsupported |
| H11 structured question | Synthetic writer | Native nonblocking; blocking branch synthetic | No addon-name inference |
| H12 question-tool return | Synthetic writer | Native publication; blocking return synthetic | Unsupported |
| H15 elicitation request | Synthetic writer | Unsupported | Unsupported |
| H16 selected action | Synthetic writer | Unsupported | Unsupported |
| H17 elicitation notice | Synthetic writer | Unsupported | Unsupported |
| H18 child attribution | Synthetic writer | Synthetic writer | No native child contract |
| H19 compaction attempt | Synthetic writer | Synthetic writer | Runtime dispatch, including later cancellation |
| H20 compaction success | Synthetic writer | Synthetic writer | Runtime dispatch |

Native Claude emission, model-driven Pi emission, ordinary real-provider approval/question/cancel interaction, and live registrations remain unverified and were not activated.

## Codex findings from actual contact

Source pin: `rust-v0.154.0`, commit `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`. The test checks the installed binary version before running.

1. `request_user_input_async` passes through native PreToolUse and PostToolUse, then permits a final response and Stop without a user answer.
2. PostToolUse's `tool_response` is a **JSON string** containing `{"accepted":true}`. The first source-derived test assumed an array and failed at contact. Following `function_tool_response` revealed the single-text-item collapse; the adapter now accepts the observed string shape and rejects unsupported shapes.
3. A test-owned PTY and headless terminal show a real pending-question summary alongside a separately queued input. After Stop, the queued input reaches the local backend while the question remains pending. Initial and queued submission hooks carry no originating question-tool ID.
4. The UI test waits for the actual queued row and pending-question summary, not a phrase left in scrollback. It does not infer queue state from response-request ordinals: the terminal run can make additional requests.

The native-created snapshot is then consumed by installed WezTerm's getter and the consumer example. Publication can select `follow_up` after Stop; one consumer's dismissal does not alter another consumer or the records.

Containment: fresh HOME/CODEX_HOME, ephemeral CLI credential storage, no real credentials, no authorization headers accepted by the mock server, localhost backend, private hook registration only, and no input sent to a user pane. Test-only `@xterm/headless` 5.5.0 is installed in disposable storage; repository dependencies are unchanged.

## Execution identity is a separate verdict

Verified inherited launch plus current matching binding passes the admission tests. Missing inherited identity does not gain rich admission merely from a tty match. A delayed writer cannot append facts after the claim rotates.

A controlled Node exec probe preserved PID, parent PID, executable and process start time across an exec transition. Those metadata fields alone do not identify an execution generation. This is not proof that every possible metadata approach is impossible. A production wrapper-free proof remains **unverified**, and that admission path remains disabled for rich facts.

No process-environment dump was used. Existing environment-scanning production probes were not used to obtain identity evidence.

## Reproduction

`sh tests/gate.sh` includes the normal suites, explicit native terminal test, Pi baseline runtime/type check, frozen compatibility and Rust performance comparison. It provisions test-only packages in a temporary directory when paths are not supplied. The source clone for the pinned Codex tag must be available at `/Users/provi/Development/_sources/codex` or through `ATTENTION_CODEX_SOURCE`.

Useful existing-material overrides are `ATTENTION_PI_BASELINE_ROOT`, `ATTENTION_XTERM_MODULE`, and `ATTENTION_BASELINE_RUST`. They do not change production configuration.

## Decisions the plan did not cover

- Native elicitation correlation includes `mcp_server_name` so two MCP servers reusing an elicitation ID remain distinct. This is the one added public metadata field; it is strict, namespaced, and covered by cross-server fixtures.
- Pi queue entries receive a transport UUID once, separate from native tool/turn IDs. The launch ID is captured with the callback rather than read when the queued process starts.
- Snapshot writes prepare immutable validated bytes before legacy output is applied, then use the existing atomic file writer without validating the same bytes twice. The no-side-effect and partial-write tests cover this boundary.
- Installed-WezTerm tests reuse the existing loader with synthetic state. Frozen compatibility reconstructs the pinned Git source; test-only Pi/xterm packages live in disposable directories. No production dependency was added.

These choices preserve the planned scope. Native Codex contact corrected the assumed receipt shape to the actual JSON string; no answer-text inference or badge-policy change was added.