import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { randomUUID } from "node:crypto";
import { mkdir, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";

const DEFAULT_TTL_MS = 30 * 60 * 1000;
const ATTENTION_EVENT = "wezterm-attention:mark";
// Cap for the session_shutdown drain. Bounds the WHOLE queued backlog, not one
// write — the drain awaits the tail of the serial chain, so N writes trip it once
// their SUM exceeds the cap, even though no single write is slow.
const DRAIN_TIMEOUT_MS = 2000;

type AttentionState = "thinking" | "stop" | "notify" | "review";

type Marker = {
	type: AttentionState;
	source: "pi";
	publication_id: string;
	updated_at: number;
	label?: string;
	ttl_ms?: number;
};

// Resolve the marker dir and require it absolute — closes two degenerate-env holes:
// WEZTERM_ATTENTION_DIR="" would make clear's rm() delete a cwd-relative file, and
// HOME="" resolves to a relative ".local/..." that scatters markers. `||` (not `??`)
// so an empty override falls through to the HOME default; the isAbsolute gate is what
// closes the HOME hole (homedir() also returns "" for HOME=""), so don't drop it.
function markerDirectory(): string | undefined {
	const dir = process.env.WEZTERM_ATTENTION_DIR || join(process.env.HOME || homedir(), ".local", "state", "wezterm-attention");
	return isAbsolute(dir) ? dir : undefined;
}

// WezTerm injects WEZTERM_PANE as a non-negative integer pane id. Validate the
// contract: an unvalidated value like "../../foo" would escape the marker
// directory, and clearMarker's rm could then delete an arbitrary file.
function paneId(): string | undefined {
	const id = process.env.WEZTERM_PANE;
	if (!id || !/^\d+$/.test(id)) return undefined;
	return id;
}

function ttlMs(): number {
	const raw = process.env.PI_WEZTERM_ATTENTION_TTL_MS;
	if (!raw) return DEFAULT_TTL_MS;
	// Strict digits only: parseInt("30m") is 30, silently turning a "30 minutes"
	// typo into a 30ms TTL that expires the spinner almost instantly. Reject
	// anything that isn't a plain integer and fall back to the default.
	if (!/^\d+$/.test(raw.trim())) return DEFAULT_TTL_MS;
	const parsed = Number.parseInt(raw.trim(), 10);
	return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_TTL_MS;
}

// Maps the states other extensions may request over the event bus (including a
// few friendly aliases) to a canonical marker state, or "clear".
function normalizeState(value: string): AttentionState | "clear" | undefined {
	switch (value) {
		case "busy":
		case "thinking":
			return "thinking";
		case "ready":
		case "stop":
			return "stop";
		case "blocked":
		case "pending":
		case "notify":
			return "notify";
		case "review":
			return "review";
		case "clear":
			return "clear";
		default:
			return undefined;
	}
}

// All mutations run through one serial chain so emit order == apply order even
// for the fire-and-forget event path: a `notify` immediately followed by a
// `clear` must not race (rm finishing before the write's rename would leave the
// marker present). The chain is module-local; cross-reload ordering comes from
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

async function writeMarkerNow(state: AttentionState, label?: string): Promise<void> {
	const id = paneId();
	if (!id) return;

	const dir = markerDirectory();
	if (!dir) return;
	const path = join(dir, id);
	const publicationId = randomUUID();
	const marker: Marker = {
		type: state,
		source: "pi",
		publication_id: publicationId,
		updated_at: Date.now(),
	};
	if (label) marker.label = label;
	if (state === "thinking") marker.ttl_ms = ttlMs();

	const tmp = `${path}.tmp.${publicationId}`;
	try {
		await mkdir(dir, { recursive: true });
		await writeFile(tmp, JSON.stringify(marker) + "\n");
		await rename(tmp, path);
	} catch {
		// Best-effort: a marker that fails to write just means the tab doesn't change.
		// A failed rename (e.g. the destination is a directory) leaves tmp behind —
		// remove it so failures don't accumulate filesystem residue.
		await rm(tmp, { force: true }).catch(() => {});
	}
}

async function clearMarkerNow(): Promise<void> {
	const id = paneId();
	if (!id) return;
	const dir = markerDirectory();
	if (!dir) return;
	try {
		await rm(join(dir, id), { force: true });
	} catch {
		// Best-effort.
	}
}

function mark(state: AttentionState, label?: string): Promise<void> {
	return enqueue(() => writeMarkerNow(state, label));
}

function clearMarker(): Promise<void> {
	return enqueue(() => clearMarkerNow());
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
	const bus = pi.events as unknown as object;
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
	// Automatic: Pi lifecycle → WezTerm tab state. Lifecycle handlers are per-instance,
	// replaced wholesale on reload, so they don't accumulate (unlike the shared bus
	// listener below — no dedup needed here).
	//
	// These do NOT await the write. Pi awaits lifecycle handlers on the agent's own
	// critical path with no timeout, so awaiting marker I/O here would put the
	// filesystem in the agent's latency budget — and on a mount whose syscalls block
	// forever (hard NFS/SMB, dead FUSE) it would wedge the host (one stuck op parks
	// every later event via the shared chain; headless `pi -p` never terminates). A tab
	// tint is best-effort; host liveness is not. Ordering is unaffected — `enqueue`
	// chains synchronously at call time, so emit order == apply order regardless of the
	// await. The session_shutdown drain now guarantees these land before a reload; don't
	// weaken it.
	pi.on("agent_start", () => {
		void mark("thinking").catch(() => {});
	});

	pi.on("tool_execution_start", () => {
		void mark("thinking").catch(() => {});
	});

	// `agent_settled`, NOT `agent_end`: agent_end fires at the end of every
	// low-level run, but Pi may still auto-retry, auto-compact and retry, or
	// continue with queued follow-up messages — writing `stop` there flashes a
	// false ✓ mid-task. agent_settled fires only once Pi will not continue
	// running automatically. Requires Pi >= 0.80.5.
	pi.on("agent_settled", () => {
		void mark("stop").catch(() => {});
	});

	// Cooperative: any other Pi extension (e.g. an ask-user extension) can emit
	// this event to request a state — notably `notify` (the "waiting for you" `!`),
	// which the lifecycle events never produce. Emit a bare string ("notify") or
	// an object ({ type: "notify", label }); "clear" removes the marker.
	// The bus ignores the handler's return value; we return the mutation promise
	// so callers/tests can await completion deterministically.
	installBusListener(pi, (data) => {
		const request = normalizeEventData(data);
		if (!request) return;
		return request.state === "clear" ? clearMarker() : mark(request.state, request.label);
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
	//      pure race, fine on a healthy system). A stale write self-limits (next event or
	//      ttl_ms); a stale `clear` removes the marker, and ttl_ms can't restore an absent
	//      file — only a later write does.
	//   2. Memory — only under a *permanently* blocked write (a dead NFS/SMB/FUSE mount,
	//      not a merely slow one): the chain retains every op queued behind the stuck head
	//      (a real in-flight fs op is libuv-rooted), growing until the mount recovers or
	//      the process restarts.
	//
	// The obvious fixes are all worse: a sticky abandoned flag → permanent silence after
	// a failed reload; sharing the chain across generations → a stuck predecessor stalls
	// or wedges the successor; a per-op generation counter → the gate precedes the
	// publish, so it can't catch a write already stalling inside rename; a coalescing
	// mailbox → bounds memory but breaks the FIFO ordering the tests lock. The real fix
	// is the deferred bus-owned controller (one per bus, owning the listener + mutation
	// state, re-asserting the last requested state after an abandoned write lands).
	pi.on("session_shutdown", async () => {
		let timer: ReturnType<typeof setTimeout> | undefined;
		await Promise.race([
			mutationChain,
			new Promise<void>((resolve) => {
				timer = setTimeout(resolve, DRAIN_TIMEOUT_MS);
			}),
		]);
		if (timer) clearTimeout(timer);
	});
}
