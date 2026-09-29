#!/usr/bin/env python3
"""Opt-in contact check: what Claude Code sends that the sub-agent count reads.

The count ends a sub-agent whose lead `Stop` no longer lists it in
`background_tasks`. That rests on three things Claude Code does, checked here
against the installed `claude` as of 2.1.284:

- a lead `Stop` carries `background_tasks`, and names each sub-agent in flight
  by the `agent_id` its `SubagentStart` carried;
- once a sub-agent has ended, the lead's next `Stop` does not name it;
- a sub-agent the API ends sends `StopFailure` with its own `agent_id`, no
  `SubagentStop`, and is not named by a later lead `Stop`.

It runs two headless sessions, so it spends API calls; the gate runs it only
when ATTENTION_CLAUDE_CONTACT is set. A session in which the model did not start
the sub-agent proves nothing, and is reported as inconclusive rather than passed.
"""

from __future__ import annotations

import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
HOOK = ROOT / "tests" / "python" / "provider_contact_hook.py"
EVENTS = ("SubagentStart", "SubagentStop", "Stop", "StopFailure")
TIMEOUT_SECONDS = 240

QUIET_AND_QUICK = (
    "Use the Agent tool twice, both with run_in_background true. Agent one: run the "
    "shell command 'sleep 25' and then reply DONE-A. Agent two: reply DONE-B without "
    "running any command. Then wait until both have reported and reply with one line: both done."
)
DOOMED = (
    "Use the Agent tool once with subagent_type doomed and run_in_background true. "
    "Then wait until it reports and reply with one line saying what happened."
)
# A model id that does not exist makes the sub-agent fail on its first request.
DOOMED_AGENT = json.dumps(
    {
        "doomed": {
            "description": "Test agent that always fails",
            "prompt": "Run the shell command sleep 5, then reply hello.",
            "model": "claude-does-not-exist-9",
        }
    }
)


class Inconclusive(Exception):
    pass


def run_session(claude: str, scratch: pathlib.Path, name: str, prompt: str, extra: list[str]) -> list[dict]:
    log = scratch / f"{name}.jsonl"
    hooks = {
        event: [{"hooks": [{"type": "command", "command": f"python3 {shlex.quote(str(HOOK))} claude {event}"}]}]
        for event in EVENTS
    }
    settings = scratch / f"{name}.settings.json"
    settings.write_text(json.dumps({"hooks": hooks}), encoding="utf-8")
    # No WezTerm variables: the hook then records what it received and forwards
    # nothing to a real attention state directory.
    env = {key: value for key, value in os.environ.items() if not key.startswith("WEZTERM_")}
    env["WEZTERM_ATTENTION_CONTACT_LOG"] = str(log)
    command = [
        claude, "-p", "--model", "sonnet", "--setting-sources", "project", "--settings", str(settings),
        "--allowedTools", "Agent", "Bash", *extra, "--output-format", "json", prompt,
    ]
    finished = subprocess.run(
        command, cwd=scratch, env=env, capture_output=True, text=True, timeout=TIMEOUT_SECONDS, check=False
    )
    if finished.returncode != 0:
        raise SystemExit(f"claude exited {finished.returncode} in {name}: {finished.stderr[-400:]}")
    if not log.exists():
        raise Inconclusive(f"{name}: no hook ran")
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


def subagent_ids(tasks: list[dict]) -> set[str]:
    return {task["id"] for task in tasks if task.get("type") == "subagent" and "id" in task}


def check_listing(records: list[dict], failures: list[str]) -> None:
    started = {r["agent_id"] for r in records if r["event"] == "SubagentStart" and "agent_id" in r}
    stops = [r for r in records if r["event"] == "Stop"]
    if not started or not stops:
        raise Inconclusive("listing: the model started no sub-agent")
    if not all("background_tasks" in r for r in stops):
        failures.append("a lead Stop carried no background_tasks")
        return
    listed = set().union(*(subagent_ids(r["background_tasks"]) for r in stops))
    if not listed:
        raise Inconclusive("listing: every sub-agent ended before the first lead Stop")
    if not listed <= started:
        failures.append(f"Stop names {sorted(listed - started)} that no SubagentStart carried as agent_id")
    if subagent_ids(stops[-1]["background_tasks"]):
        failures.append("the last lead Stop still names a sub-agent")


def check_failure(records: list[dict], failures: list[str]) -> None:
    doomed = [r for r in records if r["event"] == "SubagentStart" and r.get("agent_type") == "doomed"]
    if not doomed:
        raise Inconclusive("failure: the model did not start the failing sub-agent")
    agent = doomed[0]["agent_id"]
    failed_at = next(
        (index for index, r in enumerate(records) if r["event"] == "StopFailure" and r.get("agent_id") == agent), None
    )
    if failed_at is None:
        failures.append("the failing sub-agent sent no StopFailure carrying its agent_id")
        return
    if any(r["event"] == "SubagentStop" and r.get("agent_id") == agent for r in records):
        failures.append("the failing sub-agent sent a SubagentStop")
    later = [r for r in records[failed_at:] if r["event"] == "Stop"]
    if not later:
        raise Inconclusive("failure: no lead Stop followed the failure")
    if any(agent in subagent_ids(r.get("background_tasks", [])) for r in later):
        failures.append("a lead Stop after the failure still names the sub-agent")


def main() -> int:
    claude = os.environ.get("ATTENTION_TEST_CLAUDE") or shutil.which("claude")
    if not claude:
        print("claude contact: no claude on PATH (set ATTENTION_TEST_CLAUDE)", file=sys.stderr)
        return 1
    version = subprocess.run([claude, "--version"], capture_output=True, text=True, check=False).stdout.strip()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="attention-claude-contact.") as scratch_name:
        scratch = pathlib.Path(scratch_name)
        try:
            check_listing(run_session(claude, scratch, "listing", QUIET_AND_QUICK, []), failures)
            check_failure(run_session(claude, scratch, "failure", DOOMED, ["--agents", DOOMED_AGENT]), failures)
        except Inconclusive as reason:
            print(f"claude contact: INCONCLUSIVE on {version}: {reason}", file=sys.stderr)
            return 1
    for failure in failures:
        print(f"claude contact: FAILED on {version}: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"claude contact: ok on {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
