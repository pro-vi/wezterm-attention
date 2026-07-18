import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const DEFAULT_TTL_MS = 30 * 60 * 1000;
const ATTENTION_EVENT = "wezterm-attention:mark";

type AttentionState = "thinking" | "stop" | "notify" | "review";
type AttentionCommand = "busy" | "thinking" | "ready" | "stop" | "blocked" | "pending" | "notify" | "review" | "clear" | "status";

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

function markerPath(): string | undefined {
	const paneId = process.env.WEZTERM_PANE;
	if (!paneId) return undefined;
	return join(markerDirectory(), paneId);
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

async function writeMarker(state: AttentionState, label?: string): Promise<boolean> {
	const path = markerPath();
	if (!path) return false;

	const marker: Marker = {
		type: state,
		source: "pi",
		updated_at: Date.now(),
	};
	if (label) marker.label = label;
	if (state === "thinking") marker.ttl_ms = ttlMs();

	try {
		await mkdir(markerDirectory(), { recursive: true });
		const tmp = `${path}.tmp.${process.pid}.${Date.now()}`;
		await writeFile(tmp, JSON.stringify(marker) + "\n");
		await rename(tmp, path);
		return true;
	} catch {
		return false;
	}
}

async function clearMarker(): Promise<boolean> {
	const path = markerPath();
	if (!path) return false;
	try {
		await rm(path, { force: true });
		return true;
	} catch {
		return false;
	}
}

async function readMarkerText(): Promise<string | undefined> {
	const path = markerPath();
	if (!path) return undefined;
	try {
		return await readFile(path, "utf8");
	} catch {
		return undefined;
	}
}

async function mark(state: AttentionState, label?: string): Promise<boolean> {
	return writeMarker(state, label);
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
				const marker = await readMarkerText();
				if (!marker) {
					ctx.ui.notify(process.env.WEZTERM_PANE ? "No WezTerm attention marker for this pane" : "WEZTERM_PANE is not set; attention markers are disabled", "info");
					return;
				}
				ctx.ui.notify(`WezTerm attention marker: ${marker}`, "info");
				return;
			}

			if (command === "clear") {
				const cleared = await clearMarker();
				ctx.ui.notify(cleared ? "Cleared WezTerm attention marker" : "WEZTERM_PANE is not set; nothing to clear", "info");
				return;
			}

			const marked = await mark(command, label);
			ctx.ui.notify(marked ? `Marked WezTerm pane as ${command}` : "WEZTERM_PANE is not set; nothing written", "info");
		},
	});
}
