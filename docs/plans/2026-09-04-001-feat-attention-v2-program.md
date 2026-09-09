---
title: wezterm-attention v2 — program walkthrough
objective: Make the v2 code shape reviewable before implementation by fixing its declarations, call stacks, seams, file ownership, and build order.
type: feat
status: historical
date: 2026-09-04
origin: docs/plans/2026-09-03-001-feat-attention-v2-plan.md
superseded_by: 2026-09-05-001-feat-attention-rust-lifecycle-plan.md
---

# wezterm-attention v2 — program walkthrough

## 0. Brief

1. **wezterm-attention v2** takes pane claims and provider lifecycle events and returns validated records plus a cached tab view.
2. It is made of address data, guarded filesystem state, provider events, and presentation state.
3. An attached WezTerm GUI can now map agent attention to the correct server pane after reconnecting.

**Intent:** inspect the concrete code shape behind the approved corrective architecture before
implementation. The architecture decision remains canonical in
`docs/plans/2026-09-03-001-feat-attention-v2-plan.md#architecture-decision`; this walkthrough does
not amend it.

Contact with the code keeps `plugin/init.lua` as the Lua composition root, makes
`libexec/attention.py` the authority for shell/provider/CLI/sweep mutations, keeps acknowledgement
under Lua ownership, gives review one file per explicit owner, and preserves the old six-value Lua
API as a projection. `[ASSUMED]` installed WezTerm exposes the plugin checkout path. `[OPEN]` Bash can
identify one configured agent launch without duplicate DEBUG-trap claims. `[STALE]` provider
fixtures await current Claude and Codex contact. `[EXECUTED]` the v1 suites and 29-pane tty
republish evidence are green. `[EXECUTED]` installed WezTerm `%s%9f` UTC formatting yields decimal
epoch seconds plus nine fractional digits; its Lua API exposes no monotonic clock.

## 1. Types & signatures

```diff
// protocol/v2.json                                      + NEW · U1
+ type Hex64 = string
    // pre: lowercase [0-9a-f]{64}; parser owns rejection
    // post: safe as one path segment
    // errors: invalid length/case -> record_invalid
    // hides: original socket path or session id used to derive the digest

+ type DecimalNs20 = string
    // pre: exactly 20 decimal digits; never convert the full value to a Lua number
    // post: lexicographic digit order equals numeric order

+ type MonotonicNs20 = DecimalNs20
    // scope: writer order, clear/floor fences, and absence intervals inside one declared scope
    // errors: malformed or cross-scope comparison -> probe_unavailable

+ type UnixNs20 = DecimalNs20
    // scope: TTL and retention age only; never event order, identity, or monotonic fences
    // errors: malformed stored value -> record_invalid; now before write -> clock_skew

+ type PaneAddress = {
+   realm_id: Hex64
+   incarnation_id: Hex64
+   pane_id: CanonicalPaneId
+ }

+ type PaneClaim = {
+   kind: 'claim'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   tty_path: AbsoluteTtyPath
+   tty_fingerprint: TtyFingerprint
+   observed_mono_ns: MonotonicNs20
+ }

+ type AgentBinding = {
+   kind: 'binding'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   provider: 'claude' | 'codex' | 'pi'
+   provider_session_id: ProviderSessionId
+   expected_session_id?: ProviderSessionId
+   transcript_path?: AbsolutePath
+   cwd?: AbsolutePath
+   config_dir?: AbsolutePath
+   model?: string
+   start_source: string
+   observed_mono_ns: MonotonicNs20
+   written_at_unix_ns: UnixNs20
+   writer_version: string
+ }
    // pre: binding_id is the digest of normalized provider, session id, launch id
    // post: every path component and interior address agree
    // errors: unsupported provider/schema or interior mismatch -> typed diagnostic
    // hides: resume argv, provider auth, and whether the conversation still exists

+ type ActivityTarget =
+   | { kind: 'binding'; binding_id: Hex64 }
+   | { kind: 'launch' }

+ type MarkRequest =
+   | { action: 'activity'; type: 'thinking' | 'stop' | 'notify'; source: SafeLabel }
+   | { action: 'review'; owner_id: SafeLabel }
+   | { action: 'clear'; owner_id: SafeLabel }

+ type AgentActivity = {
+   kind: 'activity'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   target: ActivityTarget
+   event_id: UUID
+   type: 'thinking' | 'stop' | 'notify'
+   source: SafeLabel
+   frame?: NonNegativeInt
+   label?: SafeLabel
+   puppet: boolean
+   ttl_ms?: PositiveInt
+   observed_mono_ns: MonotonicNs20
+   written_at_unix_ns: UnixNs20
+ }
    // pre: source/label are bounded and contain no controls
    // post: provider activity affects its binding; unbound mark affects only its launch
    // errors: older -> ignored; equal-but-different -> conflict; invalid -> record_invalid
    // hides: tab priority and acknowledgement; launch activity is ineligible once bound

+ type SubagentPresence = {
+   kind: 'subagent_presence'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   provider: 'claude' | 'codex'
+   agent_id: SafeLabel
+   agent_key: Hex64
+   source: SafeLabel
+   status: 'active' | 'stopped'
+   event_id: UUID
+   observed_mono_ns: MonotonicNs20
+   written_at_unix_ns: UnixNs20
+   ttl_ms: 600000
+ }
    // pre: agent_key equals the digest of validated interior agent_id
    // post: newer replaces older in either direction; equal same is duplicate; equal different conflicts
    // errors: missing/invalid id -> ignored; stop writes a fence even if active is absent
    // hides: +N projection and parent-clear eligibility

+ type SubagentClearWatermark = {
+   kind: 'subagent_clear'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   event_id: UUID
+   observed_mono_ns: MonotonicNs20
+ }
    // post: Codex parent Stop hides active child observations at or below this watermark
    // hides: N child states; one newer child tool hook may become visible again

+ type SubagentRetentionFloor = {
+   kind: 'subagent_retention_floor'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   floor_mono_ns: MonotonicNs20
+   operation_id: UUID
+ }
    // post: under launch lock sweep rereads/recomputes, then replaces floor before covered deletion
    // post: same operation id may finish covered deletion/projection but never advances floor
    // errors: eligible active record blocks floor advancement and unsafe cap pruning
    // hides: removed stopped/expired/clear-covered snapshots from delayed writers

+ type ReviewClaim = {
+   kind: 'review'
+   schema: 2
+   address: PaneAddress
+   owner_id: SafeLabel
+   owner_key: Hex64
+   event_id: UUID
+ }
    // pre: owner_key is the digest of the validated owner_id
    // post: exact owner isolation replaces clock ordering; source clear removes only its owner
    // post: Alt+B may clear all owners; a claim completed afterward is a new request
    // errors: filename/interior mismatch -> record_invalid
    // hides: other review owners and agent activity

+ type BindingEnd = {
+   kind: 'binding_end'
+   schema: 2
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   reason: EndReason
+   operation_id?: UUID
+   event_id: UUID
+   observed_mono_ns: MonotonicNs20
+   written_at_unix_ns: UnixNs20
+ }
    // post: applies iff address + launch_id + binding_id match; sweep ends retain operation_id
    // hides: deletion and retention schedule; an end never rewrites a binding

+ type AbsenceProbe = {
+   kind: 'absence_probe'
+   schema: 2
+   address: PaneAddress
+   operation_id: UUID
+   observed_mono_ns: MonotonicNs20
+ }
    // post: same operation id cannot become a second absence; new evidence removes the probe

+ type DiagnosticCode =
+   | 'identity_unpublished' | 'claim_stale' | 'unsafe_tty'
+   | 'realm_unavailable' | 'incarnation_changed' | 'record_invalid'
+   | 'future_schema' | 'binding_conflict' | 'probe_unavailable' | 'clock_skew'
+   | 'integration_version_mismatch' | 'state_permissions' | 'bad_usage'

+ type Diagnostic = {
+   code: DiagnosticCode
+   message: string
+   context: Record<string, string | number | boolean | null>
+   help: string
+ }

+ type CliResponse<T> = {
+   schema: 1
+   command: string
+   status: 'ok' | 'findings' | 'unavailable' | 'usage_error'
+   complete: boolean
+   result: T
+   diagnostics: Diagnostic[]
+ }
    // post: --json emits exactly one response on stdout for exits 0-3; stderr stays empty
    // hides: human formatting; complete=false reports bounded or unavailable coverage

+ type BindingView = {
+   address: PaneAddress
+   launch_id: UUID
+   binding_id: Hex64
+   provider: 'claude' | 'codex' | 'pi'
+   provider_session_id: ProviderSessionId
+   binding_phase: 'active' | 'ended'
+   pane_presence: 'present' | 'verified_absent' | 'unavailable'
+   reader_confidence: 'confirmed' | 'unconfirmed'
+   binding_health: 'valid' | 'conflicted' | 'invalid' | 'future_schema'
+ }

+ type BindingsResult = {
+   rows: BindingView[]
+   scanned: NonNegativeInt
+   returned: NonNegativeInt
+   truncated: boolean
+ }

+ type DoctorResult = {
+   scope: ('state_files' | 'socket' | 'processes' | 'permissions' | 'versions')[]
+   unobserved: ('gui_user_vars')[]
+   probes: { name: string; status: 'healthy' | 'finding' | 'unavailable' }[]
+ }

+ type SweepRequest =
+   | { apply: false; realm_id?: Hex64 }
+   | { apply: true; realm_id?: Hex64; operation_id: UUID }
```

```diff
// libexec/attention.py                                  + NEW · U2/U3/U6
+ class WriterClock(Protocol):
+     def monotonic_ns20(self) -> MonotonicNs20: ...
+     def unix_ns20(self) -> UnixNs20: ...
+
+ @dataclass(frozen=True)
+ class RuntimePorts:
+     state: StateFiles
+     clock: WriterClock
+     panes: PaneLister
+     tty: TtyWriter
+     processes: ProcessProbe

+ @dataclass(frozen=True)
+ class ApplyResult:
+     disposition: Literal['applied', 'confirmed', 'replaced', 'skipped',
+                          'repaired_projection', 'ignored', 'conflict']
+     diagnostic: Diagnostic | None
+     event_id: UUID | None

+ def parse_provider_event(
+     provider: Literal['claude', 'codex', 'pi'],
+     event_name: str,
+     payload: JsonValue,
+     env: Mapping[str, str],
+ ) -> DomainEvent | IgnoredEvent
    # pre: payload and environment are untrusted
    # post: child tool -> ChildWork; Claude/Codex SubagentStop -> SubagentStopped
    # post: Codex root Stop -> LeadStoppedWithSubagentClear; all normalize before policy
    # ordering: classify exact cleanup before rejecting child/background lead-state writes
    # errors: malformed/unknown -> IgnoredEvent with one DiagnosticCode
    # hides: provider JSON field names from every downstream transition

+ def claim_launch(env: Mapping[str, str], ports: RuntimePorts) -> ApplyResult
    # pre: supported shell minted launch_id before starting the agent
    # post: claim.json is durable before OSC publication
    # errors: shell preexec receives exit 0 or 3; no provider-safe override applies
    # hides: hashing, file layout, locking, and tty escape bytes

+ def publish_current(env: Mapping[str, str], ports: RuntimePorts) -> PublishReport
    # post: republishes the existing v1 id and, when valid, the existing v2 claim
    # errors: no claim is v1-only; unsafe tty is reported and never opened
    # hides: OSC encoding and claim lookup

+ def mark(request: MarkRequest, env: Mapping[str, str], ports: RuntimePorts) -> ApplyResult
    # pre: source is validated (direct CLI default 'manual'); claim matches current launch
    # post: activity targets current binding or unbound launch; review targets source owner
    # errors: no safe claim -> exit 3; duplicate repairs any stale v1 projection with exit 0
    # hides: target selection, v1 projection, and review owner path

+ def apply_event(event: DomainEvent, ports: RuntimePorts) -> ApplyResult
    # pre: event is normalized; claim must match before the launch lock
    # post: monotonic order is captured before hook stdin; Unix time only after a new event wins
    # post: no older event replaces newer state; newer child continuation may replace stopped
    # post: Codex parent Stop commits lead activity before its one binding clear watermark
    # errors: unsafe/stale is a typed non-throwing disposition
    # hides: child snapshot/watermark count, temp naming, fsync sequence, and retention

+ def publish_realm(socket_path: AbsoluteSocketPath, ports: RuntimePorts) -> PublishReport
    # pre: pane rows come from the selected realm's wezterm cli
    # post: each accepted tty receives one bounded OSC write; claim files never change
    # errors: partial success returns counts + per-pane diagnostics; no pane aborts siblings
    # hides: pane enumeration command and tty open flags

+ def read_bindings(root: AbsolutePath, ports: RuntimePorts) -> list[BindingView]
    # post: duplicate provider/session ids are all conflicted; rows use deterministic address order
    # errors: invalid/future records remain visible and immutable
    # hides: raw file paths and resume commands

+ def doctor(root: AbsolutePath, ports: RuntimePorts) -> list[Diagnostic]
+ def sweep(root: AbsolutePath, request: SweepRequest, ports: RuntimePorts) -> SweepReport
    # pre: sweep consumes only candidates produced by the same strict readers as doctor
    # post: current, live, unavailable, invalid, negative-age, and future-schema records are untouched
    # post: Unix time derives retention age; monotonic time alone derives order/fences/absence gap
    # errors: unavailable evidence returns exit 3; it never becomes absence
    # hides: process scanner, operation-id replay, two-observation persistence, and retention traversal

+ def main(argv: Sequence[str], stdin: BinaryIO, env: Mapping[str, str]) -> int
    # errors: default hooks event always exits 0; --strict and direct commands use exits 0-3
    # hides: production bindings for RuntimePorts
```

```diff
// bin/attention                                         + NEW · U2
+ attention hooks claim [--json]
+ attention hooks publish [--realm SOCKET] [--json | --quiet] [--all-details]
+ attention hooks event <claude|codex|pi> <EVENT> [--strict] [--debug]
+ attention mark <thinking|stop|notify|review|clear> [--source LABEL] [--json]
+ attention bindings [--json] [--realm ID] [--provider NAME] [--limit 1..1000 | --all]
+ attention doctor [--json]
+ attention sweep [--realm ID] [--apply --operation-id UUID] [--json] [--all-details]
    # pre: shell entrypoint resolves its own checkout and python3
    # post: execs the Python authority without an intermediate serializer
    # post: attention and attention hooks without a leaf show scoped help + one example
    # errors: missing python3 emits one fixed stderr line and exits 3; no JSON serializer can run
    # errors: obsolete flat claim/publish/hook spellings exit 2 with the new path
    # hides: Python module location; makes no PATH-install promise
```

```diff
// shell/wezterm-attention.zsh                           + NEW · U2
// shell/wezterm-attention.bash                          + NEW · U2
+ wezterm_attention_preexec(command: string) -> void
    # pre: runs once for a top-level command; supported command set is configurable
    # post: matching claude/codex/pi launch inherits one fresh launch_id
    # errors: missing root/python -> no-op plus one shell diagnostic
    # hides: command-token parser and shell-hook registration

+ wezterm_attention_precmd() -> void
    # post: republishes the existing claim; never rotates away terminal activity
    # errors: no claim -> publishes v1 pane id only
    # hides: OSC encoding and tty path capture
```

```diff
// plugin/init.lua                                       ~ MODIFIED · U1/U2/U4/U5/U6
+ local function compare_ns20(left: DecimalNs20, right: DecimalNs20) -> -1 | 0 | 1
+ local function wezterm_now_unix_ns20() -> UnixNs20?, DiagnosticCode?
+ local function age_exceeds_ms(now: UnixNs20, written: UnixNs20, ttl_ms: PositiveInt)
+   -> boolean?, DiagnosticCode?
    -- split 11-digit seconds from 9-digit nanos; never tonumber the 20-digit value
    -- exact equality remains eligible; malformed/future age fails closed

+ type PaneRead =
+   | { kind = 'v2', address = PaneAddress, launch_id = string }
+   | { kind = 'v1', marker_id = string }
+   | { kind = 'unpublished', domain = string }
+   | { kind = 'invalid', diagnostic = Diagnostic }

+ type AttentionView = {
+   address?: PaneAddress
+   activity_type?: 'thinking' | 'stop' | 'notify' | 'review'
+   frame?: integer
+   indicator: string
+   color?: string
    -- present only when an activity/review marker wins; count-only leaves it absent
+   source?: string
+   provider?: 'claude' | 'codex' | 'pi'
+   puppet: boolean
+   subagents: integer
+   review: boolean
+   binding_phase?: 'active' | 'ended'
+   pane_presence: 'present' | 'verified_absent' | 'unavailable'
+   reader_confidence: 'confirmed' | 'unconfirmed'
+   binding_health: 'valid' | 'conflicted' | 'invalid' | 'future_schema'
+   base_title: string
+   settled_title?: string
+ }
    -- post: built-in formatter, manual formatter, redraw equality, and compatibility tuple
    --       consume this same named value
    -- hides: disk layout, provider payloads, and liveness probes

+ local function resolve_pane_read(pane) -> PaneRead
    -- errors: mux method failure stays distinct from absent user var
    -- hides: v2 user-var JSON and local-domain v1 fallback

+ local function read_attention_view(read: PaneRead, now_unix_ns: UnixNs20?, opts) -> AttentionView?
    -- pre: called only by poll; never by format-tab-title
    -- errors: read/stat error preserves last view; invalid/future age omits only TTL-bearing state
    -- hides: strict-v2/tolerant-v1 parsing, monotonic fences, Unix TTL, and derived count

~ function M.poll(window, opts) -> nil
    -- post: samples UTC once; cache updated; first-ineligible wakeup token rearmed from fresh records
    -- post: call_after wakes a reread/recompute and never changes eligibility by itself
    -- errors: unavailable UTC makes TTL-bearing state ineligible for this poll and logs once
    -- hides: file reads, title sampling, wakeup scheduling, and redraw comparison

+ function M.doctor(window?) -> Diagnostic[]
    -- post: names GUI-only unpublished/invalid user-var findings
    -- errors: pane API failure is probe_unavailable, not healthy
    -- hides: CLI doctor and filesystem/process diagnostics

~ function M.get_attention(marker_id, opts)
~   -> state?, frame?, source?, puppet, subagents, review
    -- post: exact six-value compatibility contract is unchanged
    -- hides: new axes; callers needing them use formatter context or CLI JSON

~ function M.apply_to_config(config, opts?: {
+   integration_root?: absolute_path,
+   supported_commands?: string[],
+   settled_title_fallback?: boolean,
+   show_puppet?: boolean,
    ...existing_options
  }) -> nil
    -- composition root
    -- binds: io.open/os.rename; wezterm.background_child_process; WezTerm pane APIs;
    --        WezTerm UTC clock + call_after wakeup; protocol/v2.json; display policies
    -- errors: unresolved integration root leaves v2 publishers disabled and doctor-visible
    -- hides: object graph and plugin-checkout discovery
```

```diff
// pi/index.ts                                           ~ MODIFIED · U3
+ type WriterRequest =
+   | { kind: 'binding'; provider: 'pi'; sessionId: string; sessionFile?: string;
+       cwd: string; model?: string; startSource: PiStartSource }
+   | { kind: 'activity'; type: 'thinking' | 'stop' | 'notify'; label?: string }
+   | { kind: 'review'; owner: 'pi-bus'; set: boolean }

+ function requestFromSessionStart(
+   event: SessionStartEvent,
+   ctx: ExtensionContext,
+ ): WriterRequest
    // pre: ctx.sessionManager owns actual session id/file
    // post: reload is confirmation; new/resume/fork remain distinct
    // errors: invalid context -> request omitted, host continues
    // hides: Python provider payload and filesystem records

~ function enqueue(request: WriterRequest): Promise<void>
    // post: request order equals child-process invocation order
    // errors: one failed child never poisons later queue entries
    // hides: subprocess, disk latency, and v1 dual-write

~ export default function weztermAttentionPiExtension(pi: ExtensionAPI): void
    // composition: session_start + existing lifecycle + bus -> one queue
    // post: handlers enqueue synchronously and never await writer I/O
    // hides: session persistence and provider semantics from bus callers
```

## 2. Call stacks

```diff
// supported agent launch
shell command entered
+ wezterm_attention_preexec(command)                               · U2
+   guard command ∈ supported_commands -> return
+   guard WEZTERM_ATTENTION_ROOT usable -> v1-only diagnostic
+   mint launch_id
+   export WEZTERM_ATTENTION_LAUNCH_ID
+   attention hooks claim
+     main()
+       claim_launch()
+         derive PaneAddress + tty fingerprint
+         StateFiles.replace(claim.json)         // durable before publication
+         TtyWriter.publish(v1 + v2 user vars)
agent process starts with the accepted launch id
```

```diff
// provider event to durable state
Codex internal/synthetic child -> no supported hook invocation; this stack does not run
Claude/thread-spawned-Codex hook stdin OR Pi WriterRequest
+ main(['hooks', 'event', provider, event])
+   observed_mono_ns = WriterClock.monotonic_ns20()      // before hook stdin
+   read bounded hook payload
+   parse_provider_event()
+     classify child tool | SubagentStop | Codex parent Stop
+     guard child lead-state/background/Cursor/thread mismatch -> ignored
+     guard provider source is known -> ignored
+   claim_matches(event.launch_id)
+     guard mismatch -> ignored 'claim_stale'
+   StateFiles.with_launch_lock()
+     compare_observation(current, observed_mono_ns)
+       guard older -> ignored
+       guard equal + unequal semantics -> conflict
+       guard active observation < stopped observation -> ignored as older
+       strictly newer active after stopped -> replace as continued work
+       guard active observation <= agents-clear watermark -> ignored by binding fence
+       guard active/stopped observation <= agents-floor -> ignored by retention fence
+     accepted new TTL/retention event -> written_at_unix_ns = WriterClock.unix_ns20()
+     apply_event()
+       binding start -> write binding -> replace current-binding pointer
+       activity      -> replace exact binding/activity.json
+       end           -> write exact binding/end.json
+       child tool    -> replace exact agents/<hash(agent_id)>.json as active
+       SubagentStop  -> replace same exact record as ordered stopped, even if absent
+       Codex Stop    -> write lead Stop -> replace exact binding agents-clear.json
+     reconcile_v1_projection()
+       active/stopped/parent-clear -> add/remove/clear flat v1 .agents
+       duplicate -> repair missing/different v1 without new timestamps or v2 event
```

```diff
// GUI reattach recovery
M.poll(window)                                                     · U2
+ resolve_pane_read(pane)
+   missing mux user var
+ request_republish_once(domain)
+   wezterm.background_child_process([attention, hooks, publish, --realm, socket, --quiet])
+     publish_realm()
+       PaneLister.list(socket)
+       for pane row:
+         guard canonical pane id + same-UID tty + character device -> skip unsafe
+         read current claim
+         guard claim tty fingerprint matches -> publish v1 only
+         TtyWriter.publish(v1 + current v2 claim)
next M.poll()
+ resolve_pane_read(pane) -> v2
+ read_attention_view() -> confirmed
```

```diff
// normal poll, render, and acknowledgement
M.poll(window)                                                     · U1/U4/U5
~ now_unix_ns = wezterm_now_unix_ns20()              // one sample for the whole poll
~ earliest_first_ineligible = nil
~ for pane:
+   resolve_pane_read()
+   read_attention_view(read, now_unix_ns)
+     strict parse activity + active/stopped presences + binding clear + retention floor
+     order/fences = compare only MonotonicNs20 values
+     TTL = compare UnixNs20 digit strings + safe 11-digit seconds / 9-digit nanos
+     eligible child = active + newer than clear/floor + UTC age <= 600s
+     malformed/unavailable/negative age -> omit only TTL-bearing record + diagnostic
+     collect earliest written_at + TTL + 1ns first-ineligible instant
+   sample_settled_title()
+   build_attention_view()
+   attention_cache[full_address] = view
+ rearm one tokenized wezterm.time.call_after wakeup
+   callback -> rerun poll; exact-boundary result rearms; callback never expires state directly
~ if focused active pane:
+   acknowledge_focused_activity(view.event_id)    // exact activity only
~ if derived visible view or eligible subagent count changed:
    request_tab_bar_redraw()

format-tab-title                                                   · U4
+ cached_view_for_tab()
+ resolve_visible_attention(views)
+   winning marker -> append +N; use colors[marker]
+   no marker + subagents -> "+N ", type=nil, color=nil
+ build_formatter_context(view)
+ decorate_tab_title()
+   color=nil -> plain title in default tab colors
    // no file, process, clock, subprocess, title write, or acknowledgement path
```

```diff
// validated consumer read
attention bindings --json                                         · U6
+ read_bindings()
+   enumerate realm/incarnation/pane claims
+   validate interior addresses and versions
+   derive binding_phase from exact end
+   derive pane_presence from conservative probes
+   derive reader_confidence from current evidence
+   group by (provider, provider_session_id)
+     duplicate group -> every member binding_health='conflicted'
+ apply realm/provider filters + deterministic order + default 100 rows
+ serialize CliResponse<BindingsResult>      // facts only; no resume command
```

```diff
// doctor and sweep
attention doctor                                                  · U6
+ doctor()
+   same strict readers as normal commands
+   socket + permissions + version + process + UTC-format probes
+   map every failure to one DiagnosticCode
+   report scope + unobserved=['gui_user_vars']

attention sweep
+ preview by default; write only with --apply --operation-id UUID
+ sweep(request)
+   plans = strict_sweep_plans()
+   for plan:
+     guard unavailable/invalid/future -> preserve all state
+     UnixNs20 age selects TTL/30d candidates; invalid or negative age is preserved
+     MonotonicNs20 alone selects absence gap, child order, clear, and retention floor
+     subagent plan = stored-floor replay | safe prefix | active-child block | none
+     binding plan = preserve live/current | replay | first absence | exact end
+     if preview -> emit both plans, continue with no write
+     with launch lock -> reread/recompute subagent plan
+       stored floor operation -> delete only currently covered files; never advance floor
+       safe prefix -> replace agents-floor.json -> delete currently covered files -> reconcile v1 .agents
+     apply binding plan + prune eligible ended history after 30d / per-realm cap 500
```

### Renderer behavior: marker owns tint; count owns text

This rule applies to both v1-adapted and native v2 views. A subagent count changes text only; a
winning activity or review marker is the sole source of a state color. Acknowledgement and the
ten-minute subagent TTL are unchanged; its age now comes from `written_at_unix_ns` plus one
poll-scoped WezTerm UTC sample.

The count-only result is `indicator="+N "`, `type=nil`, and `color=nil`; the tab therefore keeps
its default colors.

| Winning marker | Subagents | Projection | Color |
|---|---:|---|---|
| None | `0` | Empty indicator | None |
| None | `N > 0` | `+N`, `type=nil` | `color=nil`; default tab colors |
| Activity or review | `0` | Marker glyph | Winning marker tint |
| Activity or review | `N > 0` | Marker glyph with appended `+N` | Winning marker tint |

#### Subagent presence lifecycle

1. **Start** — Creating a child writes nothing. Presence begins only when a child tool hook supplies
   a valid non-empty `agent_id`.
2. **Work** — The tool hook atomically replaces that child's exact
   `bindings/<binding_id>/agents/<hash(agent_id)>.json` as `active`, with a fresh event id, monotonic observation, Unix write time, and TTL. It never writes lead activity.
3. **Stop** — Claude and thread-spawned Codex `SubagentStop` carry the same `agent_id`. The writer
   atomically replaces that exact presence as ordered `stopped`, even when no active record exists.
   A duplicate stop is a no-write exit-zero result. Missing or invalid ids touch no path.

`stopped` replaces rather than deletes the v2 snapshot because deletion would discard the order
fence and let an older in-flight tool writer recreate the count. Older work loses; strictly newer
work after a stop-hook continuation replaces `stopped` as `active` and becomes visible again. Codex
`stop_hook_active` is not stored; the observation sequence represents stop/continue/stop directly.

**Fallback:** Codex parent `Stop` writes lead Stop first, then one `agents-clear.json` watermark for
that exact binding. Older child observations disappear together; a newer tool hook from a child
still running may count again. Claude has no parent clear because its background children may
outlive parent Stop. Active presence counts while one poll-scoped UTC value is no greater than
`written_at_unix_ns + 600_000_000_000`; exact equality counts and one nanosecond later does not.
Malformed or negative age omits that presence and reports a typed diagnostic. Expiry remains a
read-time derivation with no file write. `SubagentStart` remains unused.

**Retention:** Under `sweep --apply`, a presence becomes 30-day age-eligible from its latest
accepted `written_at_unix_ns`; invalid or negative age cannot authorize deletion. An old or excess
contiguous ineligible prefix is compacted by
replacing monotonic `agents-floor.json` before deleting covered files. An eligible active child
blocks unsafe floor advancement; a newer observation above the floor remains valid. Replaying the
stored floor operation id finishes only covered deletions and v1 reconciliation; it never advances
the floor. Apply acquires the launch lock and rereads/recomputes before replacing the floor or
deleting, so a child reactivated after preview is preserved.

Codex internal and synthetic children invoke neither supported lifecycle nor child-context tool
hooks, so the adapter invents no event or presence for them. Every accepted child work, stop, or
parent-clear transition reconciles the flat v1 `.agents` projection; duplicate stop and parent-stop
retry repair a missing projection without a new v2 event.

**Ownership:** `wezterm-attention` owns v2 normalization, snapshots, the Codex clear watermark, TTL
derivation from Unix write time, monotonic ordering/fences, and v1 `.agents` projection. Bootstrap owns its current flat v1 hook scripts and live
provider registration; deferred U8 makes those scripts thin `attention hooks event` callers.

### CLI commands: behavior and call stacks

These are planned v2 contracts, not implemented behavior. The CLI has five top-level commands and
seven executable leaves. `hooks` owns callback entrypoints; a supported coding agent does not
normally call them directly. An agent acting as an operator primarily uses `bindings --json` and `doctor --json`,
uses `mark` only for an explicit custom event, and may preview `sweep` without mutation.

`attention` and `attention hooks` without a leaf print scoped help, valid choices, and one example.
The three obsolete flat spellings are rejected with exit `2` and a new-path hint. Machine mode emits
one `CliResponse` on stdout for exits `0` through `3`; rows and details are deterministically ordered
and bounded, and `complete=false` names incomplete coverage. Default `hooks event` is the exception:
it emits no stdout and always exits `0`; `--strict` exposes normal exits and `--debug` writes one
sanitized response to stderr. `--json` and `--quiet` are mutually exclusive. Failure before Python
starts is the only non-JSON exit: the wrapper emits one actionable stderr line and returns `3`.

Provider-specific `hooks event --help` includes Claude/Codex `SubagentStop` and deliberately omits
`SubagentStart`. Default mode ignores an invalid child id with exit `0`; strict mode returns `1`.

| Command | Normal caller | Behavior | Durable writes | Exit contract |
|---|---|---|---|---|
| `attention hooks claim [--json]` | Shell preexec before a configured agent | Make the pre-minted launch id this pane's current claim, then emit v1/v2 identity to its tty | `claim.json` | `0` claimed; `3` identity or tty unavailable |
| `attention hooks publish [--realm SOCKET] [--json \| --quiet] [--all-details]` | Shell prompt or WezTerm recovery callback | Replay this pane's claim, or every safe current claim in one realm; never mint or alter a claim | None | `0` all published; `1` partial/skipped; `3` realm unavailable |
| `attention hooks event PROVIDER EVENT [--strict] [--debug]` | Claude, Codex, or Pi callback | Normalize one JSON payload; child work writes active presence, Claude/Codex `SubagentStop` writes exact ordered stopped presence, and Codex parent `Stop` writes one clear watermark | Exact launch/binding/presence records, then reconciled v1 activity and `.agents` projections | Default always `0`, including invalid child id; strict uses `0` applied, `1` ignored/conflict, `2` bad input, `3` unavailable |
| `attention mark STATE [--source LABEL] [--json]` | Human or custom tool | Apply source-owned activity, review, or clear to the safe current target | Activity or per-owner review, then reconciled v1 projection | `0` applied/duplicate/repaired; `3` no safe current claim |
| `attention bindings [--json] [--realm ID] [--provider NAME] [--limit 1..1000 \| --all]` | Restore/checkpoint consumer | Return validated binding axes in stable order; default to 100 rows; never return resume argv | None | `0` successful snapshot; `1` snapshot with findings; `3` snapshot unavailable; `complete` separately reports truncation |
| `attention doctor [--json]` | Human or diagnostic agent | Inspect state files, sockets, processes, permissions, and versions; state that GUI user vars were not observed | None | `0` named CLI scope healthy; `1` findings; `3` required probe unavailable |
| `attention sweep [--realm ID] [--apply --operation-id UUID] [--json] [--all-details]` | Human or scheduled maintenance | Preview by default; under `--apply`, conservatively end after two independent absences, compact subagent prefixes floor-first, and prune eligible history | Under `--apply`: absence probe, exact end, retention floor, eligible deletion | `0` complete; `1` skips/findings; `3` no safe evidence |

```diff
// attention hooks claim
main(['hooks', 'claim'])
+ claim_launch(env, ports)
+   normalize PaneAddress + pre-minted launch_id + tty fingerprint
+   StateFiles.replace(claim.json)
+   TtyWriter.publish(v1 + v2)
+ render text | CliResponse when requested
+ return 0 | 3
```

```diff
// attention hooks publish [--realm SOCKET]
main(['hooks', 'publish', ...])
+ if --realm:
+   publish_realm(socket, ports)
+     PaneLister.list(socket)
+     each row -> validate tty -> read claim -> publish v1[/v2]
+ else:
+   publish_current(env, ports)
+     validate current tty -> read claim -> publish v1[/v2]
+ summarize counts + skipped_by_code + at most 50 details unless --all-details
+ render text | CliResponse, or nothing under --quiet
+ return 0 | 1 | 3
```

```diff
// attention hooks event PROVIDER EVENT [--strict] [--debug]
Codex internal/synthetic child -> no invocation; adapter invents no event
main(['hooks', 'event', provider, event, ...])
+ refuse TTY or empty stdin without waiting
+ observed_mono_ns = WriterClock.monotonic_ns20()      // before reading stdin
+ payload = read_bounded_json(stdin)
+ parse_provider_event(provider, event, payload, env)
+   classify child tool | SubagentStop | Codex parent Stop before lead-state guards
+   guard background/nested/unknown lead writes -> IgnoredEvent
+   validate agent_id -> agent_key digest + exact parent binding
+ claim_matches()
+ StateFiles.with_launch_lock()
+ compare_observation(current, observed_mono_ns)
+   older/equal conflict -> ignore or conflict before Unix sampling
+ accepted new TTL/retention event -> written_at_unix_ns = WriterClock.unix_ns20()
+ apply_event()
+   binding -> durable record, then pointer
+   activity -> exact binding activity
+   end -> exact binding end
+   child tool -> exact active SubagentPresence
+   SubagentStop -> exact ordered stopped presence, even if active is absent
+   Codex parent Stop -> lead Stop then one binding clear watermark
+   older active loses to stopped/clear; strictly newer continuation may reactivate
+   active/stopped observation <= retention floor -> ignored
+ reconcile_v1_projection()
+   child work/stop/parent clear -> add/remove/clear flat v1 .agents
+   duplicate stop/parent retry -> repair missing v1 projection, no new timestamps or v2 event
+ default -> no stdout, at most one sanitized stderr diagnostic, return 0
+ --strict -> return 0 | 1 | 2 | 3
+ --debug -> emit one sanitized CliResponse to stderr
```

```diff
// attention mark STATE [--source LABEL] [--json]
main(['mark', state, ...])
+ parse MarkRequest + validate source
+ guard current claim matches -> 3 'claim_stale'
+ target = current binding ?? current launch
+ apply activity | review | source-owned clear
+ reconcile_v1_projection()
+   semantic duplicate + stale v1 -> repair without new v2 event
+ return 0 | 3
```

```diff
// attention bindings [--json] [filters]
main(['bindings', ...])
+ rows = read_bindings(root, ports)
+   strict parse + interior-address check
+   derive phase / presence / confidence / health
+   mark every duplicate provider/session binding conflicted
+ filter + deterministic order + default 100-row limit
+ emit CliResponse<BindingsResult>             // never resume argv
+ return 0 | 1 | 3
```

```diff
// attention doctor [--json]
main(['doctor', ...])
+ result = doctor(root, ports)
+   strict readers shared with bindings/sweep
+   socket + process + permission + version + UTC-format probes
+   map every finding to DiagnosticCode
+   scope = state_files/socket/processes/permissions/versions
+   unobserved = ['gui_user_vars']
+ render 'CLI probes healthy; GUI publication not checked' | CliResponse
+ return 0 | 1 | 3
```

```diff
// attention sweep [--realm ID] [--apply --operation-id UUID] [--json]
main(['sweep', ...])
+ request = parse SweepRequest                  // preview unless --apply
+ plans = strict_sweep_plans(root, ports)
+   UnixNs20 age selects TTL/30d candidates; invalid or negative age is preserved
+   MonotonicNs20 alone selects absence gap, child order, clear, and retention floor
+   subagent = stored-floor replay | safe ineligible prefix | active-child block | none
+   binding = preserve live/current | absence replay | first absence | exact end
+ if preview -> emit every plan; return without a write
+ for plan under --apply:
+   guard unavailable/invalid/future -> preserve all state
+   with StateFiles.with_launch_lock() -> reread/recompute subagent plan
+     stored floor operation -> delete only currently covered files; never advance floor
+     safe prefix -> replace agents-floor.json -> delete currently covered files -> reconcile v1 .agents
+   active-child block -> retain >500 safely + finding
+   apply binding plan + prune eligible ended history
+ return at most 50 details unless --all-details
+ return 0 | 1 | 3
```

## 3. File-tree diff

```diff
+ protocol/v2.json                         NEW        closed vocabulary + limits             U1
+ bin/attention                            NEW        stable sh command                      U2
+ libexec/attention.py                     NEW        v2 parsers, transitions, CLI           U2/U3/U6
+ shell/wezterm-attention.zsh              NEW        zsh launch claim + prompt republish    U2
+ shell/wezterm-attention.bash             NEW        bash launch claim + prompt republish   U2

~ plugin/init.lua                          MODIFIED   strict reader, view, render, doctor     U1/U2/U4/U5/U6
~ pi/index.ts                              MODIFIED   Pi binding adapter + canonical writer   U3
~ package.json                             MODIFIED   command metadata + full test aliases    U2/U7

+ tests/fixtures/v2/protocol-cases.json    NEW        cross-language protocol authority       U1
+ tests/fixtures/providers/claude.json     NEW        sanitized Claude payload matrix         U3
+ tests/fixtures/providers/codex.json      NEW        sanitized Codex payload matrix          U3
+ tests/fixtures/providers/pi.json         NEW        Pi reason/context matrix                U3
+ tests/wezterm_protocol_smoke.lua         NEW        production JSON boundary                U1
+ tests/attention_cli_test.py              NEW        writer/provider/subagent/sweep races    U2/U3/U6
~ tests/auto_clear_spec.lua                MODIFIED   v1 parity + v2 reader/TTL/render         U1/U2/U4/U5/U6
~ tests/pi_extension.test.ts               MODIFIED   Pi binding + queue/drain contracts      U3/U7

+ docs/mux-setup.md                        NEW        hooks claim/publish/event + doctor        U7
+ docs/record-contract.md                  NEW        validated consumer contract             U7
+ docs/mux-pane-moves.md                   NEW        M6 ghost-tab warning                     U7
~ README.md                                MODIFIED   v2 install/protocol/migration            U7
~ examples/hook.sh                         MODIFIED   delegate; no second serializer           U7
~ examples/hook.ts                         MODIFIED   delegate; no second serializer           U7
~ examples/wezterm.lua                     MODIFIED   v2 options and preserved Alt+B           U4/U7

  docs/plans/2026-09-03-001-feat-attention-v2-plan.md
                                             (context — architecture authority, not modified)

  plugin/init.lua:401-414                    (context — existing pane-id compatibility seam)
  plugin/init.lua:484-539                    (context — existing pure projection)
  plugin/init.lua:650-709                    (context — exact acknowledgement overlay)
  pi/index.ts:78-93,229-306                  (context — current queue and lifecycle)
```

## 4. Seams & enabling points

| Seam | Interface | Enabling point | Test double | Proves |
|---|---|---|---|---|
| **S1** | `StateFiles` | `main()` constructs `RuntimePorts` | temporary state tree with delayed replace hooks | binding-before-pointer; child/clear ordering; locked floor revalidation before delete; future files untouched |
| **S2** | `WriterClock` | `RuntimePorts.clock` | fixed paired `MonotonicNs20`/`UnixNs20` samples | monotonic ordering/fences; immutable Unix write time; retention boundary; duplicate repair does not refresh age |
| **S3** | `PaneLister` | `publish_realm()` | captured `wezterm cli list` rows | realm selection; pane/tty enumeration; partial success |
| **S4** | `TtyWriter` | `RuntimePorts.tty` | byte-capturing tty double | validation precedes open; one bounded OSC write; no stdin bytes |
| **S5** | `ProcessProbe` | `doctor()` and `sweep()` | present/absent/unavailable truth-table double | two real negatives; failure never becomes absence |
| **S6** | Lua poll ports (`read`, `spawn`, `now_unix_ns20`, `call_after`) | private options passed by `M.poll` tests; production bound in `apply_to_config` | fixed UTC value, captured wakeup, and current WezTerm/window/filesystem doubles | one UTC sample per poll; exact TTL boundary; first-ineligible wakeup-only timer; cache-only formatter; realm republish; confidence promotion |
| **S7** | Pi writer runner | module-local `enqueue` production binding | deferred/failed child runner | handler returns before I/O; FIFO invocation; shutdown drain |
| **S8** | Protocol fixture corpus | Python/Lua parser test entrypoints | `protocol-cases.json` itself | both runtimes accept/reject and normalize the same boundary rows |
| **S9** | Provider child lifecycle | Claude/Codex parser entrypoints plus P6 contact harness | captured tool/stop payload pairs and zero-event internal-child run | shared `agent_id`, stop/continue/stop order, and no invented internal/synthetic presence |

`S4` still requires a live disposable mux rehearsal: no double proves that writing the tty output
side is invisible to the running agent. `S6` still requires a real WezTerm load for plugin-root
discovery and `wezterm.json_parse`.

## 5. Build order

```text
U1  v2 protocol authority + Lua reader
    deps: —
    establishes: strict PaneAddress, split clock fields, activity, subagent presence/clear/retention-floor records, and v1 projections
    checkpoint: pause — inspect the rendered protocol fixture and attention state tree before any writer commits to this one-way protocol

U2  launch claims + guarded writer + reattach publication
    deps: U1
    establishes: agents receive distinct launch ids; delayed writers lose; duplicate retries repair v1; a new GUI recovers claims
    checkpoint: pause — show the live reattach recovery, supported-command claim behavior, and resolved plugin root before provider adapters and docs depend on them

U3  Claude, Codex, and Pi provider adapters
    deps: U2
    establishes: hooks event owns ordered child work/stop/continuation, Codex parent clear, and v1 .agents repair
    checkpoint: auto — provider matrix, delayed-writer cases, live Claude/Codex 0→1→0, Python, and Pi suites pass

U4  unified renderer + v1 API projection
    deps: U1, U3
    establishes: eligible active children derive +N; TTL expiry redraws without a write; count-only tint stays nil
    checkpoint: auto — count-only tint, exact Unix TTL, first-ineligible wakeup-only timer, Lua suite, and live formatter load pass

U6  doctor + bindings + sweep
    deps: U2, U3
    establishes: scoped JSON stays honest; sweep retries safely and compacts subagent state floor-first
    checkpoint: auto — JSON, scoped-health, Unix retention age, monotonic absence, sweep retry, and floor-before-delete suites pass

U5  settled titles + churn diagnostics
    deps: U4
    establishes: transient provider titles never become tab names and the formatter remains cache-only
    checkpoint: auto — settling, redraw, and no-write assertions pass

U7  installation + contract docs + migration gate
    deps: U3, U4, U5, U6
    establishes: a disposable POSIX home can install, publish, reattach, diagnose, and render from this repository alone
    checkpoint: auto — full gate and documentation audit pass
```
