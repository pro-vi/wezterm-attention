#!/usr/bin/env bun
//
// Minimal example: write a WezTerm attention marker from a CLI hook.
// Adapt this for any tool that wants to signal "I'm done" or "look at me"
// to the WezTerm tab bar.
//
// Protocol:
//   Write {"type":"<state>"} to ~/.local/state/wezterm-attention/<WEZTERM_PANE>
//   Valid types: "thinking", "stop", "notify", "review"
//   Optional: {"type":"thinking","frame":0} for animated spinner (0-3)
//
// The WEZTERM_PANE env var is set automatically by WezTerm for every shell.

import { mkdir, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";

type AttentionType = "thinking" | "stop" | "notify" | "review";

async function writeMarker(type: AttentionType, frame?: number): Promise<void> {
  const paneId = process.env.WEZTERM_PANE;
  const home = process.env.HOME;
  if (!paneId || !home) return;

  const dir = join(home, ".local", "state", "wezterm-attention");
  await mkdir(dir, { recursive: true });

  const data: Record<string, unknown> = { type };
  if (frame !== undefined) data.frame = frame;

  // Atomic write: tmp file + rename avoids partial reads
  const file = join(dir, paneId);
  await writeFile(file + ".tmp", JSON.stringify(data));
  await rename(file + ".tmp", file);
}

// Example: signal that work is done
await writeMarker("stop");
