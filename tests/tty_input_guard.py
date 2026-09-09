#!/usr/bin/env python3
"""Disposable P4 pane process: record whether reattach publication reached stdin."""

import base64
import json
import os
import pathlib
import select
import sys
import termios
import time
import tty


result_path = pathlib.Path(sys.argv[1])
original = termios.tcgetattr(sys.stdin.fileno())
try:
    tty.setraw(sys.stdin.fileno())
    ready, _, _ = select.select([sys.stdin], [], [], 4.0)
    received = os.read(sys.stdin.fileno(), 8192) if ready else b""
    result_path.write_text(json.dumps({
        "bytes": len(received),
        "base64": base64.b64encode(received).decode("ascii"),
    }), encoding="utf-8")
    time.sleep(300)
finally:
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)
