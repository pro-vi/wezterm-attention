# Backlog

Lanes awaiting triage. Verdicts: architect | direct | drop | defer | pending.
Checkbox marks a lane consumed (plan written, or direct commit landed).

## Lanes

- [ ] **pi-ui-observations** — observe useful Pi UI state when provider evidence supports it · verdict: defer · origin: 2026-09-09-001-feat-lifecycle-observation-facts-plan · source rows: H13, H14
- [ ] **pi-compaction-failure** — expose Pi's explicit compaction failure/abort evidence when its compatibility cost is justified · verdict: defer · origin: 2026-09-09-001-feat-lifecycle-observation-facts-plan · source row: H21
- [ ] **api-error-badge-policy** — define what a lead API failure should display without mistaking retryable failure for final completion · verdict: defer · origin: 2026-09-09-001-feat-lifecycle-observation-facts-plan · source row: H06
- [ ] **child-approval-badge-policy** — make attributed child approval visible without replacing lead state or inventing a human-response result · verdict: defer · origin: 2026-09-09-001-feat-lifecycle-observation-facts-plan · source row: H18

## pi-ui-observations

Removed from the 18-row lifecycle build. Installed Pi 0.84.4 supplies UI open/close callbacks without a request ID or outcome. An executed ExtensionRunner probe with synthetic UI delivered close before open when an earlier extension delayed its open handler. These callbacks cannot reliably toggle current waiting state.

Reopen when a concrete consumer needs the narrow unpaired facts, or provider identity/order improves enough to support a stronger contract. The decision must justify support above the 0.80.5 baseline. Acceptance must cover callback inversion, overlap, failure close, missing identity, and absence of answer data. No prompt title or answer capture is authorized.

Evidence: the lifecycle plan's historical runtime note and the 2026-09-08 hook map. The temporary probe path is evidence only; recreate a repeatable test if this lane is built.

## pi-compaction-failure

Removed from the 18-row lifecycle build to eliminate newer-Pi callback gates. This is a useful explicit signal, not weak evidence: the callback exposes failure/abort, reason and retry intention. It was added in Pi 0.84.3; the retained API baseline is 0.80.5.

Reopen when a consumer needs this outcome or the supported baseline includes it. Acceptance must preserve failure versus abort, distinguish retry intention from a retry occurrence, and keep compaction outcome separate from settled state. Define older-version behavior if still needed. Do not reserve unused active-schema variants now.

Evidence: installed Pi CHANGELOG 0.84.3 and the hook map's H21. Current compaction attempt/success observations do not imply that a missing success means failure or current compaction.

## api-error-badge-policy

The Claude adapter records StopFailure as an observation without changing the badge. When thinking is the previous eligible activity, that badge can remain until existing expiry or another transition removes it. Pi differs: agent_settled can later write stop after an attempt error. The lifecycle build adds facts and intentionally does not change either badge mapping.

Reopen as a presentation-policy change. Define actionable error versus retryable attempt failure, continuation, precedence with review/notify, and exact acknowledgement behavior before choosing a mapping. Acceptance must cover error→retry→settled, error without a later hook, and focus before recovery. Native outcome coverage remains provider-specific.

Owner: Attention provider policy plus Lua presentation/acknowledgement. Evidence: src/providers.rs supported-event dispatch, pi/index.ts agent_settled, and plugin/reader.lua activity eligibility. The existing badge behavior is not widened by the 18-row facts plan.

## child-approval-badge-policy

Attributed PreToolUse/PermissionRequest currently writes ChildActive presence; it does not write lead notify. A child waiting on approval therefore does not independently make the pane show notify. The lifecycle build preserves child request evidence for consumers while keeping that badge behavior.

Reopen when the default display policy is chosen. Define how child requests combine with lead activity, multiple children, review, parent Stop, resumed child work and focus acknowledgement. A direct switch from ChildActive to lead Activity is not a complete fix: it loses ownership and changes the parent.

Acceptance must keep child/lead identity separate, show the chosen child-attention signal, preserve exact acknowledgement, and avoid claiming that focus answered the approval. Owner: provider policy, child aggregation and Lua presentation. Evidence: src/providers.rs child-first dispatch and src/lifecycle.rs child presence writes.
