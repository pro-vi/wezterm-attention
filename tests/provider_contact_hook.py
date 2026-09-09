#!/usr/bin/env python3
"""Disposable provider-contact forwarder that records identity fields only."""

from __future__ import annotations

import fcntl
import json
import os
import pathlib
import subprocess
import sys


MAX_BYTES = 1_048_576
SAFE_FIELDS = (
    "hook_event_name",
    "session_id",
    "agent_id",
    "agent_type",
    "source",
    "notification_type",
    "reason",
    "tool_name",
    "stop_hook_active",
)


def main() -> int:
    if len(sys.argv) != 3:
        return 0
    provider, event_name = sys.argv[1:]
    payload_bytes = sys.stdin.buffer.read(MAX_BYTES + 1)
    if len(payload_bytes) > MAX_BYTES:
        return 0
    try:
        payload = json.loads(payload_bytes)
    except (json.JSONDecodeError, UnicodeDecodeError):
        payload = None
    contact_log = os.environ.get("WEZTERM_ATTENTION_CONTACT_LOG")
    if contact_log:
        record = {"provider": provider, "event": event_name}
        for environment_field in ("CODEX_THREAD_ID", "CODEX_SESSION_ID"):
            environment_value = os.environ.get(environment_field)
            if environment_value:
                record[environment_field.lower()] = environment_value
        if isinstance(payload, dict):
            for field in SAFE_FIELDS:
                value = payload.get(field)
                if isinstance(value, (str, bool)):
                    record[field] = value
        log_path = pathlib.Path(contact_log)
        log_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        with log_path.open("a", encoding="utf-8") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            handle.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    root = os.environ.get("WEZTERM_ATTENTION_ROOT")
    if not root:
        return 0
    try:
        subprocess.run(
            [str(pathlib.Path(root) / "bin" / "attention"), "hooks", "event", provider, event_name],
            input=payload_bytes,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
