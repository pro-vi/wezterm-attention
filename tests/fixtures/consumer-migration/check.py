#!/usr/bin/env python3
"""Exercise the adapted bootstrap consumer copies without installing them."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tempfile
from pathlib import Path


HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]


def load_bridge_reader():
    spec = importlib.util.spec_from_file_location("consumer_bridge_reader", HERE / "bridge_reader.py")
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load bridge reader fixture")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check_bridge_reader(scratch: Path) -> None:
    marker_path = scratch / "42"
    marker_path.write_text(
        json.dumps({"type": "thinking", "source": "pi", "updated_at": 10, "updated_at_ms": 10_000}),
        encoding="utf-8",
    )
    marker = load_bridge_reader().marker_for(marker_path, now_s=15)
    assert marker is not None
    assert marker["age_s"] == 5


def check_hook_copies(scratch: Path) -> None:
    root = scratch / "checkout"
    binary = root / "bin" / "attention"
    log = scratch / "hook-calls.jsonl"
    binary.parent.mkdir(parents=True)
    binary.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, sys\n"
        "with open(os.environ['WEZTERM_ATTENTION_TEST_LOG'], 'a', encoding='utf-8') as stream:\n"
        "    stream.write(json.dumps({'argv': sys.argv[1:], 'stdin': sys.stdin.read()}) + '\\n')\n",
        encoding="utf-8",
    )
    binary.chmod(0o755)
    environment = os.environ.copy()
    environment["WEZTERM_ATTENTION_ROOT"] = str(root)
    environment["WEZTERM_ATTENTION_TEST_LOG"] = str(log)
    payload = '{"session_id":"fixture-session","cwd":"/tmp/project"}'
    for name in ("claude-stop.sh", "codex-stop.sh"):
        subprocess.run(
            ["sh", str(HERE / name)],
            input=payload,
            text=True,
            env=environment,
            check=True,
        )
    calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
    assert calls == [
        {"argv": ["hooks", "event", "claude", "Stop"], "stdin": payload},
        {"argv": ["hooks", "event", "codex", "Stop"], "stdin": payload},
    ]


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="attention-consumer-migration-") as directory:
        scratch = Path(directory)
        check_bridge_reader(scratch)
        check_hook_copies(scratch)
    subprocess.run(
        ["luajit", str(HERE / "check.lua"), str(HERE / "detect_agent.lua")],
        cwd=REPO,
        check=True,
    )
    print("ok - migrated bridge and provider hooks consume the v2 contracts")


if __name__ == "__main__":
    main()
