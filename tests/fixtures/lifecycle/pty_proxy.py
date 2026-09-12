#!/usr/bin/env python3
"""Connect a test-owned PTY to pipes; never attach to an existing user pane."""
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios

child, master = pty.fork()
if child == 0:
    os.execvpe(sys.argv[1], sys.argv[1:], os.environ)

fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
alive = True


def stop(_signal=None, _frame=None):
    if not alive:
        return
    try:
        if os.getpgid(child) == child:
            os.killpg(child, signal.SIGTERM)
        else:
            os.kill(child, signal.SIGTERM)
    except ProcessLookupError:
        pass


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
try:
    while True:
        ready, _, _ = select.select([master, sys.stdin.fileno()], [], [], 0.1)
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            os.write(sys.stdout.fileno(), data)
        if sys.stdin.fileno() in ready:
            data = os.read(sys.stdin.fileno(), 65536)
            if not data:
                stop()
                break
            os.write(master, data)
        ended, result = os.waitpid(child, os.WNOHANG)
        if ended:
            alive = False
            raise SystemExit(os.waitstatus_to_exitcode(result))
finally:
    stop()
    os.close(master)
    try:
        os.waitpid(child, 0)
    except ChildProcessError:
        pass
