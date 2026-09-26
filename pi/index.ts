import type {
	ExtensionAPI,
	ExtensionContext,
	ExtensionEvent,
	SessionShutdownEvent,
	SessionStartEvent,
} from "@earendil-works/pi-coding-agent";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { accessSync, constants } from "node:fs";
import { isAbsolute, join } from "node:path";

const ATTENTION_EVENT = "wezterm-attention:mark";
// Cap for the session_shutdown drain. Bounds the WHOLE queued backlog, not one
// write — the drain awaits the tail of the serial chain, so N writes trip it once
// their SUM exceeds the cap, even though no single write is slow.
const DRAIN_TIMEOUT_MS = 2000;

type AttentionState = "thinking" | "stop" | "notify" | "review";

type PiStartSource = SessionStartEvent["reason"];

type SessionFacts = {
	sessionId: string;
	sessionFile?: string;
	cwd: string;
	model?: string;
	launchId?: string;
};

type WriterRequest =
	| ({ kind: "binding"; startSource: PiStartSource } & SessionFacts)
	| ({
		kind: "activity";
		state: "thinking" | "stop" | "notify";
		event: "agent_start" | "agent_settled" | "bus";
		label?: string;
	} & SessionFacts)
	| ({ kind: "tool_start"; toolCallId: string; toolName: string } & SessionFacts)
	| ({ kind: "tool_end"; toolCallId: string; toolName: string; isError: boolean } & SessionFacts)
	| ({ kind: "input"; source: Extract<ExtensionEvent, { type: "input" }>["source"] } & SessionFacts)
	| ({ kind: "attempt_outcome"; stopReason: "error" | "aborted" } & SessionFacts)
	| ({ kind: "compaction"; event: "session_before_compact" | "session_compact"; reason: Extract<ExtensionEvent, { type: "session_compact" }>["reason"] } & SessionFacts)
	| ({ kind: "review" } & SessionFacts)
	| ({ kind: "clear" } & SessionFacts)
	| ({ kind: "end"; reason: SessionShutdownEvent["reason"] } & SessionFacts);

// Tells the user one warning; the extension passes a Pi UI notification.
type Report = (message: string) => void;

type WriterResult =
	| { kind: "succeeded" }
	| { kind: "outside_pane" }
	| { kind: "failed"; message: string };

// Node decodes the environment as UTF-8 and puts U+FFFD where the bytes were
// not, so a value holding it most likely was not UTF-8. A path that really
// contains U+FFFD is refused too.
function decodedFromBrokenBytes(value: string): boolean {
	return value.includes("\uFFFD");
}

// The drain cap is a wall clock, so a test that asserts the drain finished is
// really asserting that a process spawn fits inside it. On a loaded machine it
// does not, and the test fails for a reason that has nothing to do with the
// extension. The parse takes strict digits only: a malformed value falls back
// rather than collapsing the cap to something near zero.
//
// The upper bound is not defensive tidiness. `setTimeout` stores its delay in a
// signed 32-bit int, so a larger one wraps to 1ms: measured at 2ms in Node and
// 3ms in Bun for a requested 2147483648. Someone raising this to "effectively
// unlimited" would get the shortest drain possible, which is the opposite of
// what they asked for and fails silently. Exported so the boundary can be tested
// without waiting on a timer.
const MAX_TIMEOUT_MS = 2_147_483_647;
export function drainTimeoutMs(): number {
	const raw = process.env.PI_WEZTERM_ATTENTION_DRAIN_TIMEOUT_MS;
	if (!raw || !/^\d+$/.test(raw.trim())) return DRAIN_TIMEOUT_MS;
	const parsed = Number.parseInt(raw.trim(), 10);
	return parsed > 0 && parsed <= MAX_TIMEOUT_MS ? parsed : DRAIN_TIMEOUT_MS;
}

// The states other extensions may request over the event bus, or "clear".
function normalizeState(value: string): AttentionState | "clear" | undefined {
	switch (value) {
		case "thinking":
		case "stop":
		case "notify":
		case "review":
		case "clear":
			return value;
		default:
			return undefined;
	}
}

// All mutations run through one serial chain so emit order == apply order even
// for the fire-and-forget event path: a `notify` immediately followed by a
// `clear` must not race (the clear landing first would leave the notify
// showing). The chain is module-local; cross-reload ordering comes from
// the session_shutdown drain. Results are swallowed so one failure never poisons
// later operations — that contract is `enqueue`'s own, deliberately independent
// of whether today's callers happen to be non-rejecting.
let mutationChain: Promise<unknown> = Promise.resolve();
function enqueue(op: () => Promise<void>): Promise<void> {
	const run = mutationChain.then(op, op);
	mutationChain = run.then(
		() => undefined,
		() => undefined,
	);
	return run;
}

function sessionFacts(ctx: ExtensionContext): SessionFacts | undefined {
	const sessionId = ctx.sessionManager.getSessionId();
	if (typeof sessionId !== "string" || sessionId.length === 0) return undefined;
	const sessionFile = ctx.sessionManager.getSessionFile();
	const model = typeof ctx.model?.id === "string" && ctx.model.id.length > 0 ? ctx.model.id : undefined;
	return {
		sessionId,
		launchId: process.env.WEZTERM_ATTENTION_LAUNCH_ID,
		...(typeof sessionFile === "string" && sessionFile.length > 0 ? { sessionFile } : {}),
		cwd: ctx.cwd,
		...(model ? { model } : {}),
	};
}

// The Rust writer refuses a WEZTERM_ATTENTION_DIR or XDG_STATE_HOME that is
// not UTF-8 where it decides the root: the first of the two that is not empty,
// and XDG_STATE_HOME only when absolute, since a relative one is ignored.
// The child is given the decoded text, U+FFFD encoded as valid UTF-8, so it
// would take that as a root no reader resolves; the refusal is made here.
function writerStateRootRefusal(): string | undefined {
	for (const name of ["WEZTERM_ATTENTION_DIR", "XDG_STATE_HOME"]) {
		const value = process.env[name];
		if (!value) continue;
		const decides = name !== "XDG_STATE_HOME" || isAbsolute(value);
		return decides && decodedFromBrokenBytes(value) ? `wezterm-attention: ${name} is not UTF-8` : undefined;
	}
	return undefined;
}

// Without the attention command nothing records what Pi is doing: the tab
// shows nothing for it, and the one warning below says why. Outside a WezTerm
// pane there is no tab, so an unset root there is not worth a warning.
function writerExecutable():
	| { kind: "ready"; executable: string }
	| { kind: "outside_pane" }
	| { kind: "failed"; message: string } {
	const root = process.env.WEZTERM_ATTENTION_ROOT;
	if (root === undefined) {
		if (!process.env.WEZTERM_PANE) return { kind: "outside_pane" };
		return {
			kind: "failed",
			message: "wezterm-attention: WEZTERM_ATTENTION_ROOT is not set, so nothing is recorded; build the attention command with scripts/install-cli.sh",
		};
	}
	if (!root || !isAbsolute(root)) {
		return { kind: "failed", message: "wezterm-attention: WEZTERM_ATTENTION_ROOT must name an absolute checkout" };
	}
	const refusal = writerStateRootRefusal();
	if (refusal) return { kind: "failed", message: refusal };
	const executable = join(root, "bin", "attention");
	try {
		accessSync(executable, constants.X_OK);
	} catch {
		return { kind: "failed", message: "wezterm-attention: configured checkout has no executable bin/attention" };
	}
	return { kind: "ready", executable };
}

function writerInvocation(request: WriterRequest): { event: string; payload: Record<string, unknown> } {
	const common: Record<string, unknown> = {
		session_id: request.sessionId,
		cwd: request.cwd,
		...(request.sessionFile ? { session_file: request.sessionFile } : {}),
		...(request.model ? { model: request.model } : {}),
	};
	switch (request.kind) {
		case "compaction":
			return { event: request.event, payload: { ...common, reason: request.reason } };
		case "tool_start":
			return { event: "tool_execution_start", payload: { ...common, tool_name: request.toolName, tool_use_id: request.toolCallId } };
		case "tool_end":
			return { event: "tool_execution_end", payload: { ...common, tool_name: request.toolName, tool_use_id: request.toolCallId, is_error: request.isError } };
		case "input":
			return { event: "input", payload: { ...common, source: request.source } };
		case "attempt_outcome":
			return { event: "message_end", payload: { ...common, role: "assistant", stop_reason: request.stopReason } };
		case "binding":
			return { event: "session_start", payload: { ...common, start_source: request.startSource } };
		case "activity":
			return {
				event: request.event,
				payload: request.event === "bus"
					? { ...common, state: request.state, ...(request.label ? { label: request.label } : {}) }
					: common,
			};
		case "review":
			return { event: "bus", payload: { ...common, state: "review" } };
		case "clear":
			return { event: "bus", payload: { ...common, state: "clear" } };
		case "end":
			return { event: "session_shutdown", payload: { ...common, reason: request.reason } };
	}
}

// The first line of the writer's stderr, for the warning that reports it. It
// comes from another process and is shown in Pi's interface, so control
// characters are dropped and the length is capped.
const DIAGNOSTIC_BYTES = 4096;
const DIAGNOSTIC_CHARACTERS = 200;
function firstDiagnosticLine(stderr: Buffer): string {
	const line = stderr.toString("utf8").split("\n", 1)[0] ?? "";
	return line.replace(/\p{Cc}/gu, "").trim().slice(0, DIAGNOSTIC_CHARACTERS);
}

async function invokeWriter(request: WriterRequest, transportId: string): Promise<WriterResult> {
	const target = writerExecutable();
	if (target.kind !== "ready") return target;
	const invocation = writerInvocation(request);
	return new Promise<WriterResult>((resolve) => {
		let settled = false;
		let diagnosticReceived = false;
		let diagnostic = Buffer.alloc(0);
		const finish = (result: WriterResult) => {
			if (settled) return;
			settled = true;
			resolve(result);
		};
		try {
			// The writer's direct parent is this Pi process, and a pane it claims is
			// claimed for the process this names, so a value Pi inherited is replaced.
			// The launch id is the one captured with the request; unset when there
			// was none.
			const child = spawn(
				target.executable,
				["hooks", "event", "pi", invocation.event],
				{
					env: {
						...process.env,
						WEZTERM_ATTENTION_HOST_PID: String(process.pid),
						WEZTERM_ATTENTION_LAUNCH_ID: request.launchId,
					},
					stdio: ["pipe", "ignore", "pipe"],
				},
			);
			child.once("error", () => finish({ kind: "failed", message: "wezterm-attention: writer process could not start" }));
			child.stderr.on("data", (chunk: Buffer | string) => {
				if (chunk.length > 0) diagnosticReceived = true;
				if (diagnostic.length < DIAGNOSTIC_BYTES) {
					diagnostic = Buffer.concat([diagnostic, Buffer.from(chunk)]).subarray(0, DIAGNOSTIC_BYTES);
				}
			});
			child.stderr.once("error", () => finish({ kind: "failed", message: "wezterm-attention: writer diagnostics failed" }));
			child.once("close", (code) => {
				if (code === 0 && !diagnosticReceived) return finish({ kind: "succeeded" });
				const summary = code === 0
					? "wezterm-attention: writer rejected the event"
					: `wezterm-attention: writer exited with status ${code ?? "signal"}`;
				const line = firstDiagnosticLine(diagnostic);
				finish({ kind: "failed", message: line ? `${summary}: ${line}` : summary });
			});
			child.stdin.once("error", () => finish({ kind: "failed", message: "wezterm-attention: writer input failed" }));
			child.stdin.end(JSON.stringify({ ...invocation.payload, transport_id: transportId }));
		} catch {
			finish({ kind: "failed", message: "wezterm-attention: writer process could not start" });
		}
	});
}

function enqueueWriter(request: WriterRequest, reportFailure: Report): Promise<void> {
	const transportId = randomUUID();
	return enqueue(async () => {
		const result = await invokeWriter(request, transportId);
		if (result.kind === "failed") reportFailure(result.message);
	});
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringFromUnknown(value: unknown): string | undefined {
	return typeof value === "string" ? value : undefined;
}

// Accepts a bare string ("notify") or an object ({ type | state, label }).
function normalizeEventData(data: unknown): { state: AttentionState | "clear"; label?: string } | undefined {
	if (typeof data === "string") {
		const state = normalizeState(data);
		return state ? { state } : undefined;
	}
	if (!isRecord(data)) return undefined;

	const raw = stringFromUnknown(data.type) ?? stringFromUnknown(data.state);
	if (!raw) return undefined;

	const state = normalizeState(raw);
	if (!state) return undefined;

	const label = stringFromUnknown(data.label);
	return { state, ...(label ? { label } : {}) };
}

// Retire the previous generation's bus listener at the NEXT registration, not in
// session_shutdown. The shared bus is never cleared, so listeners would accumulate —
// but disposing on shutdown is unsafe: reload() emits it before its fallible work, and
// a caught reload failure keeps the old generation running, so we'd kill the notify
// path with no replacement. Disposing only once a successor exists avoids that. Keyed
// on `pi.events`, stable across /reload (one bus per resource-loader); a /new, /fork,
// /resume or fresh import gets a different bus — safe, its stale entry is collectible.
const REGISTRY_KEY = "__weztermAttentionPiBusRegistry__";
function busRegistry(): WeakMap<object, () => void> {
	const g = globalThis as Record<string, unknown>;
	let reg = g[REGISTRY_KEY] as WeakMap<object, () => void> | undefined;
	if (!reg) {
		reg = new WeakMap();
		g[REGISTRY_KEY] = reg;
	}
	return reg;
}

// Register FIRST, then retire the predecessor: a throw in `pi.events.on` then leaks a
// listener rather than leaving zero, and the steps are synchronous so no emit sees
// both. Retiring is best-effort — a throwing disposer must not fail the host reload.
// This function is the seam the bus-owned-controller design would replace.
function installBusListener(pi: ExtensionAPI, handler: (data: unknown) => unknown): void {
	const registry = busRegistry();
	const bus: object = pi.events;
	const prev = registry.get(bus);
	const disposeEvent = pi.events.on(ATTENTION_EVENT, handler);
	registry.set(bus, disposeEvent);
	try {
		prev?.();
	} catch {
		// Best-effort dedup; a failed retire leaves a duplicate, never a broken load.
	}
}

export default function weztermAttentionPiExtension(pi: ExtensionAPI): void {
	let currentSession: SessionFacts | undefined;
	let currentContext: ExtensionContext | undefined;
	let writerFailureReported = false;
	const reportWriterFailure = (message: string) => {
		if (writerFailureReported) return;
		writerFailureReported = true;
		if (currentContext) currentContext.ui.notify(message, "warning");
		else console.error(message);
	};

	// Forwards one Pi event to the writer, for the session `ctx` belongs to.
	// `remember` makes that session the one bus requests are written into and
	// its UI the one a warning goes to.
	//
	// The write is NOT awaited. Pi awaits lifecycle handlers on the agent's own
	// critical path with no timeout, so awaiting the writer here would put the
	// filesystem in the agent's latency budget — and on a mount whose syscalls block
	// forever (hard NFS/SMB, dead FUSE) it would wedge the host (one stuck op parks
	// every later event via the shared chain; headless `pi -p` never terminates). A tab
	// tint is best-effort; host liveness is not. Ordering is unaffected — `enqueue`
	// chains synchronously at call time, so emit order == apply order regardless of the
	// await. The session_shutdown drain guarantees these land before a reload; don't
	// weaken it.
	const forward = (ctx: ExtensionContext, request: (facts: SessionFacts) => WriterRequest, remember = false) => {
		const facts = sessionFacts(ctx);
		if (!facts) return;
		if (remember) {
			currentContext = ctx;
			currentSession = facts;
		}
		void enqueueWriter(request(facts), reportWriterFailure).catch(() => {});
	};

	// Automatic: Pi lifecycle → WezTerm tab state. Lifecycle handlers are per-instance,
	// replaced wholesale on reload, so they don't accumulate (unlike the shared bus
	// listener below — no dedup needed here).
	pi.on("session_start", (event, ctx) =>
		forward(ctx, (facts) => ({ kind: "binding", startSource: event.reason, ...facts }), true));
	pi.on("agent_start", (_event, ctx) =>
		forward(ctx, (facts) => ({ kind: "activity", state: "thinking", event: "agent_start", ...facts }), true));
	pi.on("tool_execution_start", (event, ctx) =>
		forward(ctx, (facts) => ({ kind: "tool_start", toolName: event.toolName, toolCallId: event.toolCallId, ...facts }), true));
	pi.on("tool_execution_end", (event, ctx) =>
		forward(ctx, (facts) => ({ kind: "tool_end", toolName: event.toolName, toolCallId: event.toolCallId, isError: event.isError, ...facts })));
	pi.on("input", (event, ctx) => forward(ctx, (facts) => ({ kind: "input", source: event.source, ...facts })));
	pi.on("message_end", (event, ctx) => {
		const stopReason = event.message.role === "assistant" ? event.message.stopReason : undefined;
		if (stopReason !== "error" && stopReason !== "aborted") return;
		forward(ctx, (facts) => ({ kind: "attempt_outcome", stopReason, ...facts }));
	});
	pi.on("session_before_compact", (event, ctx) =>
		forward(ctx, (facts) => ({ kind: "compaction", event: event.type, reason: event.reason, ...facts })));
	pi.on("session_compact", (event, ctx) =>
		forward(ctx, (facts) => ({ kind: "compaction", event: event.type, reason: event.reason, ...facts })));

	// `agent_settled`, NOT `agent_end`: agent_end fires at the end of every
	// low-level run, but Pi may still auto-retry, auto-compact and retry, or
	// continue with queued follow-up messages — writing `stop` there flashes a
	// false ✓ mid-task. agent_settled fires only once Pi will not continue
	// running automatically. Requires Pi >= 0.80.5.
	pi.on("agent_settled", (_event, ctx) =>
		forward(ctx, (facts) => ({ kind: "activity", state: "stop", event: "agent_settled", ...facts }), true));

	// Cooperative: any other Pi extension (e.g. an ask-user extension) can emit
	// this event to request a state — notably `notify` (the "waiting for you" `!`),
	// which the lifecycle events never produce. Emit a bare string ("notify") or
	// an object ({ type: "notify", label }); "clear" withdraws it.
	// The bus ignores the handler's return value; we return the mutation promise
	// so callers/tests can await completion deterministically.
	installBusListener(pi, (data) => {
		const request = normalizeEventData(data);
		if (!request) return;
		if (!currentSession) {
			reportWriterFailure("wezterm-attention: no Pi session is available for the configured writer");
			return;
		}
		if (request.state === "clear") return enqueueWriter({ kind: "clear", ...currentSession }, reportWriterFailure);
		if (request.state === "review") return enqueueWriter({ kind: "review", ...currentSession }, reportWriterFailure);
		return enqueueWriter({
			kind: "activity", state: request.state, event: "bus",
			...(request.label ? { label: request.label } : {}), ...currentSession,
		}, reportWriterFailure);
	});

	// Drain in-flight writes on teardown: Pi awaits session_shutdown before re-loading,
	// so everything enqueued before shutdown lands before the next generation's fresh
	// chain. Drain only — do NOT dispose the listener here (see installBusListener): a
	// shutdown before a *failed* reload would leave us with no successor.
	//
	// Two residuals are accepted best-effort costs, with different triggers:
	//   1. Ordering — on a merely slow or racing write, no dead mount needed: the cap
	//      doesn't cancel a write that overruns it, nor an op enqueued after this race
	//      snapshots the chain (e.g. a later extension emitting `clear` on the bus during
	//      its own shutdown, which our still-live listener enqueues past the snapshot — a
	//      pure race, fine on a healthy system). A stale write self-limits (the next
	//      event replaces it); a stale `clear` withdraws the state, and only a later
	//      write restores it.
	//   2. Memory — only under a *permanently* blocked write (a dead NFS/SMB/FUSE mount,
	//      not a merely slow one): the chain retains every op queued behind the stuck head
	//      (a real in-flight fs op is libuv-rooted), growing until the mount recovers or
	//      the process restarts.
	//
	// The obvious fixes are all worse: a sticky abandoned flag → permanent silence after
	// a failed reload; sharing the chain across generations → a stuck predecessor stalls
	// or wedges the successor; a per-op generation counter → the gate precedes the
	// publish, so it can't catch a write already stalling inside the writer; a coalescing
	// mailbox → bounds memory but breaks the FIFO ordering the tests lock. The real fix
	// is the deferred bus-owned controller (one per bus, owning the listener + mutation
	// state, re-asserting the last requested state after an abandoned write lands).
	pi.on("session_shutdown", async (event, ctx) => {
		const facts = sessionFacts(ctx) ?? currentSession;
		currentContext = ctx;
		// Pi emits shutdown before attempting an extension reload. The provider's
		// contract keeps the current binding for that reason, so there is no writer
		// mutation to request and no diagnostic to reinterpret. Earlier queued writes
		// still drain below before the replacement generation starts.
		if (facts && event.reason !== "reload") {
			void enqueueWriter({ kind: "end", reason: event.reason, ...facts }, reportWriterFailure).catch(() => {});
		}
		let timer: ReturnType<typeof setTimeout> | undefined;
		await Promise.race([
			mutationChain,
			new Promise<void>((resolve) => {
				timer = setTimeout(resolve, drainTimeoutMs());
			}),
		]);
		if (timer) clearTimeout(timer);
	});
}
