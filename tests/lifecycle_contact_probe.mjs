// Synthetic Codex contact only: fresh home, loopback backend, no real credentials.
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const state = process.env.WEZTERM_ATTENTION_DIR;
assert(state && state.includes("/wl-"), "Rust test must own the disposable state");
const home = join(dirname(state), "codex-contact");
mkdirSync(home, { recursive: true, mode: 0o700 });
const canonicalHome = realpathSync(home);
const cwd = join(canonicalHome, "workspace");
mkdirSync(cwd);
const codex = "/opt/homebrew/bin/codex";
const source = process.env.ATTENTION_CODEX_SOURCE || "/Users/provi/Development/_sources/codex";
const revision = "6b9826e3aa83b1a5947db50f4332cb9c65f1b340";
const version = execFileSync(codex, ["--version"], { encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: canonicalHome, CODEX_HOME: canonicalHome } }).trim();
assert.equal(version, "codex-cli 0.154.0", "native fixture is pinned to the inspected release");
const identityScript = `const fs=require("fs"), cp=require("child_process");
const phase=process.argv[1] || "before";
fs.writeSync(1,JSON.stringify({phase,pid:process.pid,ppid:process.ppid,executable:process.execPath,start:cp.execFileSync("/bin/ps",["-p",String(process.pid),"-o","lstart="],{encoding:"utf8",env:{PATH:"/usr/bin:/bin"}}).trim()})+"\\n");
if(phase==="before") process.execve(process.execPath,[process.execPath,"-e",process.argv[2],"after"],{PATH:"/usr/bin:/bin"});`;
const identities = execFileSync(process.execPath, ["-e", identityScript, "before", identityScript], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } }).trim().split("\n").map((line) => JSON.parse(line));
assert.equal(identities.length, 2);
for (const field of ["pid", "ppid", "executable", "start"]) assert.equal(identities[0][field], identities[1][field]);
assert.notEqual(identities[0].phase, identities[1].phase);
const catalog = JSON.parse(execFileSync("/usr/bin/git", ["-C", source, "show", `${revision}:codex-rs/models-manager/models.json`], { encoding: "utf8", maxBuffer: 4 * 1024 * 1024 }));
const model = catalog.models.find((item) => item.slug === "gpt-5.2");
assert(model);
model.tool_mode = "code_mode_only";
model.experimental_supported_tools = ["request_user_input_async"];
model.prefer_websockets = false;
model.base_instructions = "Synthetic local test. No external service is available.";
model.model_messages = null;
writeFileSync(join(canonicalHome, "models.json"), JSON.stringify({ models: [model] }));

const requests = [];
let unexpectedAuthorization = false;
const ui = process.env.ATTENTION_NATIVE_UI === "1";
const heldFinals = [];
let released = false;
const completed = (id) => ({ type: "response.completed", response: { id, usage: { input_tokens: 0, output_tokens: 0, total_tokens: 0 } } });
const sse = (events) => events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join("");
const server = createServer(async (request, response) => {
  if (Object.hasOwn(request.headers, "authorization")) { unexpectedAuthorization = true; response.writeHead(403); response.end(); return; }
  if (request.method !== "POST" || !request.url.endsWith("/responses")) { response.writeHead(404); response.end(); return; }
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  requests.push(JSON.parse(Buffer.concat(chunks).toString()));
  const index = requests.length;
  response.writeHead(200, { "content-type": "text/event-stream" });
  const events = [
    { type: "response.created", response: { id: `response-${index}` } },
    index === 1 ? { type: "response.output_item.done", item: { type: "function_call", call_id: "native-question-call", namespace: "functions", name: "request_user_input_async", arguments: JSON.stringify({ questions: [{ title: "Synthetic follow-up?", options: ["A", "B"] }] }) } }
      : { type: "response.output_item.done", item: { type: "message", role: "assistant", id: `message-${index}`, content: [{ type: "output_text", text: "Synthetic turn finished." }] } },
    completed(`response-${index}`),
  ];
  if (ui && index > 1 && !released) {
    response.write(sse(events.slice(0, 1)));
    heldFinals.push(() => { if (!response.destroyed) response.end(sse(events.slice(1))); });
  } else response.end(sse(events));
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const address = server.address();
assert(address && typeof address !== "string");
const quote = JSON.stringify;
const hooks = {};
let trust = "";
const sorted = (value) => Array.isArray(value) ? value.map(sorted) : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, sorted(value[key])])) : value;
for (const [event, key] of [["SessionStart", "session_start"], ["PreToolUse", "pre_tool_use"], ["PostToolUse", "post_tool_use"], ["Stop", "stop"], ["UserPromptSubmit", "user_prompt_submit"]]) {
  const command = `/usr/bin/python3 ${quote(join(root, "tests/provider_contact_hook.py"))} codex ${event}`;
  const handler = { type: "command", command, timeout: 5, async: false };
  const group = { hooks: [handler], ...(event.includes("ToolUse") ? { matcher: "request_user_input_async" } : {}) };
  hooks[event] = [group];
  const hash = "sha256:" + createHash("sha256").update(JSON.stringify(sorted({ event_name: key, ...group }))).digest("hex");
  trust += `\n[hooks.state.${quote(`${canonicalHome}/hooks.json:${key}:0:0`)}]\ntrusted_hash = ${quote(hash)}\n`;
}
writeFileSync(join(canonicalHome, "hooks.json"), JSON.stringify({ hooks }));
writeFileSync(join(canonicalHome, "config.toml"), `model = "gpt-5.2"\nmodel_provider = "fixture"\nmodel_catalog_json = ${quote(join(canonicalHome, "models.json"))}\ncli_auth_credentials_store = "ephemeral"\nmcp_oauth_credentials_store = "file"\napproval_policy = "never"\nsandbox_mode = "danger-full-access"\n[model_providers.fixture]\nname = "Local fixture"\nbase_url = "http://127.0.0.1:${address.port}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\n[features]\nhooks = true\n[projects.${quote(cwd)}]\ntrust_level = "trusted"\n${trust}`);
const log = join(canonicalHome, "hooks.jsonl");
const environment = { PATH: "/usr/bin:/bin:/opt/homebrew/bin", HOME: canonicalHome, CODEX_HOME: canonicalHome, TERM: "xterm-256color", WEZTERM_ATTENTION_CONTACT_LOG: log };
for (const key of ["WEZTERM_ATTENTION_DIR", "WEZTERM_ATTENTION_ROOT", "WEZTERM_UNIX_SOCKET", "WEZTERM_PANE", "WEZTERM_ATTENTION_LAUNCH_ID"]) if (process.env[key]) environment[key] = process.env[key];
let child;
try {
  if (ui) {
    const { Terminal } = createRequire(import.meta.url)(process.env.ATTENTION_XTERM_MODULE || "@xterm/headless");
    const terminal = new Terminal({ cols: 120, rows: 40, allowProposedApi: true, scrollback: 1000 });
    child = spawn("/usr/bin/python3", [join(root, "tests/fixtures/lifecycle/pty_proxy.py"), codex, "--no-alt-screen", "Synthetic local question probe"], { cwd, env: environment, stdio: ["pipe", "pipe", "pipe"] });
    let errors = "";
    child.stderr.on("data", (chunk) => { errors += chunk; });
    terminal.onData((data) => child.stdin.write(data));
    child.stdout.on("data", (chunk) => terminal.write(chunk));
    const screen = () => Array.from({ length: terminal.rows }, (_, row) => terminal.buffer.active.getLine(terminal.buffer.active.baseY + row)?.translateToString(true) || "").join("\n");
    const wait = async (predicate, label) => {
      const deadline = Date.now() + 120_000;
      while (!predicate()) {
        assert(!unexpectedAuthorization, "local fixture must not receive authorization headers");
        assert(child.exitCode === null, `Codex exited before ${label}: ${errors}`);
        assert(Date.now() < deadline, `timeout waiting for ${label}; synthetic screen:\n${screen()}`);
        await new Promise((done) => setTimeout(done, 25));
      }
    };
    await wait(() => heldFinals.length > 0 && screen().includes("? 1 question"), "pending question during a running turn");
    child.stdin.write("\x1b[200~separate queued input\x1b[201~");
    await wait(() => screen().includes("separate queued input"), "composer input");
    child.stdin.write("\t");
    await wait(() => screen().includes("Queued follow-up inputs") && screen().includes("? 1 question") && screen().includes("↳ separate queued input"), "pending question plus queued input in one viewport");
    released = true;
    for (const release of heldFinals) release();
    await wait(() => requests.some((request) => JSON.stringify(request.input).includes("separate queued input")), "queued input reaching the model after Stop while the question is unanswered");
    await wait(() => readFileSync(log, "utf8").split("\n").filter((line) => line.includes('"event":"Stop"')).length >= 2, "second Stop");
    const records = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    const post = records.find((record) => record.event === "PostToolUse");
    assert(post?.accepted_publication_receipt);
    const submissions = records.filter((record) => record.event === "UserPromptSubmit");
    assert(submissions.length >= 2 && submissions.every((record) => record.tool_use_id === undefined));
    const firstStop = records.findIndex((record) => record.event === "Stop");
    assert(records.indexOf(submissions[0]) < firstStop && records.indexOf(submissions[1]) > firstStop);
    assert(screen().includes("? 1 question"), "the queued message must not be mistaken for a question answer");
    console.log(JSON.stringify({ version, source_revision: revision, ui_question_and_queue: true, queue_drained: true, native_events: records.map((record) => record.event), session_id: post.session_id, metadata_identity_proof: "PID/parent/image/start do not distinguish exec", paid_model_calls: 0 }));
    child.stdin.end();
    await new Promise((done) => child.once("close", done));
    terminal.dispose();
  } else {
  child = spawn(codex, ["exec", "--skip-git-repo-check", "--json", "Synthetic local question probe"], { cwd, env: environment, stdio: ["ignore", "pipe", "pipe"] });
  let output = "", errors = "";
  child.stdout.on("data", (chunk) => { output += chunk; });
  child.stderr.on("data", (chunk) => { errors += chunk; });
  const deadline = setTimeout(() => child.kill("SIGTERM"), 120_000);
  const exit = await new Promise((done, fail) => { child.once("error", fail); child.once("close", done); });
  clearTimeout(deadline);
  assert.equal(exit, 0, errors.slice(-3000));
  assert(!unexpectedAuthorization, "local fixture must not receive authorization headers");
  const records = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
  const pre = records.find((record) => record.event === "PreToolUse");
  const post = records.find((record) => record.event === "PostToolUse");
  assert(pre && post, `native tool hooks missing; events=${records.map((record) => record.event)}; ${errors.slice(-1500)}; ${output.slice(-1000)}`);
  assert.equal(pre.tool_name, "request_user_input_async");
  assert.equal(post.tool_use_id, pre.tool_use_id);
  assert.equal(post.accepted_publication_receipt, true, JSON.stringify(post));
  assert(records.some((record) => record.event === "Stop"));
  assert.equal(requests.length, 2, "async question must return and allow a final response");
  console.log(JSON.stringify({ version, source_revision: revision, local_model_requests: requests.length, native_events: records.map((record) => record.event), receipt_shape: post.tool_response_shape, session_id: post.session_id, publication_call_id: post.tool_use_id, metadata_identity_proof: "PID/parent/image/start do not distinguish exec", paid_model_calls: 0 }));
  }
} finally {
  if (child && child.exitCode === null) child.kill("SIGTERM");
  server.closeAllConnections();
  await new Promise((done) => server.close(done));
}
