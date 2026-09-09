# Attention v2: module specification

**Implemented module map; provider activation remains separate.** The Rust and split-Lua modules below exist in this checkout, `bin/attention` selects the installed Rust executable, and the Python writer is retired. The inspected bootstrap Claude/Codex helpers still write V1 flat markers and the inspected zsh setup still publishes only the V1 pane identity, so an installed Rust shim is not evidence that provider hooks use V2. Automatic launch self-claim remains disabled; its macOS tty premise failed verification. Current certification is in [Rust port certification](2026-09-07-rust-port-certification.md), with later verification in [Hook review triage](triage-2026-09-08/report.md).

## Runtime module map

One Rust executable, one Lua plugin, and their adapters. Rust and Lua communicate through records and published pane identity.

```text
Rust: one attention executable
src/
├── main.rs         commands
├── protocol.rs     record types
├── identity.rs     exact IDs
├── providers.rs    hook adapters
├── lifecycle.rs    transitions
├── records.rs      file I/O
├── wezterm.rs      mux / tty
├── query.rs        read API
├── maintenance.rs  cleanup
└── compat.rs       V1 compatibility projection
```

```text
Lua: one WezTerm plugin
plugin/
├── init.lua        public API
├── protocol.lua    validation
├── reader.lua      read / merge
├── runtime.lua     poll / cache
├── overlays.lua    ack / review
├── legacy.lua      v1 adapter
├── titles.lua      title policy
└── format.lua      rendering
```

```text
Producer and delivery code
shell/
  wezterm-attention.bash
  wezterm-attention.zsh
pi/index.ts            Pi adapter
bin/attention          command path
scripts/install-cli.sh installer
tests/                 verification
```

The implemented split has nine Rust responsibility modules, one compatibility module, and eight Lua responsibility modules. The shell pair and Pi extension can connect provider processes to the core when their host configuration adopts them. Installer and test files support delivery; they are not more runtime services. `Cargo.toml`, `Cargo.lock` and `src/lib.rs` provide package wiring.

> **Your request:** “I want to see all the modules. I want you to $show me all the modules that you will. That v2 will involve. A tech spec if you will.”

A module owns a code responsibility; a feature can use several modules. Safe child cleanup uses lifecycle rules, record storage and maintenance. A separate record file does not require a separate source module.

## Rust module contracts

| Module | Owns | Input → output | Required proof |
|---|---|---|---|
| `protocol.rs` | Typed record/result shapes, shared event definitions, closed variants and limits | Serialized protocol JSON → validated record/result values or a diagnostic | Shared Rust/Lua vectors; reject null, invalid identity fields, unknown record versions and unknown schema types |
| `identity.rs` | Full pane address and launch/binding/child/owner identifiers; one definition per identity | Validated socket/pane/provider facts → exact identifiers | Equal pane numbers in different mux instances stay distinct; old socket lifetime cannot claim a new pane |
| `providers.rs` | Claude, Codex and Pi callback meanings | Provider + event name + bounded payload → normalized event | Each supported/rejected provider fixture; child events never become lead activity; missing launch identity remains `claim_stale` while automatic correlation is unresolved |
| `lifecycle.rs` | Claim rotation, binding selection, lead activity, child active/stopped, prompt-return evidence, exact ending, clear rules and source-owned review | Validated event + environment + locked record reads + observation → accepted transition or rejection | Delayed writers lose; duplicate retries are safe; new child work after stop can reactivate; manual/provider activity share one rule; a prompt return ends the current activity until a strictly newer provider event reactivates it |
| `records.rs` | Record paths, path/interior checks, private permissions, lock scopes, atomic file replacement, inventory and the one commit helper (lock, reread, decide, apply, compatibility output) | Typed addresses/records → validated reads and durable file operations | Missing differs from invalid/unavailable; future records survive; failures and interrupted multi-file operations remain recoverable; `main.rs` never holds a lock |
| `wezterm.rs` | Server pane enumeration, socket/tty evidence, identity publication and `wezterm` executable discovery with a fallback to WezTerm's own executable directory; scoped presence probes where retained | Connection facts + validated claim → pane observations or tty output | Opened tty is checked before writing; no bytes enter agent stdin; republish never mints a launch; enumeration works under the launchd PATH |
| `query.rs` | Validated facts for external readers, current versus historical selection, duplicate-binding findings | Records + relevant observations → bounded binding/state results | Follow the pane's current claim; expose provider without visible activity; never choose silently between conflicting bindings |
| `maintenance.rs` | Explicit doctor/sweep work, safe retention, child-record compaction and immediate revalidation | Inventory + time + operation intent → report, preview or guarded maintenance result | Active children survive; floor precedes covered deletion; replay cannot advance absence twice; unknown files survive |
| `main.rs` | CLI parsing, adapter construction, bounded stdin, response envelopes and exits | Arguments/stdin → dispatched operation and result | Capture ordering time before stdin; hook failures do not block provider work; a successful hook writes nothing to stdout because `SessionStart` stdout enters the agent's context; `PermissionRequest` stdout behavior is verified against the installed provider; JSON is bounded and composable |
| `compat.rs` | V1 activity/child output and retry repair while old consumers remain | Accepted new state → V1 activity/child projection | Only current launch can update flat output; retry repairs missing output without changing the new event |

`identity.rs` owns the meaning of identifiers. `protocol.rs` uses those identifiers in record shapes. Raw provider callback interpretation belongs to `providers.rs`. `lifecycle.rs` owns semantic decisions and the commit closures that request record reads; `records.rs` owns the lock/read/apply mechanics. Query and maintenance use the same validated record reader and identity vocabulary, with selection rules local to their own modules.

Clock capture belongs at the command boundary. Ordering and wall-age types belong in the shared contract. OS clock access is an injected runtime dependency, not a timer service or a new daemon. Independent processes must share the ordering coordinate; a new Rust `Instant` measured from process startup cannot be persisted as that coordinate. The coordinate is `CLOCK_MONOTONIC_RAW`, valid within one boot; socket-incarnation scoping is what keeps cross-process comparison safe.

The shared manifest `protocol/v2.json` is the executed authority. Rust embeds it, Lua reads it, and the independent fixture checker supplies a separate verdict. Tests require every shared protocol row to receive the same Rust and checker verdict and require the embedded manifest to equal the on-disk file byte for byte. No generation framework. Modules must not independently invent schemas.

[Rust library](../../src/lib.rs) · [Rust command](../../src/main.rs) · [current protocol manifest](../../protocol/v2.json)

## Lua module contracts

| Module | Owns | Input → output | Boundary that must hold |
|---|---|---|---|
| `init.lua` | Public API, configuration defaults, module composition, plugin/binary discovery and callback registration | WezTerm config/options → installed handlers and public functions | One documented setup path; retain existing public APIs |
| `protocol.lua` | Lua validation of shared wire/records, address keys, digest checks and time arithmetic | User-var/file JSON → validated values or diagnostics | Cross-language parity; future data never becomes plausible v1 data |
| `reader.lua` | Reading separate records and deriving one named `AttentionView` | Full address + records + previous validated records + current time → view and next expiry | Recover each unavailable record only within its exact scope; fresh successful reads win |
| `runtime.lua` | Full-address cache, per-window polling/focus, redraw, expiry wakeups and reconnect publication requests | GUI callbacks → refreshed cache and allowed GUI effects | No process scan or per-poll subprocess; the reconnect publication is the exception, retried with backoff while any pane in the domain stays unpublished, never latched on spawn, with a failed child visible in the GUI log |
| `overlays.lua` | Exact acknowledgement and user-owned review writes | Focused activity identity or user review action → independent overlay files | Child refresh cannot revive an acknowledgement; user actions cannot erase process activity |
| `legacy.lua` | Tolerant v1 reading and legacy ack/review/expiry behavior | Existing flat files → legacy attention behavior | Preserve old inputs and six-value API behavior where identity is unambiguous |
| `titles.lua` | Server-name/directory/settled-title choice and title sampling | Pane metadata sampled during polling → cached title facts | Two equal samples for the settled fallback; never write a server or pane title |
| `format.lua` | Tab aggregation, priority, glyph/count/color, formatter context and retained display options | Cached views/title facts → formatted title | No filesystem, clock sampling or process work; count-only state stays neutral |

`runtime.lua` owns the cache. `reader.lua` receives previous records and returns a new view; it does not keep a second global cache. `format.lua` receives values only. `titles.lua` samples during polling, so title formatting never fetches live data.

Keep existing title and expiry behavior visible in this map. Whether to remove a specific title option, reduce timestamp precision or remove an extra expiry wakeup remains a separate decision. Those changes do not require deleting the module that owns them.

[Current Lua implementation: init.lua](../../plugin/init.lua) · [existing Lua regression cases](../../tests/auto_clear_spec.lua)

## Producer adapters and consumers

| Component | What attention owns | What stays outside |
|---|---|---|
| Bash and zsh integration | Establish a new launch identity before the agent where the shell can; export it to hooks; republish at prompts, which is also the prompt-return evidence | General shell parsing or a terminal/session launcher beyond the supported integration |
| Pi extension | Translate Pi events, serialize requests through its queue, handle reload/shutdown and v1 fallback only when no checkout root exists | Provider conversation execution and Pi's internal agent loop |
| Claude/Codex registration | Documented callback commands and payload handling | Actual user settings and bootstrap-owned hook registration; inspected 2026-09-08, bootstrap's hook helpers still write V1 markers directly, while the live plugin directory is a symlink into this checkout |
| Title bar | Cached attention display and formatter extension point | A new terminal UI application |
| Relay | Exact cached pane/provider/binding facts | Reply capture, reply records, selection, routing and pasting |
| Bridge dashboard | Validated binding/state output with full pane identity | Slot, repository, PR and CI joins; cockpit presentation |
| Recovery tooling | Validated provider/session facts and retained binding candidates | Choosing and executing resume commands; restoring windows or pane placement |

Ordinary launch convenience is a retained goal. The inspected live zsh configuration publishes `WEZTERM_PANE` at each prompt but does not run the V2 claim command. The implemented Bash adapter claims supported commands automatically; the implemented zsh adapter needs an explicit call and is not evidence of installed shell activation.

**Current launch status (2026-09-08):** automatic `SessionStart` self-claim is disabled. On macOS, opening `/dev/tty` from the tested hook-like child does not identify the pane's real pty, and current Claude documentation says command hooks have no controlling terminal. Explicit shell claims remain supported. A replacement automatic-correlation design must prove the pane and process generation across every later callback; it is deferred in [Hook review follow-ups](triage-2026-09-08/followups.md#automatic-launch-correlation).

Pi remains a Node-hosted TypeScript adapter that calls the Rust binary. Rust replaces the Python production writer; it does not replace Pi's host API or WezTerm's Lua API. The adapter falls back to V1 only when no checkout root is configured. A configured missing, nonzero, or diagnostic writer is reported and never downgraded. A reload drains queued writes without sending the provider-defined no-op end request.

[Bash](../../shell/wezterm-attention.bash) · [zsh](../../shell/wezterm-attention.zsh) · [Pi](../../pi/index.ts)

## Shared facts and public interfaces

| Fact | Meaning and owner | Important distinction |
|---|---|---|
| Pane address | Mux connection + socket lifetime + server pane ID; identity module | A GUI-local pane number is not storage identity |
| Launch | One top-level agent process; lifecycle claim operation | Starting another agent needs a new launch identity |
| Binding | Actual provider/session associated with that launch | A notification is not a substitute for session identity |
| Activity | Latest accepted lead activity and its event ID | Reported Stop is not proof that a task succeeded |
| Child presence | Ordered active/stopped evidence per child | Parent and child work can have different lifetimes |
| Acknowledgement | GUI dismissal of one exact activity | It does not end the session or clear child work |
| Review | An explicit request owned by a user or source | Source clear does not remove another owner's request |
| Availability and health | What this reader can prove from its observations | Missing, unavailable, invalid and ended remain distinct |

Callbacks miss transitions. An Escape interrupt and a denied permission emit no hook, and herdr replaced this same Claude hook set with screen reading for that reason. This design accepts bounded drift instead and names what re-anchors a record: `thinking` expires by TTL (30 minutes by default), `notify` and `stop` clear on focus acknowledgement, a prompt return ends the launch's activity, and the explicit sweep is the last resort. Every reference pairs hooks with a continuously observable signal; the prompt return is ours.

Current public contract:

| Surface | Call or command | Promise |
|---|---|---|
| Provider callbacks | `attention hooks event PROVIDER EVENT` | Bounded original JSON on stdin; normalized guarded mutation; default failure does not block the agent; nothing on stdout when accepted |
| Shell launch | `attention hooks claim` | Commit a claim and print its launch ID for parent-shell export; provider callbacks without a valid launch remain stale |
| Prompt/reattach | `attention hooks publish` | Republish existing identity and record that the prompt returned after the current launch; never create a replacement launch |
| Human/tool activity | `attention mark …` | Use the same transition rules as provider callers; source-owned review remains separate |
| External readers | `attention bindings --json` | Validated full-address binding identity and liveness facts; no resume command and no complete activity/review/subagent view |
| Operator | `attention doctor`, `attention sweep` | Diagnosis and preview-first, explicitly applied maintenance |
| Existing Lua consumers | `get_attention`, `pane_marker_id`, `poll`, `remove_marker`, `wrap_title_formatter`, `apply_to_config`, `doctor` | Preserve documented legacy API behavior; do not resolve ambiguous new addresses by guessing |
| New Lua consumers | `get_attention_view(pane)` | Cached pane view, including provider/binding when no alert is drawn; no I/O in the accessor |

Provider lifecycle state and GUI visibility must remain separately queryable. A consumer must not interpret “no icon” as “no agent.” New consumers must use full pane identity. The scalar v1 API cannot represent two different mux panes that share a number.

There is **no committed `changes`/event-log API** in this module specification. Reading retained session records is different from replaying missed transitions. The earlier 64-event proposal is not part of this baseline.

## Record layout and write ownership

This is the implemented separate-record structure. The pane subtree is shown below; mux connection and socket-lifetime manifests live above it. Placeholder IDs are shortened. `protocol/v2.json` owns the selected wire and record schema versions.

```text
pane/
├── claim.json
├── reviews/<owner>.json
├── absence-probe.json
└── launches/<launch>/
    ├── current-binding.json
    ├── activity.json
    ├── ack.json
    └── bindings/<binding>/
        ├── binding.json
        ├── activity.json
        ├── activity-clear.json
        ├── end.json
        ├── ack.json
        ├── agents-clear.json
        ├── agents-floor.json
        └── agents/<child>.json
```

Launch-level activity/ack records cover the period before a current binding. Binding-level activity is selected after binding. A review request remains pane-owned. Files shown are record kinds, not separate source modules. Lock files are omitted from this drawing.

| Writer | Allowed records | Never inferred from its action |
|---|---|---|
| Rust lifecycle | Claims, binding selection, activity, child records, provider ending/clear and source review | Provider completion does not acknowledge what the user saw |
| Rust maintenance | Absence observations, approved inferred ending, retention floor and covered deletions | Missing/failed probe does not prove absence; cleanup is not conversation compression |
| Lua GUI actions | Exact ack, user review and explicit review clear | Acknowledgement does not mutate provider truth |
| Optional compatibility output | Derived v1 activity/child files | An old flat record does not become new-protocol authority |

Individual replacements are atomic; a sequence of separate-file replacements is not one transaction. Binding-before-pointer publication, Stop/clear retry repair and floor-before-delete ordering remain explicit contracts. A single record I/O helper cannot remove those obligations.

## Runtime flows

Each numbered list is an execution sequence. The module lists above are responsibilities, not a build order.

### Provider work or child completion

1. An activated shell integration establishes and exports launch identity before the agent starts. The inspected live zsh configuration has not adopted this step.
2. A provider hook or Pi adapter calls `bin/attention`.
3. `main.rs` captures ordering time before reading bounded input; `providers.rs` normalizes the callback.
4. `records.rs` runs the one commit helper: take the scope lock, reread current records, call `lifecycle.rs` to decide, apply the ordered file replacements and any compatibility output. `main.rs` never holds a lock. The Python version repeats this choreography at fourteen call sites; herdr's arbitration grew to 5,886 lines the same way.
5. `runtime.lua` calls `reader.lua` on a poll. The reader validates records and derives activity, child eligibility and health.
6. `format.lua` renders cached values. Relay can read the same cached pane identity through the public accessor.

### Focus and acknowledgement

1. `runtime.lua` verifies that this GUI is focused and selects its current pane.
2. `overlays.lua` rereads the current scoped activity and writes an acknowledgement for that exact event.
3. The affected view is refreshed and redrawn. Provider identity and child state remain available.

### GUI reconnect

1. A poll finds a mux pane without published identity.
2. The Lua runtime spawns a background `hooks publish` for that mux connection with WezTerm's executable directory on the child's PATH, and repeats it with backoff on later polls while any pane in the domain stays unpublished.
3. Rust validates server pane and opened tty evidence, then writes identity to terminal output, not agent input. The mux server forwards a user variable only to panes already attached, so one publication cannot cover a GUI that is still attaching.
4. A later poll resolves the restored identity and reads retained records. No new provider event is required for this recovery. Recovered views stay `unconfirmed` until a newer event confirms them.

Both failures of the once-only request were measured on 2026-09-05: it fired before the GUI had attached most panes, and under a Dock-launched GUI the child could not find `wezterm` on the launchd PATH.

### Explicit maintenance

1. `main.rs` dispatches a requested doctor/sweep operation to query/maintenance code.
2. Queries read validated records and the required current observations. Preview does not write.
3. Apply reacquires the relevant locks and rechecks the target immediately before mutation.
4. Cleanup retains the ordering evidence needed to reject delayed callbacks before deleting covered records. Any retained inferred-ending policy requires its own successful absence observations.

Nothing in this module map installs a periodic process scan. The current scan is reached by explicit commands. An external scheduler is a separate owner.

## Where the eight reviewed features belong

| Reviewed item | Module ownership | Current review position |
|---|---|---|
| Separate binding/child files | Rust records/protocol; Lua protocol/reader | Retain separation and scoped recovery |
| Process-based ending inference | WezTerm observations, query and maintenance | Prompt-return evidence from `hooks publish` becomes the ordinary ending signal; the two-observation sweep stays as the explicit last resort |
| Retained binding history | Records, query and maintenance | Preserve existing records where useful; measure cost before richer history |
| Child-record compaction | Lifecycle eligibility rules, maintenance and record I/O | Keep safe growth handling and delayed-writer protection |
| Precision and expiry wakeups | Shared protocol/time rules; Lua reader/runtime | Precision and timer removal are separate open choices |
| Title options | Lua titles and format | Keep ownership clear; review individual options rather than deleting the capability |
| V1 output mirroring | Rust compat and migration tooling | Required while inspected provider helpers and bridge consumers remain V1; retire only after those real consumers migrate |
| Automatic launch claiming | Shell integration and Rust lifecycle claim | Automatic provider self-claim remains disabled; explicit claims work, and automatic correlation is deferred |

## Delivery, verification and open decisions

`bin/attention` is the stable command path. It locates the installed Rust binary and never compiles during a hook. `scripts/install-cli.sh` performs the explicit build/install step; packaging remains one Cargo package.

Tests are part of the delivery specification, not more product modules:

- Rust identity, parsing, transition, lock, retry, query and retention cases.
- Shared Rust/Lua record and expiry fixtures; keep an independent oracle where useful.
- Lua cache, focus, review, rendering and legacy regression cases.
- Bash/zsh inheritance and command-behavior checks; Pi Node dispatch and queue checks.
- Installed-WezTerm module/JSON/formatter contact, disposable reconnect and zero-stdin proofs.
- Consumer contract exercises for Relay, bridge and recovery; compatibility/upgrade rehearsal before live activation.

The implementation is built. These product or activation decisions remain open and do not hide any module from this map:

| Decision | What must be settled |
|---|---|
| Automatic launch correlation | A provider-supported pane and process-generation signal for callbacks without an explicit shell claim |
| Acknowledgement of `notify` | Whether focus clears a pending prompt, as shipped, or only a newer provider event clears it; herdr never acknowledges a blocker |
| Lua digests | Whether the Lua reader keeps recomputing SHA-256 for every child record on every poll, or trusts filenames that Rust verified at write; measure before deciding |
| Provider activation | When bootstrap-owned shell and provider hooks move from V1 helpers to the Rust command; installed shim selection alone does not perform this migration |
| Presentation | Which optional title features remain; do not let this change lifecycle facts |

No event archive, workflow engine, reply store, topology manager, daemon or native Lua library is implied. If a later consumer needs one, it requires a separate justification. Keeping the current implementation as a behavioral reference does not make its duplicated helpers mandatory.

The [earlier Rust implementation plan](../plans/2026-09-05-001-feat-attention-rust-lifecycle-plan.md) is **superseded** by the [2026-09-06 plan](../plans/2026-09-06-001-feat-attention-rust-lifecycle-plan.md), which was written from this spec. Neither approves the earlier single-pane snapshot, capacity-refusal replacement for cleanup, manual Bash cut or 64-event catch-up system.

This specification began from the Python/Lua/Pi/shell implementation and the 2026-09-06 review. The Rust and split-Lua module paths are now implemented; [Rust port certification](2026-09-07-rust-port-certification.md) records activation and test evidence. The source remains uncommitted in this checkout, the live plugin is linked here, and the provider hook/shell activation boundary remains separate as described above. Updated 2026-09-08 to remove retired Python anchors and distinguish implemented code from inspected live registration.
