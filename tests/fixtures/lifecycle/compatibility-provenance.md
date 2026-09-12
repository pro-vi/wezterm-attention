# Lifecycle compatibility provenance

Baseline production source: commit `c9cc3e1d9906a3a3a4d70ac4bae9c9752a0c3b5e`.
The baseline Rust binary embeds that commit's original manifest. Its observed macOS release-build SHA-256 was `45f64cec20eb6a43631e7746e0fbb5b4921ffc29cd83f7ecb01e14ae8c31b1e6`.

The repeatable checks are:

- `frozen_reader_accepts_new_manifest_and_ignores_sidecar`: reconstructs the listed old Lua modules from Git, installs the candidate manifest only in disposable storage, and loads the old plugin through production WezTerm's module loader. The candidate CLI creates the state. Old core rendering and the six-value API remain usable; the sidecar bytes are unchanged.
- `check-compatibility.py`: extracts pinned production source, adds only a test overlay, and runs old maintenance against synthetic state with fake process ports. A lifecycle sidecar prevents pruning of the old binding. No real process environment scan is performed.
- `measure.py --baseline-rust`: compares distinct frozen/candidate binaries with identical synthetic callbacks, full observation pools, 100 timed hook processes and a 20-child burst. Two alternating rounds must satisfy the plan's local 2× bounds.

All three checks passed during implementation. The final full gate reruns them. Temporary source exports are not durable dependencies: the commit and `compatibility.json` reconstruct them.

This is not transparent cleanup compatibility. Old maintenance intentionally preserves unfamiliar files. It does not validate the new observation semantics or promise to reclaim those bindings. An old Rust source tree compiled against the new manifest is not the tested combination.
