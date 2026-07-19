import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { mkdir, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const DEFAULT_TTL_MS = 30 * 60 * 1000;
const ATTENTION_EVENT = "wezterm-attention:mark";

type AttentionState = "thinking" | "stop" | "notify" | "review";

type Marker = {
	type: AttentionState;
	source: "pi";
	updated_at: number;
	label?: string;
	ttl_ms?: number;
};

function markerDirectory(): string {
	return process.env.WEZTERM_ATTENTION_DIR ?? join(process.env.HOME ?? homedir(), ".local", "state", "wezterm-attention");
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
	const parsed = Number.parseInt(raw, 10);
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
// marker present). The chain swallows results so one failure never poisons later
// operations.
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
	try {
		await rm(join(markerDirectory(), id), { force: true });
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

export default function weztermAttentionPiExtension(pi: ExtensionAPI): void {
	// Automatic: Pi lifecycle → WezTerm tab state. These produce thinking/stop only.
	pi.on("agent_start", async () => {
		await mark("thinking");
	});

	pi.on("tool_execution_start", async () => {
		await mark("thinking");
	});

	pi.on("agent_end", async () => {
		await mark("stop");
	});

	// Cooperative: any other Pi extension (e.g. an ask-user extension) can emit
	// this event to request a state — notably `notify` (the "waiting for you" `!`),
	// which the lifecycle events never produce. Emit a bare string ("notify") or
	// an object ({ type: "notify", label }); "clear" removes the marker.
	// The bus ignores the handler's return value; we return the mutation promise
	// so callers/tests can await completion deterministically.
	pi.events.on(ATTENTION_EVENT, (data) => {
		const request = normalizeEventData(data);
		if (!request) return;
		return request.state === "clear" ? clearMarker() : mark(request.state, request.label);
	});
}
