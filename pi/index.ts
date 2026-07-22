import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { mkdir, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";

const DEFAULT_TTL_MS = 30 * 60 * 1000;
const ATTENTION_EVENT = "wezterm-attention:mark";
// Cap the session_shutdown drain so a hung marker FS can never wedge Pi's reload
// or quit. Measured p99 for one write is well under a millisecond, so this is
// only a safety valve — but note it bounds the WHOLE queued backlog, not a single
// write: the drain awaits the tail of the serial chain, so N queued writes trip
// it once their SUM exceeds the cap, even though no individual write is slow.
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
// marker present). The chain is module-local; cross-generation ordering across a
// reload is handled by draining it in session_shutdown (which Pi awaits before
// re-evaluating the module), not by any per-generation disposal here. The chain
// swallows results so one failure never poisons later operations.
let mutationChain: Promise<unknown> = Promise.resolve();
function enqueue(op: () => Promise<void>): Promise<void> {
	const run = mutationChain.then(op, op);
	mutationChain = run.then(
		() => undefined,
		() => undefined,
	);
	return run;
}

// Monotonic counter keeps temp filenames unique within the process even for
// same-millisecond writes; serialization already prevents overlap, this is
// defense in depth.
let tmpSeq = 0;

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

	const tmp = `${path}.tmp.${process.pid}.${Date.now()}.${tmpSeq++}`;
	try {
		await mkdir(dir, { recursive: true });
		await writeFile(tmp, JSON.stringify(marker) + "\n");
		await rename(tmp, path);
	} catch {
		// Best-effort: a marker that fails to write just means the tab doesn't change.
		// A failed rename (e.g. the destination is a directory) leaves tmp behind —
		// remove it so failures don't accumulate filesystem residue. Cleanup only on
		// failure; on success tmp was already renamed away.
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

// Accepts a bare string ("notify") or an object ({ type | state | command, label }).
function normalizeEventData(data: unknown): { state: AttentionState | "clear"; label?: string } | undefined {
	if (typeof data === "string") {
		const state = normalizeState(data);
		return state ? { state } : undefined;
	}
	if (!isRecord(data)) return undefined;

	const raw = stringFromUnknown(data.state) ?? stringFromUnknown(data.type) ?? stringFromUnknown(data.command);
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
// This keying relies on `pi.events` being referentially stable across /reload.
// The bus is created once per resource-loader (not strictly per process) and the
// same object is passed to every reloaded generation, which holds. A /new, /fork,
// /resume, or fresh import gets a DIFFERENT bus — that is safe, not a leak:
// nothing emits on or retains the old bus, and the WeakMap value closes over the
// emitter rather than the key, so the stale entry is collectible. Only a
// hypothetical Pi that swapped the bus on /reload itself would defeat the keying.
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

export default function weztermAttentionPiExtension(pi: ExtensionAPI): void {
	// Automatic: Pi lifecycle → WezTerm tab state. Lifecycle handlers are stored
	// per-extension-instance by Pi's runner and replaced wholesale on reload, so
	// they don't accumulate — safe to register on every load.
	pi.on("agent_start", async () => {
		await mark("thinking");
	});

	pi.on("tool_execution_start", async () => {
		await mark("thinking");
	});

	// `agent_settled`, NOT `agent_end`: agent_end fires at the end of every
	// low-level run, but Pi may still auto-retry, auto-compact and retry, or
	// continue with queued follow-up messages — writing `stop` there flashes a
	// false ✓ mid-task. agent_settled fires only once Pi will not continue
	// running automatically. Requires Pi >= 0.80.5.
	pi.on("agent_settled", async () => {
		await mark("stop");
	});

	// Cooperative: any other Pi extension (e.g. an ask-user extension) can emit
	// this event to request a state — notably `notify` (the "waiting for you" `!`),
	// which the lifecycle events never produce. Emit a bare string ("notify") or
	// an object ({ type: "notify", label }); "clear" removes the marker.
	// The bus ignores the handler's return value; we return the mutation promise
	// so callers/tests can await completion deterministically.
	//
	// Register ours FIRST, record it, THEN retire the prior generation's listener
	// on THIS bus (see busRegistry) so reloads never accumulate listeners. This is
	// synchronous — no await between the steps — so no emit can observe both live.
	// Ordering it register-then-retire means a throw in `pi.events.on` can never
	// leave zero listeners (the old one survives); the worst case is a leak, never
	// silence.
	const registry = busRegistry();
	const bus = pi.events as unknown as object;
	const prev = registry.get(bus);
	const disposeEvent = pi.events.on(ATTENTION_EVENT, (data) => {
		const request = normalizeEventData(data);
		if (!request) return;
		return request.state === "clear" ? clearMarker() : mark(request.state, request.label);
	});
	registry.set(bus, disposeEvent);
	prev?.();

	// Drain in-flight writes on teardown. Pi awaits session_shutdown before it
	// re-loads extensions, so every write ENQUEUED BEFORE shutdown settles before
	// the next generation's (fresh) chain starts — the common-path cross-reload
	// ordering case. (One narrow, self-correcting residual: our listener is kept
	// live through the reload for failed-reload safety, so a late emit delivered
	// during Pi's post-shutdown steps enqueues on the old chain and could still be
	// writing as the new generation writes; the next event corrects the marker.)
	// We deliberately do NOT dispose here (that happens at the next registration);
	// disposing on a shutdown that precedes a *failed* reload would silence us
	// with no successor.
	//
	// The drain is bounded: Pi awaits this handler with no timeout of its own, so
	// a hung marker FS must never wedge /reload or quit.
	//
	// Be precise about what the cap trades away. Timing out does NOT cancel the
	// abandoned write — it is still in flight, and it can land AFTER the next
	// generation has written, leaving the older state on disk (e.g. a stale
	// `thinking` overwriting a fresh `stop`, which the plugin then renders until
	// the next event or the marker's own ttl_ms expires). So the cap converts
	// "reload hangs" into "marker may be briefly wrong", which is the right trade
	// for a best-effort indicator, but it is a different failure, not no failure.
	// Reachable only on /reload and the /new,/fork,/resume teardowns (quit exits
	// the process immediately), and only when the queued backlog exceeds the cap.
	//
	// Do NOT "fix" this with a sticky abandoned flag that suppresses later writes:
	// a generation survives a *failed* reload by design (see above and the
	// failed-reload test), so one timeout plus one failed reload would silence the
	// extension permanently. Gate on a per-operation generation counter instead.
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
