// Regression tests for pi/index.ts, driving the REAL registered command/event
// handlers through a mock ExtensionAPI. Named after the properties they lock —
// several are the negation of a bug fixed during review triage. Run: `bun test`.
import { test, expect } from "bun:test";
import { mkdtempSync, mkdirSync, existsSync, readFileSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ext from "../pi/index.ts";

type Note = { msg: string; level: string };

function loadExt() {
	let cmdHandler!: (args: string, ctx: { ui: { notify: (m: string, l: string) => void } }) => Promise<void>;
	let eventHandler!: (data: unknown) => void;
	const notes: Note[] = [];
	const pi = {
		events: { on: (_e: string, h: (d: unknown) => void) => { eventHandler = h; } },
		on: () => {},
		registerCommand: (_n: string, o: { handler: typeof cmdHandler }) => { cmdHandler = o.handler; },
	};
	// deno-lint-ignore no-explicit-any
	ext(pi as any);
	const ctx = { ui: { notify: (msg: string, level: string) => notes.push({ msg, level }) } };
	return { cmdHandler, eventHandler, notes, ctx };
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function freshDir(prefix: string): string {
	const d = mkdtempSync(join(tmpdir(), prefix));
	process.env.WEZTERM_ATTENTION_DIR = d;
	return d;
}

test("happy path: /attention busy writes a thinking marker with ttl_ms and source pi", async () => {
	const dir = freshDir("wez-happy-");
	process.env.WEZTERM_PANE = "42";
	const { cmdHandler, notes } = loadExt();
	await cmdHandler("busy label here", { ui: { notify: (m, l) => notes.push({ msg: m, level: l }) } });
	const m = JSON.parse(readFileSync(join(dir, "42"), "utf8"));
	expect(m.type).toBe("thinking");
	expect(m.source).toBe("pi");
	expect(typeof m.ttl_ms).toBe("number");
	expect(m.label).toBe("label here");
	// The message reports the resolved state (busy -> thinking), pre-existing behavior.
	expect(notes.at(-1)?.msg).toBe("Marked WezTerm pane as thinking");
	rmSync(dir, { recursive: true, force: true });
});

test("F1: last requested mutation wins — notify then clear leaves NO marker", async () => {
	const dir = freshDir("wez-order1-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	let present = 0;
	for (let i = 0; i < 30; i++) {
		eventHandler("notify");
		eventHandler("clear");
		await sleep(10);
		if (existsSync(join(dir, "42"))) present++;
	}
	expect(present).toBe(0);
	rmSync(dir, { recursive: true, force: true });
});

test("F1: ordering preserved — clear then notify leaves a notify marker", async () => {
	const dir = freshDir("wez-order2-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	writeFileSync(join(dir, "42"), "{}");
	eventHandler("clear");
	eventHandler("notify");
	await sleep(20);
	expect(existsSync(join(dir, "42"))).toBe(true);
	expect(JSON.parse(readFileSync(join(dir, "42"), "utf8")).type).toBe("notify");
	rmSync(dir, { recursive: true, force: true });
});

test("F3: filesystem failure reports io-error, NOT 'not set', when the pane IS set", async () => {
	const parent = mkdtempSync(join(tmpdir(), "wez-io-"));
	const dir = join(parent, "markers");
	mkdirSync(dir);
	chmodSync(dir, 0o500); // no write
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "42";
	const { cmdHandler, notes } = loadExt();
	await cmdHandler("notify", { ui: { notify: (m, l) => notes.push({ msg: m, level: l }) } });
	const msg = notes.at(-1)?.msg ?? "";
	expect(msg).toContain("I/O error");
	expect(msg).not.toContain("not set");
	chmodSync(dir, 0o700);
	rmSync(parent, { recursive: true, force: true });
});

test("F3: missing pane still reports 'not set' (unchanged truthful behavior)", async () => {
	freshDir("wez-missing-");
	delete process.env.WEZTERM_PANE;
	const { cmdHandler, notes } = loadExt();
	await cmdHandler("notify", { ui: { notify: (m, l) => notes.push({ msg: m, level: l }) } });
	expect(notes.at(-1)?.msg).toBe("WEZTERM_PANE is not set; nothing written");
});

test("F4: traversal pane id writes NOTHING outside the marker dir and is rejected", async () => {
	const parent = mkdtempSync(join(tmpdir(), "wez-trav1-"));
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { cmdHandler, notes } = loadExt();
	await cmdHandler("busy", { ui: { notify: (m, l) => notes.push({ msg: m, level: l }) } });
	expect(readFileSync(victim, "utf8")).toBe("IMPORTANT"); // untouched
	expect(notes.at(-1)?.msg).toContain("not a valid pane id");
	rmSync(parent, { recursive: true, force: true });
});

test("F4: /attention clear with a traversal pane id does NOT delete an outside file", async () => {
	const parent = mkdtempSync(join(tmpdir(), "wez-trav2-"));
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { cmdHandler } = loadExt();
	await cmdHandler("clear", { ui: { notify: () => {} } });
	expect(existsSync(victim)).toBe(true); // not deleted
	rmSync(parent, { recursive: true, force: true });
});
