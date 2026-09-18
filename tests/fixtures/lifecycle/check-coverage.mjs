import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

// What this proves: every registry case names a provider event the manifest
// actually declares, the registry's kinds are exactly the manifest's lifecycle
// variants, and the fixtures exercise all of them. That is the product.
//
// It used to also parse a planning document's coverage table and a review
// document's hook map through a Markdown tool at an absolute path in one
// developer's home directory, and assert the registry matched them. Those
// assertions described how the work was scheduled, not what the code does, and
// they made this checker -- and therefore the gate -- unrunnable anywhere else.

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const load = (path) => JSON.parse(readFileSync(join(root, path), "utf8"));
const registry = load("tests/fixtures/lifecycle/contact-cases.json");
const manifest = load("protocol/v2.json");
const fixtures = load("tests/fixtures/lifecycle/observations.json");
// Structural checks with continuing value. The id list used to be asserted
// literally, which kept an obligation whose explanation had been deleted; each
// deferred row now carries its own description and reason instead.
const ids = [...registry.rows, ...registry.deferred].map((row) => row.id);
assert.equal(new Set(ids).size, ids.length, "row ids must be unique across active and deferred");
for (const row of registry.deferred) {
  assert(row.description && row.reason, `${row.id}: a deferred row must say what it is and why it waits`);
}
const pi = registry.rows.filter((row) => row.cases.some((item) => item.provider === "pi")).length;
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
console.log(`Lifecycle coverage: ${registry.rows.length} active rows, ${registry.deferred.length} deferred, ${pi} Pi rows, ${count} production-writer cases, ${kinds.size} manifest kinds`);
