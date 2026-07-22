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
	updated_at: number;
	label?: string;
	ttl_ms?: number;
};

// Resolve the marker directory, then require it to be absolute. This closes two
// degenerate-env holes: WEZTERM_ATTENTION_DIR="" would otherwise leave the clear
// path calling rm() on a cwd-relative name (deleting a file where Pi was
// launched), and HOME="" resolves to a relative ".local/..." that scatters
// markers into the launch directory where the plugin never looks.
//
// The two operators do NOT split that work evenly. `||` (not `??`) matters only
// for the override: with `||` an empty WEZTERM_ATTENTION_DIR falls through to the
// HOME-based default and markers are still written, where `??` would keep "" and
// no-op everything. For HOME it changes nothing — os.homedir() also returns ""
// when HOME="" — so the isAbsolute gate below is what actually closes that hole.
// Do not remove it on the assumption that `||` covers HOME; it does not.
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
	const marker: Marker = {
		type: state,
		source: "pi",
		updated_at: Date.now(),
	};
	if (label) marker.label = label;
	if (state === "thinking") marker.ttl_ms = ttlMs();

	const tmp = `${path}.tmp.${randomUUID()}`;
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

// Retire the previous generation's bus listener when a NEW generation registers,
// rather than in session_shutdown. The shared event bus is not cleared on reload,
// so a leaked listener would accumulate — but disposal cannot happen on shutdown:
// reload() emits session_shutdown BEFORE its fallible work, and handleReloadCommand
// catches a reload failure and keeps the session running, so disposing on shutdown
// would kill the notify path with no replacement. Disposing only once a successor
// provably exists (at the next registration) avoids that. The registry is a
// WeakMap keyed by the bus object, so a multi-loader process never disposes
// another bus's listener.
//
// Keyed on `pi.events`, which is referentially stable across /reload (one bus per
// resource-loader, handed to every reloaded generation). A /new, /fork, /resume or
// fresh import gets a different bus; that is correct, not a leak — nothing retains
// the old one, so its WeakMap entry is collectible.
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

// Register this generation's listener, then retire the predecessor's. Order is
// load-bearing: register-before-retire means a throw in `pi.events.on` leaks a
// listener rather than leaving zero, and the steps are synchronous so no emit can
// observe both live. Retiring is best-effort — a throwing disposer must never fail
// the host's reload.
//
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
	// Automatic: Pi lifecycle → WezTerm tab state. Lifecycle handlers are stored
	// per-extension-instance by Pi's runner and replaced wholesale on reload, so
	// they don't accumulate — safe to register on every load.
	// These do NOT await the write. Pi awaits lifecycle handlers on the agent's own
	// critical path — agent-loop.ts emits `tool_execution_start` and awaits it before
	// preparing the tool call, through agent.ts's serial `await listener(...)` and
	// agent-session.ts's `await this._emitExtensionEvent(event)`, down to runner.ts's
	// `await handler(event, ctx)`. None of those has a timeout. So awaiting a marker
	// write here puts the filesystem inside the agent's latency budget, and on a mount
	// whose syscalls block indefinitely (hard NFS/SMB, dead FUSE daemon) it wedges the
	// host outright: every write shares one serial chain, so a single stuck op parks
	// every later lifecycle event too, the TUI never sees the event (the notify at
	// agent-session.ts:601 is downstream of the await), and aborting does not release
	// an already-parked await. Headless is worse — `_resolveIdleWaitIfIdle()` sits in a
	// `finally` whose `try` awaits this emit, so `pi -p` never terminates.
	//
	// A tab tint is best-effort; the host's liveness is not ours to spend. Ordering is
	// unaffected: `enqueue` chains synchronously at call time, so emit order == apply
	// order is a property of CALL order, not await order. The session_shutdown drain is
	// what guarantees these land before a reload — which makes that drain load-bearing
	// in a way it was not before. Do not weaken it.
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

	// Drain in-flight writes on teardown: Pi awaits session_shutdown before it
	// re-loads, so everything enqueued before shutdown lands before the next
	// generation's fresh chain starts. Drain only — do NOT dispose the listener
	// here (see installBusListener): a shutdown preceding a *failed* reload would
	// leave us silenced with no successor.
	//
	// The cap does not cancel an abandoned write; it can land after the next
	// generation's write and leave stale state until the next event or the marker's
	// ttl_ms. Accepted trade for a best-effort indicator. Do not "fix" it with a
	// sticky abandoned flag — one failed reload turns that into permanent silence;
	// use a per-operation generation counter if it ever matters.
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
