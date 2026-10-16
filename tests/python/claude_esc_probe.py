"""Opt-in live check that Esc in a real Claude Code ends the turn on Attention's records.

Runs Claude Code (Sonnet) in a new WezTerm tab, in a folder under /Users/Shared, with only Attention's hooks registered
(project settings) and pointed at a copy of the `attention` this checkout built, writing to a scratch state root. It
drives six turns: a normal turn, Esc during a foreground command, Esc before any output, Esc at an approval prompt, Esc
while a background command runs, and Esc while a foreground sub-agent works. After each Esc it waits for Attention to
record the end. Then it ends Claude Code, closes the tab, and deletes the scratch state root, the settings it wrote, and
every file under the Claude Code configuration directory whose name holds the session's id.

It spends API calls on the account of the Claude Code it runs, and opens a tab in the WezTerm window it runs from.
tests/gate.sh runs it only with ATTENTION_CLAUDE_ESC_PROBE set. ATTENTION_TEST_CLAUDE names the claude to run,
ATTENTION_TEST_BINARY the attention to copy, and ATTENTION_ESC_PROBE_LOG a file for the log.

Exits 0 when each of the four Esc cases cleared the activity within 2 s of the Esc with one `user_interrupt` from
`session_status`, and the normal turn ended with `Stop` and no interrupt. The sub-agent case is reported as observed.
"""
import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DIR = Path("/Users/Shared/attention-demo-esc-probe")
# Claude Code 2.1.300 asks before a command that writes outside the project
# folder; the Esc at that prompt rejects it, so nothing is made there.
OUTSIDE = Path(f"/Users/Shared/attention-esc-probe-outside-{os.getpid()}")
CONFIG = Path.home() / ".claude"
CLEARED_WITHIN = 2.0

t0 = time.time()
log_file = open(os.environ["ATTENTION_ESC_PROBE_LOG"], "w") if os.environ.get("ATTENTION_ESC_PROBE_LOG") else None


def log(*parts):
    line = " ".join(str(part) for part in (f"{time.time() - t0:7.2f}",) + parts)
    print(line, flush=True)
    if log_file:
        log_file.write(line + "\n")
        log_file.flush()


def run(*args, **kwargs):
    return subprocess.run(list(args), check=True, capture_output=True, text=True, **kwargs).stdout


claude = os.environ.get("ATTENTION_TEST_CLAUDE") or shutil.which("claude")
wezterm = shutil.which("wezterm")
built = Path(os.environ.get("ATTENTION_TEST_BINARY") or ROOT / "target/debug/attention")
if not claude or not wezterm or not built.is_file():
    sys.exit(f"claude_esc_probe: needs claude ({claude}), wezterm ({wezterm}) and a built attention ({built})")
if not DIR.is_dir() or any(DIR.iterdir()):
    sys.exit(f"claude_esc_probe: {DIR} must exist and be empty")

scratch = Path(tempfile.mkdtemp(prefix="attention-esc-probe-"))
state = scratch / "state"
binary = scratch / "bin" / "attention"
binary.parent.mkdir()
shutil.copy2(built, binary)
log("attention", run(str(binary), "--version").strip(), "| claude", run(claude, "--version").strip())

# Every hook Attention registers, as the README gives them, plus a Stop hook that
# sleeps, so the file reads no turn only well after Attention recorded Stop.
described = json.loads(run(str(binary), "hooks", "describe", "--provider", "claude"))["result"]
hooks = {}
for spec in described["native_hooks"]:
    if spec["registration"] != "register":
        continue
    command = f"WEZTERM_ATTENTION_HOST_PID=$PPID exec {binary} " + " ".join(spec["arguments"])
    hooks.setdefault(spec["native_event"], []).append({"hooks": [{"type": "command", "command": command}]})
hooks["Stop"].append({"hooks": [{"type": "command", "command": "sleep 3"}]})
(DIR / ".claude").mkdir()
(DIR / ".claude" / "settings.json").write_text(json.dumps({"hooks": hooks}, indent=2))

pane = None
session = None


def send(text):
    run(wezterm, "cli", "send-text", "--pane-id", pane, "--no-paste", text)


def screen():
    return subprocess.run([wezterm, "cli", "get-text", "--pane-id", pane], capture_output=True, text=True).stdout


def status_file():
    best = None
    for path in glob.glob(str(CONFIG / "sessions" / "*.json")):
        try:
            data = json.load(open(path))
        except (OSError, ValueError):
            continue
        if data.get("cwd") == str(DIR) and data.get("startedAt", 0) / 1000 > t0:
            if best is None or data["startedAt"] > best["startedAt"]:
                best = data
    return best


def binding_dir():
    for path in glob.glob(str(state / "v2/realms/*/incarnations/*/panes/*/launches/*/bindings/*/binding.json")):
        try:
            data = json.load(open(path))
        except (OSError, ValueError):
            continue
        if data.get("provider") == "claude" and data.get("provider_session_id") == session:
            return Path(path).parent
    return None


def read(path):
    try:
        return json.load(open(path))
    except (OSError, ValueError):
        return None


def records():
    """What Attention shows and recorded: the shown activity type, or None once cleared, and the lead's observations."""
    binding = binding_dir()
    if binding is None:
        return None, []
    activity, clear = read(binding / "activity.json"), read(binding / "activity-clear.json")
    shown = None
    if activity and not (clear and activity["observed_mono_ns"] <= clear["observed_mono_ns"]):
        shown = activity["type"]
    snapshot = read(binding / "lifecycle.json") or {}
    observations = [
        item
        for pool in snapshot.get("pools", {}).values()
        for item in pool.get("observations", [])
        if item.get("actor", {}).get("kind") == "lead"
    ]
    return shown, sorted(observations, key=lambda item: (item["observed_mono_ns"], item["observation_id"]))


def interrupts():
    return [item for item in records()[1] if item["kind"] == "user_interrupt"]


def file_state():
    data = status_file() or {}
    return data.get("status"), data.get("waitingFor")


def wait(label, seconds, until, every=0.1):
    end, last = time.time() + seconds, None
    while time.time() < end:
        now = (file_state(), records()[0])
        if now != last:
            log(label, "file", now[0], "| shown", now[1])
            last = now
        if until(now):
            return True
        time.sleep(every)
    log(label, "gave up waiting")
    return False


def prompt(text):
    send("\x15")
    time.sleep(0.3)
    send(text)
    time.sleep(0.8)
    send("\r")


results = {}


def turn_over(now):
    """No turn works and the tab shows no turn either: a prompt typed now starts a turn of its own."""
    return now[0][0] in ("idle", "shell") and now[1] != "thinking"


def esc_case(name, text, ready, settle=0.0, expect_cleared=True):
    log("==", name)
    wait(name, 60, turn_over)
    before = len(interrupts())
    prompt(text)
    if not wait(name, 90, ready):
        results[name] = {"ok": False, "why": "never reached the state to press Esc in"}
        return
    time.sleep(settle)
    log(name, "pressing Esc")
    pressed = time.time()
    send("\x1b")
    cleared = None
    while time.time() - pressed < 10:
        shown, _ = records()
        found = interrupts()
        if shown is None and len(found) > before:
            cleared = time.time() - pressed
            break
        time.sleep(0.05)
    found = interrupts()[before:]
    log(name, "file", file_state(), "| shown", records()[0], "| new interrupts", [i["source_event"] for i in found],
        "| cleared after", None if cleared is None else round(cleared, 2))
    ok = (cleared is not None and cleared <= CLEARED_WITHIN and len(found) == 1
          and found[0]["source_event"] == "session_status")
    results[name] = {"ok": ok if expect_cleared else None, "cleared_after_s": cleared,
                     "interrupts": [i["source_event"] for i in found], "file": file_state()}
    wait(name, 30, lambda now: now[0][0] in ("idle", "shell"))
    time.sleep(2)


def is_working(kind):
    return lambda now: now[0][0] == kind and now[1] in ("thinking", "notify")


# A Claude Code that runs this probe passes its own CLAUDE* variables on. As of
# Claude Code 2.1.300 one started with them runs as a child session, which
# writes no status file and keeps no transcript.
inherited = sorted(name for name in os.environ if name.startswith("CLAUDE"))
unset = [argument for name in sorted(set(inherited) | {"CLAUDE_CONFIG_DIR"}) for argument in ("-u", name)]

try:
    pane = run(wezterm, "cli", "spawn", "--cwd", str(DIR), "--", "env", *unset,
               f"WEZTERM_ATTENTION_DIR={state}", f"PATH={os.environ.get('PATH', '/usr/bin:/bin')}",
               claude, "--setting-sources", "project", "--model", "sonnet", "--permission-mode", "default",
               "--allowedTools", "Bash(python3:*)").strip()
    log("spawned pane", pane)
    for _ in range(300):
        found = status_file()
        if found:
            session = found["sessionId"]
            break
        time.sleep(0.1)
    else:
        log("no status file after 30 s; screen follows")
        print(screen()[-3000:])
        raise SystemExit(1)
    log("session", session, "pid", found["pid"], "version", found.get("version"))
    time.sleep(4)

    log("== normal turn")
    before = len(interrupts())
    prompt('Use the Bash tool to run exactly: python3 -c "import time; time.sleep(5)" and then reply done.')
    # The prompt's hook first, then the file reading no turn once every Stop hook has run.
    wait("normal", 30, lambda now: now[1] == "thinking")
    wait("normal", 90, lambda now: now[0][0] == "idle" and now[1] == "stop")
    time.sleep(2)
    ends = [i for i in records()[1] if i.get("turn_end")]
    last = ends[-1] if ends else {}
    results["normal turn"] = {
        "ok": last.get("source_event") == "Stop" and len(interrupts()) == before,
        "last_turn_end": [last.get("kind"), last.get("source_event")],
        "interrupts": len(interrupts()) - before,
    }
    log("normal", results["normal turn"])
    time.sleep(2)

    esc_case("Esc during a foreground command",
             'Use the Bash tool to run exactly: python3 -c "import time; time.sleep(40)" and then reply done.',
             is_working("busy"), settle=6)
    esc_case("Esc before any output", "Write a 1500-word story about a cartographer, in plain prose.",
             lambda now: now[0][0] == "busy" and now[1] == "thinking")
    esc_case("Esc at an approval prompt",
             f"Use the Bash tool to run exactly: mkdir -p {OUTSIDE} and then reply done.",
             lambda now: now[0][0] == "waiting" and now[1] == "notify", settle=2)
    esc_case("Esc while a foreground sub-agent works",
             "Use the Agent tool to start one general-purpose sub-agent in the foreground, not in the background, "
             'with this task: "Use the Bash tool to run exactly: python3 -c \\"import time; time.sleep(40)\\" and '
             'then reply done." Wait for it, then reply with its answer.',
             is_working("busy"), settle=10, expect_cleared=False)
    # Last: the background command's end can start a turn of its own.
    esc_case("Esc while a background command runs",
             'Use the Bash tool with run_in_background set to true to run exactly: python3 -c "import time; '
             'time.sleep(60)". Then use the Bash tool, not in the background, to run exactly: python3 -c "import time; '
             'time.sleep(40)" and then reply done.',
             is_working("busy"), settle=8)

    log("== /exit")
    send("\x15")
    time.sleep(0.3)
    send("/exit")
    time.sleep(0.5)
    send("\r")
    time.sleep(3)
finally:
    if pane:
        subprocess.run([wezterm, "cli", "kill-pane", "--pane-id", pane], capture_output=True)
        log("closed pane", pane)
    if session:
        # Only real directories of the configuration directory, never through a
        # link, and only names that hold this session's id.
        made = []
        for top in os.scandir(CONFIG):
            if not top.is_dir(follow_symlinks=False):
                continue
            for parent, directories, files in os.walk(top.path):
                made += [os.path.join(parent, name) for name in directories + files if session in name]
                directories[:] = [name for name in directories if session not in name]
        for path in made:
            if os.path.isdir(path) and not os.path.islink(path):
                shutil.rmtree(path)
            elif os.path.lexists(path):
                os.remove(path)
            log("deleted", path)
    if OUTSIDE.is_dir() and not any(OUTSIDE.iterdir()):
        OUTSIDE.rmdir()
        log("deleted", OUTSIDE)
    shutil.rmtree(DIR / ".claude", ignore_errors=True)
    shutil.rmtree(scratch, ignore_errors=True)
    log("deleted", DIR / ".claude", "and", scratch)

print(json.dumps(results, indent=2))
if log_file:
    log_file.write(json.dumps(results, indent=2) + "\n")
failed = [name for name, result in results.items() if result["ok"] is False]
if failed or len(results) < 6:
    sys.exit(f"claude_esc_probe: not as expected: {failed or 'a case did not run'}")
