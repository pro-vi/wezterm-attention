#!/usr/bin/env bun
// Direct custom marker example. Provider hooks should instead forward their
// original stdin to `attention hooks event <provider> <event>`.

import { isAbsolute, join } from "node:path";

const root = process.env.WEZTERM_ATTENTION_ROOT;
if (!root || !isAbsolute(root)) {
  console.error("wezterm-attention: WEZTERM_ATTENTION_ROOT must be an absolute checkout path");
  process.exit(3);
}

const child = Bun.spawn({
  cmd: [join(root, "bin", "attention"), "mark", "stop", "--source", "example"],
  stdin: "ignore",
  stdout: "inherit",
  stderr: "inherit",
});
process.exit(await child.exited);
