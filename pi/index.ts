import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const DEFAULT_TTL_MS = 30 * 60 * 1000;
const ATTENTION_EVENT = "wezterm-attention:mark";

type AttentionState = "thinking" | "stop" | "notify" | "review";
type AttentionCommand = "busy" | "thinking" | "ready" | "stop" | "blocked" | "pending" | "notify" | "review" | "clear" | "status";

// Outcome of a marker mutation. Distinguishes "no pane" from "bad pane" from an
// actual filesystem failure so callers can report the true reason rather than
// collapsing every failure into "WEZTERM_PANE is not set".
type MarkOutcome = "ok" | "missing-pane" | "invalid-pane" | "io-error";

type Marker = {
	type: AttentionState;
	source: "pi";
	updated_at: number;
	label?: string;
	ttl_ms?: number;
};

const COMMANDS: Array<{ value: AttentionCommand; description: string }> = [
	{ value: "busy", description: "Mark this pane as thinking" },
	{ value: "ready", description: "Mark this pane as done" },
	{ value: "pending", description: "Alias for notify; mark this pane as waiting for human input" },
	{ value: "blocked", description: "Alias for notify" },
	{ value: "review", description: "Mark this pane for manual review" },
	{ value: "clear", description: "Clear this pane's attention marker" },
	{ value: "status", description: "Show current marker state" },
];

function markerDirectory(): string {
	return process.env.WEZTERM_ATTENTION_DIR ?? join(process.env.HOME ?? homedir(), ".local", "state", "wezterm-attention");
}

// WezTerm injects WEZTERM_PANE as a non-negative integer pane id. Validate the
// contract before building a path: an unvalidated value like "../../foo" would
// escape the marker directory, and clearMarker's rm would then delete an
// arbitrary file. Reject anything that is not purely digits.
function readPaneId(): { ok: true; id: string } | { ok: false; reason: "missing-pane" | "invalid-pane" } {
	const id = process.env.WEZTERM_PANE;
	if (!id) return { ok: false, reason: "missing-pane" };
	if (!/^\d+$/.test(id)) return { ok: false, reason: "invalid-pane" };
	return { ok: true, id };
}

function ttlMs(): number {
	const raw = process.env.PI_WEZTERM_ATTENTION_TTL_MS;
	if (!raw) return DEFAULT_TTL_MS;
	const parsed = Number.parseInt(raw, 10);
	return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_TTL_MS;
}

function normalizeCommand(command: string): AttentionState | "clear" | "status" | undefined {
	switch (command) {
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
		case "status":
		case "":
			return "status";
		default:
			return undefined;
	}
}

// All marker mutations (and status reads) run through this single serial chain,
// so emit order equals apply order even for the fire-and-forget event-bus path.
// Without it, a `notify` immediately followed by `clear` races: rm finishes
// before the write's rename, leaving the marker present. The chain swallows
// results so one failure never poisons later operations.
let mutationChain: Promise<unknown> = Promise.resolve();
function enqueue<T>(op: () => Promise<T>): Promise<T> {
	const run = mutationChain.then(op, op);
	mutationChain = run.then(
		() => undefined,
		() => undefined,
	);
	return run;
}

// Monotonic counter makes temp filenames unique within the process even for
// same-millisecond writes; serialization already prevents overlap, this is
// defense in depth.
let tmpSeq = 0;

async function writeMarkerNow(state: AttentionState, label?: string): Promise<MarkOutcome> {
	const pane = readPaneId();
	if (!pane.ok) return pane.reason;

	const dir = markerDirectory();
	const path = join(dir, pane.id);
	const marker: Marker = {
		type: state,
		source: "pi",
		updated_at: Date.now(),
	};
	if (label) marker.label = label;
	if (state === "thinking") marker.ttl_ms = ttlMs();

	try {
		await mkdir(dir, { recursive: true });
		const tmp = `${path}.tmp.${process.pid}.${Date.now()}.${tmpSeq++}`;
		await writeFile(tmp, JSON.stringify(marker) + "\n");
		await rename(tmp, path);
		return "ok";
	} catch {
		return "io-error";
	}
}

async function clearMarkerNow(): Promise<MarkOutcome> {
	const pane = readPaneId();
	if (!pane.ok) return pane.reason;
	try {
		await rm(join(markerDirectory(), pane.id), { force: true });
		return "ok";
	} catch {
		return "io-error";
	}
}

function writeMarker(state: AttentionState, label?: string): Promise<MarkOutcome> {
	return enqueue(() => writeMarkerNow(state, label));
}

function clearMarker(): Promise<MarkOutcome> {
	return enqueue(() => clearMarkerNow());
}

// Read behind the mutation chain so `status` reflects the latest queued write or
// clear rather than a stale on-disk state.
async function readMarkerText(): Promise<string | undefined> {
	const pane = readPaneId();
	if (!pane.ok) return undefined;
	return enqueue(async () => {
		try {
			return await readFile(join(markerDirectory(), pane.id), "utf8");
		} catch {
			return undefined;
		}
	});
}

async function mark(state: AttentionState, label?: string): Promise<MarkOutcome> {
	return writeMarker(state, label);
}

function writeMessage(outcome: MarkOutcome, command: string): { text: string; level: "info" | "warning" } {
	switch (outcome) {
		case "ok":
			return { text: `Marked WezTerm pane as ${command}`, level: "info" };
		case "missing-pane":
			return { text: "WEZTERM_PANE is not set; nothing written", level: "info" };
		case "invalid-pane":
			return { text: "WEZTERM_PANE is not a valid pane id; nothing written", level: "warning" };
		case "io-error":
			return { text: "Failed to write WezTerm marker (I/O error)", level: "warning" };
	}
}

function clearMessage(outcome: MarkOutcome): { text: string; level: "info" | "warning" } {
	switch (outcome) {
		case "ok":
			return { text: "Cleared WezTerm attention marker", level: "info" };
		case "missing-pane":
			return { text: "WEZTERM_PANE is not set; nothing to clear", level: "info" };
		case "invalid-pane":
			return { text: "WEZTERM_PANE is not a valid pane id; nothing to clear", level: "warning" };
		case "io-error":
			return { text: "Failed to clear WezTerm marker (I/O error)", level: "warning" };
	}
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringFromUnknown(value: unknown): string | undefined {
	return typeof value === "string" ? value : undefined;
}

function normalizeEventData(data: unknown): { state: AttentionState | "clear"; label?: string } | undefined {
	if (typeof data === "string") {
		const state = normalizeCommand(data);
		return state && state !== "status" ? { state } : undefined;
	}
	if (!isRecord(data)) return undefined;

	const rawState = stringFromUnknown(data.state) ?? stringFromUnknown(data.type) ?? stringFromUnknown(data.command);
	if (!rawState) return undefined;

	const state = normalizeCommand(rawState);
	if (!state || state === "status") return undefined;

	const label = stringFromUnknown(data.label);
	return { state, ...(label ? { label } : {}) };
}

export default function weztermAttentionPiExtension(pi: ExtensionAPI): void {
	pi.events.on(ATTENTION_EVENT, (data) => {
		const request = normalizeEventData(data);
		if (!request) return;
		if (request.state === "clear") {
			void clearMarker();
			return;
		}
		void mark(request.state, request.label);
	});

	pi.on("agent_start", async () => {
		await mark("thinking");
	});

	pi.on("tool_execution_start", async () => {
		await mark("thinking");
	});

	pi.on("agent_end", async () => {
		await mark("stop");
	});

	pi.registerCommand("attention", {
		description: "Control the WezTerm attention marker for this Pi pane",
		getArgumentCompletions: (prefix) => {
			const normalizedPrefix = prefix.trim().toLowerCase();
			return COMMANDS.filter((command) => command.value.startsWith(normalizedPrefix)).map((command) => ({
				value: command.value,
				label: command.value,
				description: command.description,
			}));
		},
		handler: async (args, ctx) => {
			const [rawCommand = "status", ...labelParts] = args.trim().split(/\s+/).filter(Boolean);
			const command = normalizeCommand(rawCommand.toLowerCase());
			const label = labelParts.join(" ").trim() || undefined;

			if (!command) {
				ctx.ui.notify(`Unknown attention command: ${rawCommand}`, "warning");
				return;
			}

			if (command === "status") {
				const pane = readPaneId();
				if (!pane.ok) {
					ctx.ui.notify(
						pane.reason === "missing-pane"
							? "WEZTERM_PANE is not set; attention markers are disabled"
							: "WEZTERM_PANE is not a valid pane id; attention markers are disabled",
						"info",
					);
					return;
				}
				const marker = await readMarkerText();
				ctx.ui.notify(marker ? `WezTerm attention marker: ${marker}` : "No WezTerm attention marker for this pane", "info");
				return;
			}

			if (command === "clear") {
				const { text, level } = clearMessage(await clearMarker());
				ctx.ui.notify(text, level);
				return;
			}

			const { text, level } = writeMessage(await mark(command, label), command);
			ctx.ui.notify(text, level);
		},
	});
}
