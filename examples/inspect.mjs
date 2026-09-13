#!/usr/bin/env node
// Bounded public discovery followed by exact address/launch/binding inspection.
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { bindings, runJson } from "./checkpoint.mjs";

export function inspectBindings({ attention, socket, run = runJson, limit = "100" }) {
  const discovery = bindings(attention, socket, run, limit);
  const panes = [];
  for (const row of discovery.current.values()) {
    const scope = { address: row.address, launch_id: row.launch_id, binding_id: row.binding_id };
    const response = run(attention, ["inspect", "--scope", "-", "--json"], JSON.stringify(scope));
    if (!response || response.schema !== 1 || response.command !== "inspect" || response.status !== "ok"
        || response.complete !== true || response.result?.scope_relation !== "matched") throw new Error("inspection degraded or scope changed; request a new snapshot explicitly");
    const actual = response.result.scope;
    if (!actual || actual.launch_id !== scope.launch_id || actual.binding_id !== scope.binding_id
        || actual.address?.realm_id !== scope.address.realm_id || actual.address?.incarnation_id !== scope.address.incarnation_id
        || actual.address?.pane_id !== scope.address.pane_id) throw new Error("inspection returned a different scope");
    panes.push(response.result);
  }
  return { schema: 1, scope: discovery.scope, panes };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [attention, socket, limit] = process.argv.slice(2);
  try {
    if (!attention || !socket) throw new Error("usage: inspect.mjs ATTENTION SOCKET [LIMIT]");
    console.log(JSON.stringify(inspectBindings({ attention, socket, limit })));
  } catch (error) { console.error(error instanceof Error ? error.message : "inspection failed"); process.exitCode = 1; }
}
