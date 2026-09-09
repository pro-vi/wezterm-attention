# Attention v2: lifecycle hook map

Rows are lifecycle observations. Provider columns show exact hook names and current handling. The last column proposes support to add, retain, or leave out.

**Now = this checkout under its existing guards, not live activation. Next = proposed, not implemented.** No provider hooks were executed for this map.

Claude Code **2.1.266** · Codex **0.153.4** · Pi **0.84.4**. Complete native catalogs: **33 / 12 / 36** names respectively, plus Claude's 12 notification types.

<details>
<summary>Reading the cells and evidence limits</summary>

“Inert” is a deliberate ignore. “No hook” means no dedicated native signal, not proof that the event never happens. “Next” is proposed work; “Later” needs a consumer; “Keep” preserves existing behavior; “Skip” stays outside attention; “Gap” needs better evidence.

Availability comes from current documentation and installed Pi types. Repeated names serve different observations; they are not additional hooks. Comparable rows do not imply identical timing. The version notes below distinguish installed releases from older source clones.

</details>

## Session identity and navigation

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| Session established or resumed | `SessionStart`<br>**Now:** binding for `startup`, `resume`, `clear`, `compact`. | `SessionStart`<br>**Now:** same binding mapping. | `session_start`<br>**Now:** binding; native reason becomes writer `start_source`. | **Keep:** session/binding facts. This does not by itself prove a new OS execution. A valid launch claim is still a prerequisite. |
| New session, resume, or fork is being attempted | No separate before-switch/fork hook. | No separate before-switch/fork hook. | `session_before_switch`, `session_before_fork`<br>**Now:** not subscribed. | **Later:** record navigation intent only if needed. An attempt can be cancelled; do not rebind on it. |
| Session replacement completes | `SessionEnd` + `SessionStart` for supported reasons.<br>**Now:** end/bind separately. | `SessionStart` identifies resume/clear; `SessionEnd` has reason `other`.<br>**Now:** handled. | `session_shutdown` + `session_start` with new/resume/fork reasons.<br>**Now:** end/bind. | **Keep:** preserve supplied replacement reasons. A forked conversation is not automatically a child agent. |
| Session-tree navigation requested/completed | No dedicated hook. | No dedicated hook. | `session_before_tree`, `session_tree`<br>**Now:** not subscribed. | **Later:** retain old/new branch position for a consumer. Do not call this a new launch or child spawn. |
| Session or extension runtime ends | `SessionEnd`<br>**Now:** end; accepted reason list is checked. | `SessionEnd`<br>**Now:** end. Main thread only; reason `other`. | `session_shutdown`<br>**Now:** end except reload; reload only drains writes. | **Keep:** reported end, distinct from turn completion. Hard process death need not emit a hook. |
| Session display name changes | No dedicated hook. | No dedicated native hook. | `session_info_changed`<br>**Now:** not subscribed. | **Later:** refresh metadata without creating activity or changing the terminal title. |

## Prompt, tools, and run outcomes

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| User submits a prompt | `UserPromptSubmit`<br>**Now:** ignored. | `UserPromptSubmit`<br>**Now:** ignored. | `input`<br>**Now:** not subscribed. Extension commands can bypass it. | **Next:** capture submission, supplied turn IDs, and input-source labels where available. Submission is not proof that the model started; another handler can intercept it. Do not retain prompt text by default. |
| Typed command expands into a prompt | `UserPromptExpansion`<br>**Now:** ignored. | No dedicated hook. | No exact equivalent; `input` precedes expansion. | **Skip:** no default lifecycle write for command expansion. |
| Agent-run preparation | No dedicated run-preparation hook. | No dedicated run-preparation hook. | `before_agent_start`<br>**Now:** not subscribed. | **Skip:** keep preparatory prompt/context contents out of attention. Actual Pi activity already has `agent_start`. |
| Agent run starts | No dedicated root run-start hook; prompt/tool signals are narrower observations. | No dedicated root run-start hook. | `agent_start`<br>**Now:** thinking. | **Keep:** Pi run activity. Do not invent matching native hooks for the other providers. |
| Tool preflight begins | `PreToolUse`<br>**Now:** thinking; attributed child updates child presence instead. | `PreToolUse`<br>**Now:** thinking; `request_user_input` and `request_permissions` → notify. Attributed child calls update child presence instead. | `tool_execution_start`, `tool_call`<br>**Now:** the first writes thinking; the second is not subscribed. | **Next:** retain tool-call identity/name. Preflight can precede a block; it is not proof the tool body ran. Observe, do not return policy decisions. |
| Tool produces progress | No dedicated progress hook. | No dedicated progress hook. | `tool_execution_update`<br>**Now:** not subscribed. | **Later:** bounded progress metadata if needed. No streaming output collection by default. |
| Tool result can be transformed by extensions | No separate result-middleware event in this map. | No separate result-middleware event. | `tool_result`<br>**Now:** not subscribed; later handlers can change its result. | **Skip:** prefer finalized `tool_execution_end` for attention's outcome observation. |
| Tool produces an execution result | `PostToolUse`<br>**Now:** ignored. Successful tool path. | `PostToolUse`<br>**Now:** ignored. Current docs also cover nonzero shell exits; post hooks can replace the model-visible result. | `tool_execution_end`<br>**Now:** not subscribed; supplies `toolCallId` and `isError` after result middleware. | **Next:** preserve exact tool identity and the observed outcome. Do not assume every provider's post hook sees immutable final output. A finished tool is not a finished agent run. |
| Tool failure is observed | `PostToolUseFailure`<br>**Now:** ignored. Not permission denial or every cancellation. | No separate failure hook. `PostToolUse` has narrower tool-specific coverage. | `tool_execution_end` with `isError`.<br>**Now:** not subscribed. | **Next:** record the observed failure/error flag. Do not equate it with denied, cancelled, or aborted without stronger data. |
| Model/tool cycle starts or finishes | `PostToolBatch` after a complete batch.<br>**Now:** ignored. | No dedicated batch hook. | `turn_start`, `turn_end` for one model response plus tools.<br>**Now:** not subscribed. | **Later:** cycle progress, not session completion. Claude's batch response is serialized tool-result content, not the structured `PostToolUse` output. |
| Message starts or finishes | No universal native message-start/end hook. | No native message-start/end hook. | `message_start`, `message_end` for user, assistant, and tool-result roles.<br>**Now:** not subscribed. | **Later:** use role/outcome metadata only. The outcome rows below justify a narrow assistant `message_end` subscription. |
| Assistant text streams or is displayed | `MessageDisplay` for batches of display lines.<br>**Now:** ignored. | No native message-stream hook. | `message_update`<br>**Now:** not subscribed. | **Skip:** high-volume text is not needed for the proposed state model. Display is not completion. |
| Low-level agent run ends | No separately named low-level run hook. | No separately named low-level run hook. | `agent_end`<br>**Now:** not subscribed; Rust deliberately treats it as inert. | **Keep:** do not turn this into settled. Pi can retry, compact, or run queued follow-ups afterward. |
| Lead response finishes / agent settles | `Stop`<br>**Now:** stop; does not clear children. Other hooks can request continuation. | `Stop`<br>**Now:** lead stop; the reader ignores earlier child activity. Other hooks can continue the turn. | `agent_settled`<br>**Now:** stop after automatic continuation finishes. | **Next:** retain completion versus outcome and continuation context. Preserve existing badge policy. Stop/settled is not proof of successful work or process exit. |
| Agent attempt fails | `StopFailure` for a turn-ending API error.<br>**Now:** ignored. | No native `StopFailure`. | `message_end` with assistant `stopReason=error`; later `agent_settled`.<br>**Now:** message outcome ignored. | **Next:** distinguish observed attempt failure from final settled state. Pi can retry; errors before entering a run have different coverage. |
| Active attempt is interrupted or aborted | No general interrupt hook. `Stop` excludes user interrupts. | `Interrupt`<br>**Now:** ignored. User interrupt of an active root turn only. | `message_end` can report assistant `stopReason=aborted`.<br>**Now:** ignored. The abort does not identify who caused it. | **Next:** consume Codex interruption and Pi's observed abort outcome. **Gap:** Pi retry cancellation need not emit an aborted message; Claude has no equivalent universal signal here. |

## Waiting, responses, and notifications

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| Tool approval is requested | `PermissionRequest`<br>**Now:** notify for lead; child-active evidence for a child. No `tool_use_id`. | `PermissionRequest`<br>**Now:** same mapping. Has `turn_id`, but no tool/request ID in stdin. | No built-in permission-request event. Extensions can implement their own gates. | **Next:** preserve “approval requested” separately from seen. A hook can resolve a request before any visible prompt; do not claim the human is necessarily waiting. |
| Permission prompt remains outstanding long enough to notify | `Notification` with `permission_prompt`.<br>**Now:** notify; subtype is not retained as a separate fact. | No native `Notification`. | No permission-specific native event; see generic UI spans below. | **Next:** preserve this notification's kind and timing limits. It is delayed and can also represent sandbox network approval. |
| Auto-mode policy denies a tool | `PermissionDenied`<br>**Now:** ignored. Auto mode only. | No dedicated result hook. | No dedicated native denial hook. | **Next:** record Claude's scoped automatic-denial outcome and tool ID. Do not extend it to manual denial. |
| Human grants, denies, or cancels ordinary tool approval | No universal dedicated result hook. | No dedicated result hook. | No core approval protocol; outcome belongs to the gate extension. | **Gap:** retain unknown when no matched result is exposed. A tool result can establish a narrower later fact; do not infer a human decision from focus or silence. |
| Agent asks a structured question | `PreToolUse` with `AskUserQuestion`.<br>**Now:** thinking, not a distinct question fact. | `PreToolUse` with `request_user_input`.<br>**Now:** notify. `request_user_input_async` is model-dependent; current mapping does not special-case it. | No dedicated question event; an addon may show an extension UI prompt. | **Next:** preserve question kind and tool identity. Verify the async Codex hook path before claiming coverage. No question text capture by default. |
| A structured-question tool returns | `PostToolUse` for the question, when emitted.<br>**Now:** ignored. | `PostToolUse` for the question, when successful/emitted.<br>**Now:** ignored. | `tool_execution_end` for a registered question tool; meaning depends on the addon result.<br>**Now:** not subscribed. UI spans alone expose no answer. | **Next:** use matched tool result evidence where supplied. Verify installed question/cancel paths; this is not a universal answer receipt. |
| Extension UI waiting span opens | No equivalent generic extension-UI span hook. | No equivalent generic extension-UI span hook. | `ui_prompt_start`<br>**Now:** not subscribed. Kind/title, no request ID. | **Next:** expose an observed outer UI waiting span, separate from seen. It can be a selector/editor/custom UI, not necessarily an agent permission. |
| Extension UI waiting span closes | No equivalent generic span hook. | No equivalent generic span hook. | `ui_prompt_end`<br>**Now:** not subscribed. No answer/cancel/error result. | **Next:** record only that the UI span ended. Overlapping dialogs share one outer pair; end also fires on failure. Print mode has no such dialog events; unsupported RPC custom UI can finish immediately. |
| MCP server requests user input | `Elicitation`<br>**Now:** ignored. Optional `elicitation_id`. | No dedicated native elicitation hook. | No dedicated native elicitation hook. | **Next:** preserve request kind and supplied correlation. Observe without supplying or changing the answer. |
| MCP elicitation response is selected | `ElicitationResult`<br>**Now:** ignored. Action plus optional ID; before send. | No dedicated hook. | No dedicated native event. | **Next:** record the selected action, not a confirmed remote delivery. Missing IDs limit matching. Never modify the response. |
| MCP form or URL prompt produces a notice | `Notification` with `elicitation_dialog` or `elicitation_url_dialog`.<br>**Now:** form → notify; URL subtype rejected. | No native notification hook. | No corresponding native notice. | **Next:** retain notice kinds. Prefer structured elicitation events for request identity. |
| MCP response sent / URL flow completed notice | `Notification` with `elicitation_response` or `elicitation_complete`.<br>**Now:** rejected subtypes. | No native notification hook. | No corresponding native notice. | **Later:** expose the reported fact without inventing request correlation. Notification input lacks a structured request ID in the documented common shape. |
| Background session needs input or completes | `Notification` with `agent_needs_input` or `agent_completed`.<br>**Now:** rejected subtypes. | No native notification hook. | No core equivalent. | **Later:** only with correct target-session attribution. These notices are not interchangeable with current-pane subagent events. |
| Idle reminder / authentication success | `Notification` with `idle_prompt` or `auth_success`.<br>**Now:** idle inert; auth → notify. | No native notification hook. | No universal notification-emitted event. | **Keep:** idle should not replace lifecycle truth. **Later:** distinguish informational auth notices from actionable requests. |
| Usage-limit wait resumes, needs attention, or ends | `Notification` with `quota_auto_resume_fired`, `quota_auto_resume_stale`, `quota_auto_resume_disabled`.<br>**Now:** rejected subtypes. | No equivalent native hook. | No native extension retry-wait event; SDK events are a different surface. | **Later:** preserve those reported wait transitions. They do not provide a universal wait-start or cancellation signal. |

## Children, tasks, and teams

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| Child agent is created | `SubagentStart`<br>**Now:** deliberately inert. | `SubagentStart`<br>**Now:** deliberately inert. | No built-in subagent event. Addons own spawning. | **Later:** expose creation separately if needed. Preserve the distinction between created and observed active; do not automatically change the count rule. |
| Child work is observed active | `PreToolUse` or `PermissionRequest` with `agent_id`.<br>**Now:** child-active presence; no lead write. | Same hooks with `agent_id`.<br>**Now:** same child mapping. | No native child identity contract. | **Next:** retain child ownership and request/tool metadata. Pi needs an explicit addon contract, not a guessed tool name. |
| Child finishes responding | `SubagentStop`<br>**Now:** child-stopped record; requires `agent_id`. | `SubagentStop`<br>**Now:** child-stopped record; requires `agent_id`. | No native child-stop event. | **Keep:** child response-stop remains separate from parent state and process exit. Continuation can follow a stop hook. |
| Task is being created | `TaskCreated`<br>**Now:** ignored. Task tools must be available. | No dedicated task hook. | No core task hook. | **Later:** task-specific facts only for a task-aware consumer. Not pane activity by default. |
| Task is being marked complete | `TaskCompleted`<br>**Now:** ignored. A hook can prevent completion. | No dedicated task hook. | No core task hook. | **Later:** preserve the reported attempt and task ID; not proof of successful task completion or an idle pane. |
| Team member is about to become idle | `TeammateIdle`<br>**Now:** ignored. | No native teammate-idle hook. | No core team hook. | **Later:** team-member state if a consumer needs it; not root-session end. |

## Compaction and metadata

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| Context compaction begins | `PreCompact`<br>**Now:** ignored. | `PreCompact`<br>**Now:** ignored. | `session_before_compact`<br>**Now:** not subscribed. | **Next:** record compaction attempt/in-progress and supplied reason. Before hooks can cancel; preserve that distinction. This is provider context compaction, not attention retention. |
| Context compaction succeeds | `PostCompact`<br>**Now:** ignored; compact `SessionStart` can update binding. | `PostCompact`<br>**Now:** ignored; compact `SessionStart` updates binding. | `session_compact`<br>**Now:** not subscribed. | **Next:** close compaction successfully without implying that the agent is settled. |
| Context compaction fails or aborts | No dedicated compaction-failure hook. | No dedicated compaction-failure hook. | `session_compact_failed`<br>**Now:** not subscribed; has `aborted`, reason, `willRetry`. | **Next:** preserve Pi failure/abort and retry intention. **Gap:** do not invent equivalent provider outcomes where no signal is exposed. |
| Model switch is requested | `PreModelSwitch`<br>**Now:** ignored. | No dedicated model-switch hook. | No before-model-switch event. | **Skip:** do not change model policy. Prefer actual metadata change below. |
| Selected model changes | `PostModelSwitch`<br>**Now:** ignored. | No dedicated hook; model snapshots accompany some other hooks. | `model_select`<br>**Now:** not subscribed. | **Later:** update model metadata without a new badge. Includes provider-supported restore/fallback cases, not proof of a new launch. |
| Thinking level changes | No dedicated hook in this catalog. | No dedicated hook in this catalog. | `thinking_level_select`<br>**Now:** not subscribed. | **Later:** metadata only if a consumer uses it. |
| Working directory changes | `CwdChanged`<br>**Now:** ignored. | No dedicated hook; `cwd` is included in other events. | No dedicated cwd-change event. | **Later:** refresh safe path metadata; do not infer a session replacement. |

## Supporting hooks with no default attention write

| Lifecycle observation | Claude Code hooks + now | Codex hooks + now | Pi events + now | Proposed attention support |
|---|---|---|---|---|
| Explicit environment setup runs | `Setup`<br>**Now:** ignored. Explicit init/maintenance modes, not ordinary startup. | No equivalent hook. | No equivalent setup hook. | **Skip:** environment setup is not lifecycle authority. |
| Project trust is about to be decided | No standalone project-trust hook in this catalog. | No standalone project-trust hook. | `project_trust`<br>**Now:** not subscribed; global/CLI extensions only. | **Skip:** do not decide trust or claim this is the final trust result. |
| Extension resources are discovered | No exact equivalent. | No exact equivalent. | `resources_discover`<br>**Now:** not subscribed. | **Skip:** resource discovery has no default activity meaning. |
| Instruction files load | `InstructionsLoaded`<br>**Now:** ignored. | No equivalent native hook. | No exact instruction-loaded event. | **Skip:** no instruction-content collection. |
| Configuration changes | `ConfigChange`<br>**Now:** ignored. | No equivalent native hook. | No exact config-change event. | **Skip:** no default notification or configuration policy. |
| Extra working directory is added | `DirectoryAdded`<br>**Now:** ignored. | No equivalent hook. | No equivalent hook. | **Skip:** workspace setup is not agent work state. |
| Watched file changes | `FileChanged`<br>**Now:** ignored. | No equivalent native hook. | No equivalent native event. | **Skip:** no broad file watch added for attention. |
| Worktree creation/removal runs | `WorktreeCreate`, `WorktreeRemove`<br>**Now:** ignored. | No equivalent native hooks. | No equivalent native events. | **Skip:** registering `WorktreeCreate` replaces the provider's creation behavior. It is not a passive lifecycle observer. |
| Model context is prepared | No equivalent native hook. | No equivalent native hook. | `context`<br>**Now:** not subscribed. | **Skip:** do not read or copy conversation context into attention. |
| Provider HTTP headers are prepared | No equivalent native hook. | No equivalent native hook. | `before_provider_headers`<br>**Now:** not subscribed. | **Skip:** no header collection or mutation. |
| Provider request payload is ready | No equivalent native hook. | No equivalent native hook. | `before_provider_request`<br>**Now:** not subscribed. | **Skip:** no payload collection or mutation. |
| Provider HTTP response arrives | No equivalent native hook. | No equivalent native hook. | `after_provider_response`<br>**Now:** not subscribed; before stream consumption. | **Later:** allowlisted status metadata if needed. This is not a complete model response. |
| User runs a shell command outside the agent loop | No dedicated native hook. | No dedicated native hook. | `user_bash`<br>**Now:** not subscribed; no paired native end event. | **Skip:** keep manual shell activity distinct from agent activity. |

## What to implement first

The proposal keeps attention observational and keeps the existing click-to-dismiss policy. The missing granularity belongs in the normalized facts and the reader contract, not in a permission-deciding hook.

1. **Preserve evidence identity:** provider/session/child, supplied turn/tool/request IDs, event kind, observation source, and time. Missing native IDs stay missing. Do not confuse an attention-generated observation ID with a provider request ID.
2. **Add the available request and outcome observations:** question preflight, structured MCP elicitation, Pi UI span open/close, tool outcomes, Claude turn failure, and Codex interruption. Preserve seen independently. A close, submitted response, successful tool result, and confirmed answer are different observations.
3. **Add compaction phases and selective metadata:** keep them separate from settled state. Add lower-priority metadata only when a consumer needs it.

No-wrapper launch identity remains a prerequisite investigation. Adding hook subscriptions does not make the current launch claim trustworthy. Native `SessionStart` is not proof that automatic self-claim works on every supported mux/provider path.

## Version and surface boundaries

- **Claude:** the current public catalog is checked against the installed version number, not a live execution of every hook. The inspected version is newer than the earlier review's 2.1.263. `PreModelSwitch` and `PostModelSwitch` are documented from 2.1.251. Notification subtype availability also has version conditions in the source.
- **Codex:** the installed CLI is 0.153.4. The local source clone `cd8dc1e` predates it and has 11 hooks. Official release notes add `Interrupt` in 0.150.0, which resolves the apparent missing hook. The 0.153.0 async-question tool is model-dependent; its hook emission was not exercised here. Hook trust/enablement still applies.
- **Pi:** the installed 0.84.4 `ExtensionAPI.on` overloads define all 36 names in this map. The local source clone declares 0.84.1 and lacks the UI-prompt types, so it is not the authority for those events. Attention's peer range starts at 0.80.5; adding UI prompt support requires an explicit older-version capability policy.
- **Not native hooks:** Codex's legacy `notify` command gets argv JSON such as `agent-turn-complete`. It is not `Notification` or the native stdin hook format. Pi's `wezterm-attention:mark` is our cooperative bus topic; the Rust request name `bus` is not a native event. That bus currently preserves state and optional label, not identified request outcomes or child lifecycle.
- **Other APIs are separate:** Pi `ctx.ui.onTerminalInput` observes raw interactive input; there is no native `terminal_input` event. SDK `AgentSession.subscribe` exposes additional retry/queue events, but those are not `pi.on` hooks. Codex app-server and Claude Agent SDK request/response callbacks are outside this native-hook grid. Importing them would change attention's role.
- **Scope of proof:** this is documentation, installed-type, and source inspection. It is not a registration audit, provider-contact test, full build gate, or implementation approval. In particular, no real prompt, tool argument, header, answer, or transcript was collected.

## Sources and reproducibility

Provider definitions: [Claude Code hooks](https://code.claude.com/docs/en/hooks), [Codex hooks](https://learn.chatgpt.com/docs/hooks), [Codex release notes](https://learn.chatgpt.com/docs/changelog), [installed Pi extension documentation](/Users/provi/.nvm/versions/node/v24.14.0/lib/node_modules/@earendil-works/pi-coding-agent/docs/extensions.md), [installed Pi event types](/Users/provi/.nvm/versions/node/v24.14.0/lib/node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/types.d.ts:907).

Current attention behavior: [provider normalization](/Users/provi/Development/_projs/wezterm-attention/src/providers.rs:287), [Pi native subscriptions](/Users/provi/Development/_projs/wezterm-attention/pi/index.ts:393), [Pi shutdown policy](/Users/provi/Development/_projs/wezterm-attention/pi/index.ts:497), [Lua acknowledgement filtering](/Users/provi/Development/_projs/wezterm-attention/plugin/reader.lua:288).

The adjacent catalog JSON records the complete provider name sets and inspected versions. The renderer checks that every catalog hook and every Claude notification subtype appears in its own provider column. This checks coverage, not runtime semantics. Research snapshots for this run are retained at `/tmp/attention-hook-map.E2eGcG/`. No runtime files or live configuration changed in this work.
