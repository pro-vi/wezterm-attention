// Real Pi loader/runner and production extension, with synthetic session metadata.
// Launched by the Rust test, which owns the disposable socket, claim and writer.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createJiti } from "jiti";

const runtimeRoot = resolve(process.env.ATTENTION_PI_RUNTIME_ROOT || "node_modules/@earendil-works/pi-coding-agent");
const load = (name) => import(pathToFileURL(join(runtimeRoot, "dist", name)).href);
const { ExtensionRunner } = await load("core/extensions/runner.js");
const { createExtensionRuntime, loadExtensionFromFactory } = await load("core/extensions/loader.js");
const { createEventBus } = await load("core/event-bus.js");
const extensionModule = await createJiti(import.meta.url, {
  virtualModules: { "@earendil-works/pi-coding-agent": Object.freeze({}) },
}).import("../pi/index.ts");
const runtime = createExtensionRuntime();
const eventBus = createEventBus();
const cwd = process.env.WEZTERM_ATTENTION_DIR;
assert(cwd && cwd.includes("/wl-"), "Rust must supply disposable state");
const loaded = await loadExtensionFromFactory(extensionModule.default, cwd, eventBus, runtime);
const extension = loaded.extension ?? loaded;
assert(extension.handlers instanceof Map);
const cancelled = await loadExtensionFromFactory((pi) => {
  pi.on("session_before_compact", () => ({ cancel: true }));
}, cwd, eventBus, runtime);
const errors = [];
const runner = new ExtensionRunner([extension, cancelled.extension ?? cancelled], runtime, cwd, {
  getSessionId: () => "pi-runtime", getSessionFile: () => join(cwd, "synthetic-session.jsonl"),
}, {});
runner.onError((event) => errors.push({ event: event.event, error: event.error }));
for (const excluded of ["ui_prompt_start", "ui_prompt_end", "session_compact_failed", "tool_call", "agent_end"]) {
  assert.equal(extension.handlers.has(excluded), false, `${excluded} must not be registered`);
}
const sentinel = "SYNTHETIC-PRIVATE-SENTINEL";
for (const event of [
  { type: "session_start", reason: "startup" },
  { type: "input", source: "interactive", text: sentinel },
  { type: "agent_start" },
  { type: "tool_execution_start", toolName: "AskUserQuestion", toolCallId: "pi-tool", args: { text: sentinel } },
  { type: "tool_execution_end", toolName: "AskUserQuestion", toolCallId: "pi-tool", isError: true, result: sentinel },
  { type: "message_end", message: { role: "assistant", stopReason: "aborted", content: sentinel } },
  { type: "message_end", message: { role: "assistant", stopReason: "error", content: sentinel } },
  { type: "agent_settled" },
]) assert.equal(await runner.emit(event), undefined, "observer must return no provider decision");
assert.deepEqual(await runner.emit({ type: "session_before_compact", reason: "threshold", preparation: {}, branchEntries: [], willRetry: false, signal: new AbortController().signal }), { cancel: true });
assert.equal(await runner.emit({ type: "session_compact", reason: "manual", fromExtension: false, willRetry: false, compactionEntry: {} }), undefined);
await runner.emit({ type: "session_shutdown", reason: "reload" });
assert.deepEqual(errors, []);
console.log(JSON.stringify({ runtime_version: JSON.parse(readFileSync(join(runtimeRoot, "package.json"), "utf8")).version, events: 10, model_calls: 0 }));
