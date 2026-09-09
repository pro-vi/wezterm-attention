#!/usr/bin/env python3
"""Adapted copy of bootstrap's bridge marker reader."""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any


def marker_for(path: Path, now_s: float | None = None) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        marker = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(marker, dict):
        return None
    updated_at_ms = marker.get("updated_at_ms") or 0
    current_s = time.time() if now_s is None else now_s
    marker["age_s"] = max(0, int(current_s - updated_at_ms / 1000)) if updated_at_ms else None
    return marker
