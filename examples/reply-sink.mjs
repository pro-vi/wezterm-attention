#!/usr/bin/env node
// Application-owned example. Select a destination outside Attention state.
import { openSync, closeSync, readFileSync, writeFileSync, renameSync, rmSync } from "node:fs";
import { isAbsolute } from "node:path";
import { randomUUID } from "node:crypto";

try {
  const file = process.env.ATTENTION_REPLY_FILE;
  if (!file || !isAbsolute(file)) throw new Error("ATTENTION_REPLY_FILE must be absolute");
  const delivery = JSON.parse(readFileSync(0, "utf8"));
  if (!delivery || delivery.schema !== 1 || typeof delivery.delivery_id !== "string"
      || typeof delivery.scope?.address?.realm_id !== "string" || typeof delivery.scope?.address?.incarnation_id !== "string"
      || typeof delivery.scope?.address?.pane_id !== "string" || typeof delivery.scope?.launch_id !== "string"
      || delivery.scope?.target?.kind !== "binding" || typeof delivery.scope.target.binding_id !== "string"
      || typeof delivery.provider !== "string" || typeof delivery.provider_session_id !== "string") throw new Error("unsupported delivery");
  if (delivery.reply?.availability === "available") {
    if (typeof delivery.reply.text !== "string") throw new Error("invalid reply content");
    const temporary = `${file}.${randomUUID()}.tmp`;
    const descriptor = openSync(temporary, "wx", 0o600);
    try {
      try {
        writeFileSync(descriptor, JSON.stringify({ schema: 1, delivery_id: delivery.delivery_id, scope: delivery.scope,
          provider: delivery.provider, provider_session_id: delivery.provider_session_id, text: delivery.reply.text }) + "\n");
      } finally { closeSync(descriptor); }
      renameSync(temporary, file);
    } finally { rmSync(temporary, { force: true }); }
  } else if (!["not_requested", "absent", "unsupported", "invalid", "too_large"].includes(delivery.reply?.availability)
      || Object.hasOwn(delivery.reply, "text")) throw new Error("unsupported reply availability");
} catch { console.error("reply sink rejected the delivery or could not write its destination"); process.exitCode = 1; }
