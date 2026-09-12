import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const load = (path) => JSON.parse(readFileSync(join(root, path), "utf8"));
const registry = load("tests/fixtures/lifecycle/contact-cases.json");
const manifest = load("protocol/v2.json");
const fixtures = load("tests/fixtures/lifecycle/observations.json");
const readSection = (file, headings) => JSON.parse(execFileSync("/Users/provi/.local/bin/md", ["read", join(root, file), "--address", JSON.stringify({kind:"section",path:headings.map((text)=>({text,occurrence:1}))})], {encoding:"utf8", maxBuffer:1024*1024})).markdown;
const plan = readSection("docs/plans/2026-09-09-001-feat-lifecycle-observation-facts-plan.md", ["Lifecycle observations for Attention consumers", "Approved row coverage"]);
const plannedIds = [...plan.matchAll(/^\| (H\d+) \|/gm)].map((match) => match[1]);
assert.deepEqual(registry.rows.map((row) => row.id), plannedIds);
assert.equal(plannedIds.length, 18);
const historical = readSection("docs/reviews/2026-09-08-attention-hook-map.md", ["Attention v2: lifecycle hook map"]);
const historicalNext = historical.split("\n").filter((line) => line.startsWith("|") && line.includes("**Next:**")).map((line) => line.split("|")[1].trim());
assert.deepEqual(new Set([...registry.rows, ...registry.deferred].map((row) => row.historical_label)), new Set(historicalNext));
assert.deepEqual(registry.deferred.map((row) => row.id), ["H13", "H14", "H21"]);
assert.equal(registry.rows.filter((row) => row.cases.some((item) => item.provider === "pi")).length, 9);
const kinds = new Set();
let count = 0;
for (const row of registry.rows) {
  assert(row.cases.length);
  for (const item of row.cases) {
    assert(manifest.lifecycle_sources[item.provider][item.kind].includes(item.event), `${row.id}: ${item.provider}/${item.event}`);
    kinds.add(item.kind); count++;
  }
}
assert.deepEqual(kinds, new Set(Object.keys(manifest.lifecycle_variants)));
assert.equal(kinds.size, 14);
const fixtureKinds = new Set(fixtures.cases.filter((item) => item.expected === "valid").flatMap((item) => Object.values(item.value.pools).flatMap((pool) => pool.observations.map((observation) => observation.kind))));
assert.deepEqual(kinds, fixtureKinds);
console.log(`Lifecycle coverage: 18 active rows, 9 Pi rows, ${count} production-writer cases, 14 manifest kinds`);
