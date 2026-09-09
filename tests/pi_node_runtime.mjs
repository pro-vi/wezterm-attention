import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createJiti } from "jiti";

const root = mkdtempSync(join(tmpdir(), "attention-pi-node-"));
try {
	const bin = join(root, "bin");
	const log = join(root, "calls.log");
	mkdirSync(bin);
	writeFileSync(
		join(bin, "attention"),
		'#!/bin/sh\nprintf "%s\\n" "$*" >> "$WEZTERM_ATTENTION_TEST_LOG"\nIFS= read -r payload || :\nprintf "%s\\n" "$payload" >> "$WEZTERM_ATTENTION_TEST_LOG"\n',
	);
	chmodSync(join(bin, "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	delete process.env.WEZTERM_PANE;

	const lifecycle = new Map();
	const pi = {
		events: { on: () => () => {} },
		on: (name, handler) => lifecycle.set(name, handler),
		registerCommand: () => {},
	};
	const context = {
		cwd: root,
		sessionManager: {
			getSessionId: () => "node-runtime-session",
			getSessionFile: () => join(root, "session.jsonl"),
		},
		model: { id: "node-runtime-model" },
	};

	const jiti = createJiti(import.meta.url);
	const extension = await jiti.import("../pi/index.ts");
	assert.equal(typeof extension.default, "function");
	extension.default(pi);
	assert.equal(typeof globalThis.Bun, "undefined");

	lifecycle.get("session_start")({ type: "session_start", reason: "startup" }, context);
	lifecycle.get("agent_start")({ type: "agent_start" }, context);
	await lifecycle.get("session_shutdown")(
		{ type: "session_shutdown", reason: "quit" }, context,
	);

	const lines = readFileSync(log, "utf8").trim().split("\n");
	assert.deepEqual(lines.filter((_, index) => index % 2 === 0), [
		"hooks event pi session_start",
		"hooks event pi agent_start",
		"hooks event pi session_shutdown",
	]);
	assert.equal(lines.filter((_, index) => index % 2 === 1).length, 3);
	console.log("Pi Node runtime dispatch: 3/3 writer calls passed");
} finally {
	delete process.env.WEZTERM_ATTENTION_ROOT;
	delete process.env.WEZTERM_ATTENTION_TEST_LOG;
	rmSync(root, { recursive: true, force: true });
}
