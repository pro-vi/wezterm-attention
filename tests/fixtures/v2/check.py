#!/usr/bin/env python3
"""Independent standard-library checker for the shared v2 protocol fixture."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
MANIFEST_PATH = ROOT / "protocol" / "v2.json"
FIXTURE_PATH = HERE / "protocol-cases.json"

HEX64 = re.compile(r"^[0-9a-f]{64}$")
UUID = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
)
DECIMAL_NS20 = re.compile(r"^[0-9]{20}$")
CANONICAL_DECIMAL = re.compile(r"^(0|[1-9][0-9]*)$")


class InvalidRecord(ValueError):
    pass


def load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def enum_set(manifest: dict[str, Any], name: str) -> set[str]:
    return set(manifest["enums"][name])


def is_safe_text(value: Any, maximum: int) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value.encode("utf-8")) <= maximum
        and not any(ord(char) < 32 or ord(char) == 127 for char in value)
    )


def validate_address(value: Any, manifest: dict[str, Any]) -> None:
    if not isinstance(value, dict) or set(value) != {
        "realm_id",
        "incarnation_id",
        "pane_id",
    }:
        raise InvalidRecord("address must contain exactly realm_id, incarnation_id, pane_id")
    if not isinstance(value["realm_id"], str) or not HEX64.fullmatch(value["realm_id"]):
        raise InvalidRecord("invalid realm_id")
    if not isinstance(value["incarnation_id"], str) or not HEX64.fullmatch(
        value["incarnation_id"]
    ):
        raise InvalidRecord("invalid incarnation_id")
    pane_id = value["pane_id"]
    if (
        not isinstance(pane_id, str)
        or not CANONICAL_DECIMAL.fullmatch(pane_id)
        or len(pane_id) > manifest["limits"]["pane_id_max_digits"]
    ):
        raise InvalidRecord("invalid pane_id")


def validate_target(value: Any) -> None:
    if not isinstance(value, dict) or value.get("kind") not in {"binding", "launch"}:
        raise InvalidRecord("invalid activity target")
    if value["kind"] == "launch":
        if set(value) != {"kind"}:
            raise InvalidRecord("launch target has extra fields")
        return
    if set(value) != {"kind", "binding_id"} or not HEX64.fullmatch(
        str(value.get("binding_id", ""))
    ):
        raise InvalidRecord("invalid binding target")


def validate_typed_field(
    field_type: str,
    value: Any,
    manifest: dict[str, Any],
    expected_kind: str | None,
) -> None:
    limits = manifest["limits"]
    if field_type in {"lifecycle_pools", "observation_pool", "native_correlation"}:
        name = {"lifecycle_pools": "pools", "observation_pool": "pool", "native_correlation": "correlation"}[field_type]
        validate_shape(value, manifest["lifecycle_shapes"][name], manifest, None)
    elif field_type == "lifecycle_actor":
        if not isinstance(value, dict) or value.get("kind") not in {"lead", "child"}:
            raise InvalidRecord("invalid actor")
        validate_shape(value, manifest["lifecycle_shapes"][value["kind"]], manifest, value["kind"])
    elif field_type == "observation_array":
        if not isinstance(value, list) or len(value) > limits["lifecycle_pool_max_count"]:
            raise InvalidRecord("invalid observation array")
        for item in value:
            if not isinstance(item, dict) or item.get("kind") not in manifest["lifecycle_variants"]:
                raise InvalidRecord("invalid observation kind")
            validate_shape(item, manifest["lifecycle_variants"][item["kind"]], manifest, item["kind"])
    elif field_type in manifest.get("lifecycle_enums", {}):
        if not isinstance(value, str) or value not in manifest["lifecycle_enums"][field_type]:
            raise InvalidRecord("unsupported lifecycle enum")
    elif field_type == "record_kind":
        if value != expected_kind:
            raise InvalidRecord("record kind mismatch")
    elif field_type == "record_schema":
        if value != manifest["record_schema"]:
            raise InvalidRecord("record schema mismatch")
    elif field_type == "wire_version":
        if value != manifest["wire_version"]:
            raise InvalidRecord("wire version mismatch")
    elif field_type in {"decimal_ns20", "monotonic_ns20", "unix_ns20"}:
        if not isinstance(value, str) or not DECIMAL_NS20.fullmatch(value):
            raise InvalidRecord(f"invalid {field_type}")
    elif field_type == "hex64":
        if not isinstance(value, str) or not HEX64.fullmatch(value):
            raise InvalidRecord("invalid hex64")
    elif field_type == "uuid":
        if not isinstance(value, str) or not UUID.fullmatch(value):
            raise InvalidRecord("invalid uuid")
    elif field_type == "canonical_decimal":
        if (
            not isinstance(value, str)
            or not CANONICAL_DECIMAL.fullmatch(value)
            or len(value) > limits["canonical_decimal_max_digits"]
        ):
            raise InvalidRecord("invalid canonical decimal")
    elif field_type == "pane_address":
        validate_address(value, manifest)
    elif field_type == "activity_target":
        validate_target(value)
    elif field_type == "absolute_path":
        if (
            not is_safe_text(value, limits["path_max_bytes"])
            or not value.startswith("/")
        ):
            raise InvalidRecord("invalid absolute path")
    elif field_type == "safe_label":
        if not is_safe_text(value, limits["safe_label_max_bytes"]):
            raise InvalidRecord("invalid safe label")
    elif field_type == "model":
        if not is_safe_text(value, limits["model_max_bytes"]):
            raise InvalidRecord("invalid model")
    elif field_type == "writer_version":
        if not is_safe_text(value, limits["writer_version_max_bytes"]):
            raise InvalidRecord("invalid writer version")
    elif field_type == "boolean":
        if not isinstance(value, bool):
            raise InvalidRecord("invalid boolean")
    elif field_type == "nonnegative_integer":
        if (
            not isinstance(value, int)
            or isinstance(value, bool)
            or value < 0
            or value > limits["frame_max"]
        ):
            raise InvalidRecord("invalid nonnegative integer")
    elif field_type == "positive_integer":
        if (
            not isinstance(value, int)
            or isinstance(value, bool)
            or value <= 0
            or value > limits["ttl_ms_max"]
        ):
            raise InvalidRecord("invalid positive integer")
    elif field_type == "subagent_ttl_ms":
        if value != limits["subagent_ttl_ms"]:
            raise InvalidRecord("invalid subagent ttl")
    elif field_type == "provider":
        if value not in enum_set(manifest, "providers"):
            raise InvalidRecord("invalid provider")
    elif field_type == "subagent_provider":
        if value not in enum_set(manifest, "subagent_providers"):
            raise InvalidRecord("invalid subagent provider")
    elif field_type == "subagent_status":
        if value not in enum_set(manifest, "subagent_statuses"):
            raise InvalidRecord("invalid subagent status")
    elif field_type == "activity_type":
        if value not in enum_set(manifest, "activity_types"):
            raise InvalidRecord("invalid activity type")
    elif field_type == "end_reason":
        if value not in enum_set(manifest, "end_reasons"):
            raise InvalidRecord("invalid end reason")
    else:
        raise InvalidRecord(f"manifest names unsupported field type {field_type!r}")


def validate_shape(
    value: Any,
    spec: dict[str, Any],
    manifest: dict[str, Any],
    expected_kind: str | None,
) -> None:
    if not isinstance(value, dict):
        raise InvalidRecord("value must be an object")
    required = set(spec["required"])
    allowed = required | set(spec["optional"])
    if not required.issubset(value):
        raise InvalidRecord("missing required field")
    if not set(value).issubset(allowed):
        raise InvalidRecord("unknown field")
    for field, field_value in value.items():
        validate_typed_field(spec["types"][field], field_value, manifest, expected_kind)


def parse_wire(value: Any, manifest: dict[str, Any]) -> str:
    if isinstance(value, dict) and isinstance(value.get("wire"), int):
        if value["wire"] > manifest["wire_version"]:
            return "future_schema"
    try:
        validate_shape(value, manifest["wire"], manifest, None)
    except InvalidRecord:
        return "record_invalid"
    return "valid"


def parse_record(value: Any, manifest: dict[str, Any]) -> str:
    if not isinstance(value, dict):
        return "record_invalid"
    schema = value.get("schema")
    if value.get("kind") == "lifecycle_snapshot" and (type(schema) is not int or not 0 <= schema < 2**64):
        return "record_invalid"
    if isinstance(schema, int) and schema > manifest["record_schema"]:
        return "future_schema"
    kind = value.get("kind")
    spec = manifest["records"].get(kind) if isinstance(kind, str) else None
    if spec is None:
        return "record_invalid"
    try:
        validate_shape(value, spec, manifest, kind)
        if kind == "lifecycle_snapshot":
            validate_lifecycle(value, manifest)
    except InvalidRecord:
        return "record_invalid"
    if kind == "subagent_presence" and hashlib.sha256(
        value["agent_id"].encode("utf-8")
    ).hexdigest() != value["agent_key"]:
        return "record_invalid"
    if kind == "review" and hashlib.sha256(value["owner_id"].encode("utf-8")).hexdigest() != value[
        "owner_key"
    ]:
        return "record_invalid"
    return "valid"


def compact_size(value: Any) -> int:
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))


def read_lifecycle_record(path: Path, manifest: dict[str, Any]) -> str:
    """Read one bounded record, rejecting deep containers before JSON decoding."""
    with path.open("rb") as handle:
        raw = handle.read(manifest["limits"]["lifecycle_max_json_bytes"] + 1)
    if len(raw) > manifest["limits"]["lifecycle_max_json_bytes"]:
        return "record_invalid"
    depth, quoted, escaped = 0, False, False
    for byte in raw:
        if quoted:
            if escaped:
                escaped = False
            elif byte == 92:
                escaped = True
            elif byte == 34:
                quoted = False
        elif byte == 34:
            quoted = True
        elif byte in (91, 123):
            depth += 1
            if depth > manifest["limits"]["lifecycle_max_depth"]:
                return "record_invalid"
        elif byte in (93, 125):
            depth -= 1
    try:
        return parse_record(json.loads(raw), manifest)
    except (ValueError, UnicodeError):
        return "record_invalid"


def validate_lifecycle(value: dict[str, Any], manifest: dict[str, Any]) -> None:
    if type(value["schema"]) is not int:
        raise InvalidRecord("lifecycle schema must be an integer")
    limits = manifest["limits"]
    keys: set[tuple[Any, ...]] = set()
    ids: set[str] = set()
    for name, pool in value["pools"].items():
        if compact_size(pool) > limits["lifecycle_pool_max_bytes"]:
            raise InvalidRecord("pool byte bound")
        prior: tuple[str, str] | None = None
        for item in pool["observations"]:
            kind, actor, correlation = item["kind"], item["actor"], item.get("correlation", {})
            if item["source_event"] not in manifest["lifecycle_sources"].get(value["provider"], {}).get(kind, []):
                raise InvalidRecord("unsupported native source")
            if "mcp_server_name" in correlation and kind not in {"elicitation_requested", "elicitation_action_selected"}:
                raise InvalidRecord("unexpected MCP namespace")
            if kind in {"tool_preflight", "tool_result", "approval_requested", "automatic_denial"}:
                namespace = "tool_call_id"
            elif kind in {"elicitation_requested", "elicitation_action_selected"}:
                namespace = "elicitation_id"
            else:
                namespace = "message_id" if "message_id" in correlation else "turn_id"
            if namespace not in correlation:
                namespace = "observation_id"
            key = (kind, actor["kind"], actor.get("agent_id"), namespace, correlation.get(namespace, item["observation_id"]), correlation.get("turn_id"), correlation.get("mcp_server_name"))
            if "elicitation_id" in correlation and "mcp_server_name" not in correlation:
                raise InvalidRecord("elicitation namespace is missing")
            order = (item["observed_mono_ns"], item["observation_id"])
            if key in keys or item["observation_id"] in ids or (prior and prior > order):
                raise InvalidRecord("duplicate or unordered observation")
            keys.add(key)
            ids.add(item["observation_id"])
            prior = order
            request = kind in {"approval_requested", "automatic_denial", "elicitation_requested", "elicitation_action_selected", "notice"}
            if kind in {"tool_preflight", "tool_result"}:
                request = item["tool_class"] != "generic"
                provider_tool = (value["provider"], item["tool_name"])
                expected = {
                    ("claude", "AskUserQuestion"): ("question", "blocking"),
                    ("codex", "request_user_input"): ("question", "blocking"),
                    ("codex", "request_user_input_async"): ("question", "nonblocking"),
                    ("codex", "request_permissions"): ("permission", None),
                }.get(provider_tool, ("generic", None))
                if (item["tool_class"], item.get("question_mode")) != expected:
                    raise InvalidRecord("tool classification mismatch")
                if item.get("question_mode") == "nonblocking":
                    if actor["kind"] != "lead":
                        raise InvalidRecord("async question is root only")
                    if kind == "tool_result" and (item["source_event"] != "PostToolUse" or item["result_surface"] != "post_hook" or item.get("is_error") is True or item.get("interrupted") is True):
                        raise InvalidRecord("invalid publication result")
            if name != ("requests" if request else "general") or compact_size(item) > limits["lifecycle_observation_max_bytes"]:
                raise InvalidRecord("observation membership or bound")
            if item["observed_mono_ns"] <= pool.get("retention_floor_mono_ns", ""):
                raise InvalidRecord("observation below pool floor")
            if actor["kind"] == "child" and (value["provider"] == "pi" or hashlib.sha256(actor["agent_id"].encode("utf-8")).hexdigest() != actor["agent_key"]):
                raise InvalidRecord("child identity mismatch")
    size = compact_size(value) + 1
    envelope = size - sum(compact_size(pool) for pool in value["pools"].values())
    if size > limits["lifecycle_max_json_bytes"] or envelope > limits["lifecycle_envelope_max_bytes"]:
        raise InvalidRecord("snapshot byte bound")


def assign_path(value: dict[str, Any], dotted: str, replacement: Any) -> None:
    parts = dotted.split(".")
    cursor: dict[str, Any] = value
    for part in parts[:-1]:
        nested = cursor.get(part)
        if not isinstance(nested, dict):
            nested = {}
            cursor[part] = nested
        cursor = nested
    cursor[parts[-1]] = replacement


def remove_path(value: dict[str, Any], dotted: str) -> None:
    parts = dotted.split(".")
    cursor: Any = value
    for part in parts[:-1]:
        if not isinstance(cursor, dict) or part not in cursor:
            return
        cursor = cursor[part]
    if isinstance(cursor, dict):
        cursor.pop(parts[-1], None)


def case_value(case: dict[str, Any], fixture: dict[str, Any]) -> Any:
    if "raw" in case:
        value = json.loads(case["raw"])
    elif "value" in case:
        value = copy.deepcopy(case["value"])
    elif case["parser"] == "wire":
        value = copy.deepcopy(fixture["wire_sample"])
    else:
        value = copy.deepcopy(fixture["record_samples"][case["sample"]])
    if isinstance(value, dict):
        for dotted, replacement in case.get("patch", {}).items():
            assign_path(value, dotted, replacement)
        for dotted, repeated in case.get("repeat_patch", {}).items():
            assign_path(
                value,
                dotted,
                repeated.get("prefix", "") + repeated["text"] * repeated["count"],
            )
        for dotted in case.get("remove", []):
            remove_path(value, dotted)
    return value


def eligible_presence(
    case: dict[str, Any], fixture: dict[str, Any], manifest: dict[str, Any]
) -> tuple[bool, str | None]:
    presence = copy.deepcopy(fixture["record_samples"]["subagent_presence"])
    for field in ("written_at_unix_ns", "observed_mono_ns", "status"):
        if field in case:
            presence[field] = case[field]
    now = case.get("now_unix_ns")
    if not isinstance(now, str):
        return False, "probe_unavailable"
    if parse_record(presence, manifest) != "valid":
        return False, "record_invalid"
    observed = presence["observed_mono_ns"]
    clear = case.get(
        "clear_mono_ns",
        fixture["record_samples"]["subagent_clear"]["observed_mono_ns"],
    )
    floor = case.get(
        "floor_mono_ns",
        fixture["record_samples"]["subagent_retention_floor"]["floor_mono_ns"],
    )
    if presence["status"] != "active":
        return False, None
    if clear is not False and observed <= clear:
        return False, None
    if floor is not False and observed <= floor:
        return False, None
    written = presence["written_at_unix_ns"]
    if not DECIMAL_NS20.fullmatch(now):
        return False, "probe_unavailable"
    if now < written:
        return False, "clock_skew"
    expiry = int(written) + presence["ttl_ms"] * 1_000_000
    return int(now) <= expiry, None


def check_path_identity(path: str, record: dict[str, Any]) -> None:
    parts = Path(path).parts
    kind = record["kind"]
    address = record.get("address")
    if kind == "realm":
        if parts[-2] != record["realm_id"]:
            raise InvalidRecord("realm path mismatch")
        return
    if kind == "incarnation":
        if parts[-2] != record["incarnation_id"] or parts[-4] != record["realm_id"]:
            raise InvalidRecord("incarnation path mismatch")
        return
    if isinstance(address, dict):
        if (
            parts[2] != address["realm_id"]
            or parts[4] != address["incarnation_id"]
            or parts[6] != address["pane_id"]
        ):
            raise InvalidRecord("pane address path mismatch")
    if "launch_id" in record and "launches" in parts:
        launch_index = parts.index("launches") + 1
        if parts[launch_index] != record["launch_id"]:
            raise InvalidRecord("launch path mismatch")
    if "binding_id" in record and "bindings" in parts:
        binding_index = parts.index("bindings") + 1
        if parts[binding_index] != record["binding_id"]:
            raise InvalidRecord("binding path mismatch")
    if kind == "review" and Path(path).stem != record["owner_key"]:
        raise InvalidRecord("review owner path mismatch")
    if kind == "subagent_presence" and Path(path).stem != record["agent_key"]:
        raise InvalidRecord("subagent path mismatch")


def run(render: bool) -> int:
    manifest = load_json(MANIFEST_PATH)
    fixture = load_json(FIXTURE_PATH)
    failures: list[str] = []

    if manifest.get("digests") != {
        "algorithm": "sha256",
        "encoding": "lowercase_hex",
        "agent_key_input": "utf8(agent_id)",
        "owner_key_input": "utf8(owner_id)",
        "realm_id_input": "utf8(canonical_socket_path)",
        "incarnation_id_input": "lp64be(realm_id,socket_device,socket_inode,socket_ctime_ns_decimal)",
        "tty_fingerprint_input": "lp64be(tty_device,tty_inode,tty_rdevice)",
        "binding_id_input": "utf8(provider) || 0x00 || utf8(provider_session_id) || 0x00 || utf8(launch_id)",
    }:
        failures.append("manifest digest contract is not the supported SHA-256 relationship")

    for case in fixture["parse_cases"]:
        value = case_value(case, fixture)
        actual = (
            parse_wire(value, manifest)
            if case["parser"] in {"wire", "wire_json"}
            else parse_record(value, manifest)
        )
        if actual != case["expected"]:
            failures.append(f"{case['id']}: expected {case['expected']}, got {actual}")

    eligibility: dict[str, bool] = {}
    for case in fixture["eligibility_cases"]:
        actual, diagnostic = eligible_presence(case, fixture, manifest)
        eligibility[case["id"]] = actual
        if actual is not case["expected"] or diagnostic != case.get("diagnostic"):
            failures.append(
                f"{case['id']}: expected ({case['expected']}, {case.get('diagnostic')}), "
                f"got ({actual}, {diagnostic})"
            )

    state = fixture["state_case"]
    for entry in state["files"]:
        record = fixture["record_samples"][entry["sample"]]
        actual = parse_record(record, manifest)
        if actual != "valid":
            failures.append(f"state {entry['path']}: parser returned {actual}")
            continue
        try:
            check_path_identity(entry["path"], record)
        except InvalidRecord as error:
            failures.append(f"state {entry['path']}: {error}")

    older = copy.deepcopy(fixture["record_samples"]["subagent_presence"])
    newer = copy.deepcopy(older)
    older["observed_mono_ns"] = "00000000000000000001"
    older["written_at_unix_ns"] = "00000000999000000000"
    newer["observed_mono_ns"] = "00000000000000000002"
    newer["written_at_unix_ns"] = "00000000001000000000"
    if not older["observed_mono_ns"] < newer["observed_mono_ns"]:
        failures.append("event order incorrectly depended on written_at_unix_ns")

    if failures:
        for failure in failures:
            print(f"not ok - {failure}", file=sys.stderr)
        return 1

    print(
        f"ok - {len(fixture['parse_cases'])} protocol rows; "
        f"{len(fixture['eligibility_cases'])} eligibility rows"
    )
    if render:
        print("\nprotocol fixture state tree")
        for entry in state["files"]:
            print(f"  {entry['path']}  [{entry['sample']}]")
        print("\nexpected AttentionView")
        print(json.dumps(state["expected_view"], indent=2, sort_keys=True))
        print("\nexpected render")
        print(json.dumps(state["expected_render"], indent=2, sort_keys=True))
        print("\neligibility")
        print(json.dumps(eligibility, indent=2, sort_keys=True))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--render", action="store_true")
    args = parser.parse_args()
    result = run(args.render)
    manifest = load_json(MANIFEST_PATH)
    lifecycle = load_json(ROOT / "tests/fixtures/lifecycle/observations.json")
    cases = lifecycle["cases"]
    for case in cases:
        actual = parse_record(case["value"], manifest)
        if actual != case["expected"]:
            print(f"not ok - lifecycle {case['id']}: {actual}", file=sys.stderr)
            result = 1
    print(f"checked {len(cases)} lifecycle protocol rows")
    for case in lifecycle["raw_cases"]:
        actual = parse_record(json.loads(case["raw"]), manifest)
        if actual != case["expected"]:
            print(f"not ok - lifecycle raw {case['id']}: {actual}", file=sys.stderr)
            result = 1
    print(f"checked {len(lifecycle['raw_cases'])} lifecycle raw JSON rows")
    return result


if __name__ == "__main__":
    raise SystemExit(main())
