#!/usr/bin/env python3
"""Measure Python and Rust hook writers against disposable state and ptys."""

from __future__ import annotations

import concurrent.futures
import argparse
import hashlib
import json
import os
import pathlib
import pty
import socket
import statistics
import subprocess
import shutil
import tempfile
import time
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[2]
HOOK_COUNT = 100
CHILD_COUNT = 20


def percentile_95(values: list[float]) -> float:
    ordered = sorted(values)
    return ordered[max(0, (95 * len(ordered) + 99) // 100 - 1)]


def run(command: list[str], environment: dict[str, str], payload: dict[str, Any], stdin=None) -> float:
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        input=None if stdin is not None else json.dumps(payload),
        stdin=stdin,
        text=stdin is None,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        env=environment,
        check=False,
    )
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if completed.returncode != 0:
        diagnostic = completed.stderr.decode() if isinstance(completed.stderr, bytes) else completed.stderr
        raise RuntimeError(f"writer exited {completed.returncode}: {diagnostic.strip()}")
    return elapsed_ms


def normalized_record_path(state: pathlib.Path, path: pathlib.Path) -> str:
    parts = list(path.relative_to(state).parts)
    if len(parts) >= 3 and parts[:2] == ["v2", "realms"]:
        parts[2] = "<realm>"
    if len(parts) >= 5 and parts[3] == "incarnations":
        parts[4] = "<incarnation>"
    return "/".join(parts)


def state_footprint(state: pathlib.Path) -> dict[str, Any]:
    files = [path for path in state.rglob("*") if path.is_file() and ".lock" not in path.name]
    return {
        "files": len(files),
        "bytes": sum(path.stat().st_size for path in files),
        "_file_bytes": {normalized_record_path(state, path): path.stat().st_size for path in files},
    }


def artifact_sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def resolve_measurement_artifacts(
    python_writer: pathlib.Path, rust_binary: pathlib.Path
) -> tuple[list[str], dict[str, str]]:
    if not python_writer.is_file():
        raise ValueError(f"Python baseline does not exist: {python_writer}")
    if not rust_binary.is_file():
        raise ValueError(f"Rust candidate does not exist: {rust_binary}")
    python_identity = artifact_sha256(python_writer)
    rust_identity = artifact_sha256(rust_binary)
    if python_identity == rust_identity:
        raise ValueError("baseline and candidate have the same artifact identity")
    return [str(rust_binary)], {
        "python_sha256": python_identity,
        "rust_sha256": rust_identity,
    }


def stage_python_baseline(
    python_writer: pathlib.Path, protocol_manifest: pathlib.Path, scratch: pathlib.Path
) -> list[str]:
    if not protocol_manifest.is_file():
        raise ValueError(f"protocol manifest does not exist: {protocol_manifest}")
    root = scratch / "python-baseline"
    writer = root / "libexec" / "attention.py"
    manifest = root / "protocol" / "v2.json"
    writer.parent.mkdir(parents=True)
    manifest.parent.mkdir(parents=True)
    shutil.copy2(python_writer, writer)
    shutil.copy2(protocol_manifest, manifest)
    return [sys.executable, str(writer)]


def measure(implementation: str, command: list[str], scratch: pathlib.Path) -> dict[str, Any]:
    state = scratch / f"state-{implementation}"
    socket_path = scratch / "mux.sock"
    socket_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(socket_path))
    master, slave = pty.openpty()
    environment = os.environ.copy()
    environment.update(
        {
            "WEZTERM_ATTENTION_ROOT": str(ROOT),
            "WEZTERM_ATTENTION_DIR": str(state),
            "WEZTERM_UNIX_SOCKET": str(socket_path),
            "WEZTERM_PANE": "42",
            "WEZTERM_ATTENTION_LAUNCH_ID": "00000000-0000-4000-8000-000000000701",
        }
    )
    try:
        run([*command, "hooks", "claim"], environment, {}, stdin=slave)
        start = {
            "session_id": "measurement-session",
            "transcript_path": "/tmp/measurement-session.jsonl",
            "cwd": "/tmp/measurement-project",
            "hook_event_name": "SessionStart",
            "source": "startup",
            "model": "measurement-model",
        }
        run([*command, "hooks", "event", "claude", "SessionStart", "--strict"], environment, start)
        activity = {
            **start,
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
        }
        for _ in range(5):
            run([*command, "hooks", "event", "claude", "PreToolUse", "--strict"], environment, activity)
        sequential = [
            run([*command, "hooks", "event", "claude", "PreToolUse", "--strict"], environment, activity)
            for _ in range(HOOK_COUNT)
        ]

        def child(index: int) -> float:
            payload = {
                **activity,
                "agent_id": f"measurement-child-{index:02d}",
            }
            return run(
                [*command, "hooks", "event", "claude", "PreToolUse", "--strict"],
                environment,
                payload,
            )

        burst_started = time.perf_counter_ns()
        with concurrent.futures.ThreadPoolExecutor(max_workers=CHILD_COUNT) as executor:
            concurrent_durations = list(executor.map(child, range(CHILD_COUNT)))
        burst_wall_ms = (time.perf_counter_ns() - burst_started) / 1_000_000
        return {
            "sequential": {
                "count": HOOK_COUNT,
                "median_ms": round(statistics.median(sequential), 3),
                "p95_ms": round(percentile_95(sequential), 3),
                "max_ms": round(max(sequential), 3),
            },
            "concurrent_child_burst": {
                "count": CHILD_COUNT,
                "wall_ms": round(burst_wall_ms, 3),
                "p95_process_ms": round(percentile_95(concurrent_durations), 3),
                "max_process_ms": round(max(concurrent_durations), 3),
            },
            "state": state_footprint(state),
        }
    finally:
        os.close(master)
        os.close(slave)
        listener.close()
        socket_path.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--python-writer", type=pathlib.Path, required=True)
    parser.add_argument(
        "--rust-binary",
        type=pathlib.Path,
        default=ROOT / "libexec" / "attention-rs",
    )
    parser.add_argument(
        "--protocol-manifest",
        type=pathlib.Path,
        default=ROOT / "protocol" / "v2.json",
    )
    args = parser.parse_args()
    try:
        rust_command, identities = resolve_measurement_artifacts(
            args.python_writer.resolve(), args.rust_binary.resolve()
        )
    except ValueError as error:
        parser.error(str(error))
    with tempfile.TemporaryDirectory(prefix="attention-rust-measure-") as directory:
        scratch = pathlib.Path(directory)
        try:
            python_command = stage_python_baseline(
                args.python_writer.resolve(), args.protocol_manifest.resolve(), scratch
            )
        except ValueError as error:
            parser.error(str(error))
        python = measure("python", python_command, scratch)
        rust = measure("rust", rust_command, scratch)
        python_file_bytes = python["state"].pop("_file_bytes")
        rust_file_bytes = rust["state"].pop("_file_bytes")
        all_records = sorted(set(python_file_bytes) | set(rust_file_bytes))
        report = {
            "machine": "M5 Max, macOS, local disposable state",
            "implementation_identities": identities,
            "python": python,
            "rust": rust,
            "state_byte_deltas": [
                {
                    "record": record,
                    "python_bytes": python_file_bytes.get(record),
                    "rust_bytes": rust_file_bytes.get(record),
                    "delta": (rust_file_bytes.get(record) or 0) - (python_file_bytes.get(record) or 0),
                }
                for record in all_records
                if python_file_bytes.get(record) != rust_file_bytes.get(record)
            ],
        }
    report["pass_shape"] = {
        "rust_p95_at_or_below_python": (
            report["rust"]["sequential"]["p95_ms"] <= report["python"]["sequential"]["p95_ms"]
        ),
        "rust_burst_wall_at_or_below_python": (
            report["rust"]["concurrent_child_burst"]["wall_ms"]
            <= report["python"]["concurrent_child_burst"]["wall_ms"]
        ),
        "state_file_count_equal": report["rust"]["state"]["files"] == report["python"]["state"]["files"],
        "state_bytes_equal": report["rust"]["state"]["bytes"] == report["python"]["state"]["bytes"],
        "expected_updated_at_ms_delta_only": report["state_byte_deltas"] == [
            {"record": "42", "python_bytes": 133, "rust_bytes": 163, "delta": 30}
        ],
    }
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
