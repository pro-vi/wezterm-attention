#!/usr/bin/env python3
"""Exercise frozen maintenance with fake process ports; never scan real processes."""
from __future__ import annotations

import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[3]

OVERLAY = r'''
#[test]
fn frozen_lifecycle_sidecar_is_preserved_by_old_maintenance() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let old_dir = setup.binding_dir();
    let sidecar = old_dir.join("lifecycle.json");
    let (address, _) = pane_address(&setup.env).unwrap();
    let record = json!({
        "kind":"lifecycle_snapshot", "schema":2, "address":address,
        "launch_id":setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        "binding_id":binding_id("claude", "session-a", &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]),
        "provider":"claude", "snapshot_id":Uuid::new_v4().to_string(),
        "written_at_unix_ns":"00000000000000000001",
        "pools":{"requests":{"observations":[]},"general":{"observations":[]}}
    });
    let bytes = serde_json::to_vec(&record).unwrap();
    fs::write(&sidecar, &bytes).unwrap();
    setup.clock.set_unix(1);
    setup.provider_event("SessionEnd", "session-a", json!({"reason":"other"}), "00000000000000000300");
    setup.provider_event("SessionStart", "session-b", json!({"source":"resume"}), "00000000000000000400");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000721"));
    assert!(old_dir.exists());
    assert_eq!(fs::read(&sidecar).unwrap(), bytes);
}
'''


def main() -> None:
    contract = json.loads((Path(__file__).with_name("compatibility.json")).read_text())
    commit = contract["baseline_commit"]
    archive = subprocess.run(
        ["git", "archive", commit, "Cargo.toml", "Cargo.lock", "src", "tests", "protocol", "bin", "shell", "plugin", "scripts"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE,
    ).stdout
    with tempfile.TemporaryDirectory(prefix="attention-frozen-maintenance-") as directory:
        source = Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive)) as tree:
            # Python 3.9 has no extraction filter. This archive is produced from
            # our pinned Git tree; admit only ordinary files/directories inside
            # the disposable root before extracting any member.
            for member in tree.getmembers():
                (source / member.name).resolve().relative_to(source.resolve())
                if not (member.isfile() or member.isdir()):
                    raise ValueError("frozen source contains a non-regular archive member")
            tree.extractall(source)
        test = source / "tests/rust/maintenance_spec.rs"
        test.write_text(test.read_text() + OVERLAY)
        subprocess.run(
            ["cargo", "test", "--manifest-path", str(source / "Cargo.toml"), "--target-dir", str(ROOT / "target/frozen-lifecycle"), "--test", "maintenance_spec", "frozen_lifecycle_sidecar_is_preserved_by_old_maintenance", "--", "--exact"],
            check=True, cwd=ROOT,
        )
    print(json.dumps({"baseline_commit": commit, "old_maintenance_preserves_sidecar": True, "process_probe": "fake ports only"}))


if __name__ == "__main__":
    main()
