#!/usr/bin/env python3
"""Drive an interactive shell through a pseudo-terminal, one line per prompt.

Usage: interactive_shell.py PROMPT SECONDS SHELL [ARG...] < lines

Prompt hooks, DEBUG traps and bash-preexec only behave as they do for a person
when the shell is interactive and its output is a terminal, so a pipe into
`bash -i` does not exercise them. Each input line is typed only after the shell
has printed PROMPT again, which is what a person at the keyboard does; typing
ahead lets readline discard the input while it sets up the terminal.

Prints the session output with carriage returns removed and exits with the
shell's status: 128+N when a signal N ended it, 124 when SECONDS ran out.
"""

import os
import pty
import select
import signal
import sys
import time


def drain(fd):
    """Read what the shell wrote before it exited."""
    data = b""
    while select.select([fd], [], [], 0.1)[0]:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        data += chunk
    return data


def main():
    prompt = sys.argv[1].encode()
    deadline = time.monotonic() + float(sys.argv[2])
    command = sys.argv[3:]
    lines = [line.encode() + b"\n" for line in sys.stdin.read().splitlines()]

    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(command[0], command)

    output = b""
    prompts_seen = 0
    sent = 0
    status = None
    while status is None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            sys.stdout.write(output.decode(errors="replace").replace("\r", ""))
            sys.stderr.write("interactive_shell: timed out\n")
            return 124
        ready, _, _ = select.select([fd], [], [], min(remaining, 0.2))
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                chunk = b""
            output += chunk
            if not chunk:
                _, raw = os.waitpid(pid, 0)
                status = raw
                break
        count = output.count(prompt)
        if count > prompts_seen:
            prompts_seen = count
            if sent < len(lines):
                os.write(fd, lines[sent])
                sent += 1
        finished, raw = os.waitpid(pid, os.WNOHANG)
        if finished:
            status = raw
            output += drain(fd)
    sys.stdout.write(output.decode(errors="replace").replace("\r", ""))
    if os.WIFSIGNALED(status):
        return 128 + os.WTERMSIG(status)
    return os.WEXITSTATUS(status)


if __name__ == "__main__":
    sys.exit(main())
