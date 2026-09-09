#!/usr/bin/env python3
"""Fail when a Python writer test has no recorded Rust-era disposition."""

from __future__ import annotations

import ast
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PYTHON_SUITE = ROOT / "tests" / "attention_cli_test.py"
MAPPING = Path(__file__).with_name("python-test-map.json")
RETIRED_INVENTORY = Path(__file__).with_name("retired-python-test-inventory.json")


def python_tests() -> set[str]:
    tree = ast.parse(PYTHON_SUITE.read_text(encoding="utf-8"))
    return {
        f"{node.name}.{item.name}"
        for node in tree.body
        if isinstance(node, ast.ClassDef)
        for item in node.body
        if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) and item.name.startswith("test_")
    }


def load_expected_tests() -> set[str]:
    if PYTHON_SUITE.exists():
        return python_tests()
    inventory = json.loads(RETIRED_INVENTORY.read_text(encoding="utf-8"))
    if inventory.get("source_suite") != "tests/attention_cli_test.py":
        raise SystemExit("retired Python test inventory has no source suite")
    source_hash = inventory.get("pre_retirement_gate_sha256")
    if not isinstance(source_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", source_hash):
        raise SystemExit("retired Python test inventory has no evidence digest")
    tests = inventory.get("tests")
    if not isinstance(tests, list) or not all(isinstance(test, str) for test in tests):
        raise SystemExit("retired Python test inventory is invalid")
    expected = set(tests)
    if len(expected) != len(tests):
        raise SystemExit("retired Python test inventory contains duplicates")
    return expected


def rust_tests() -> set[str]:
    names: set[str] = set()
    for path in (ROOT / "tests" / "rust").glob("*.rs"):
        names.update(re.findall(r"(?m)^fn (test_[A-Za-z0-9_]+|[A-Za-z0-9_]+)\s*\(", path.read_text(encoding="utf-8")))
    return names


def main() -> None:
    mapping = json.loads(MAPPING.read_text(encoding="utf-8"))
    expected = load_expected_tests()
    actual = set(mapping)
    if expected != actual:
        raise SystemExit(
            f"python test map mismatch: missing={sorted(expected - actual)!r} extra={sorted(actual - expected)!r}"
        )
    rust = rust_tests()
    for test_id, row in mapping.items():
        if row.get("disposition") not in {"retained", "changed", "removed"}:
            raise SystemExit(f"{test_id}: invalid disposition")
        proof = row.get("proof")
        if not isinstance(proof, str) or not proof:
            raise SystemExit(f"{test_id}: missing proof")
        for target in re.findall(r"rust:([A-Za-z0-9_]+)", proof):
            if target not in rust:
                raise SystemExit(f"{test_id}: missing Rust test {target}")
        if row["disposition"] == "removed" and not proof.startswith("cut:"):
            raise SystemExit(f"{test_id}: removed behavior needs a recorded cut")
    counts = {name: sum(row["disposition"] == name for row in mapping.values()) for name in ("retained", "changed", "removed")}
    print(f"ok - mapped {len(mapping)} Python tests: {counts['retained']} retained, {counts['changed']} changed, {counts['removed']} removed")


if __name__ == "__main__":
    main()
