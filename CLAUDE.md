# Instructions for agents working in this repository

## Releasing

Cut a release only when the maintainer asks for one. This repository has no CI,
so `tests/gate.sh` is the only check, and nothing runs it for you.

1. Pick the version from "The product version" in `docs/consumer-guide.md`: a
   release that adds to a public contract raises the minor version, one that
   raises a schema or wire number raises the major version, and fixes only
   raise the patch.
2. Make one release commit. It sets the version in `Cargo.toml`, `package.json`,
   `Cargo.lock` (the `wezterm-attention` entry) and `protocol/v2.json`
   (`writer_version`, which `tests/rust/cli_shell_spec.rs` holds equal to the
   crate version), and dates the heading in `CHANGELOG.md` as
   `## X.Y.Z — YYYY-MM-DD`. The version changes in no other commit, because a
   build between releases reports the last release's version.
3. Run `sh tests/gate.sh` on that exact commit, at normal load: a one-minute
   load average under about 12. At load 203 two shell timing specs failed in
   the 1.0.0 gate run, and a rerun at normal load passed.
4. Tag that commit `vX.Y.Z` and push the commit and the tag. Tag after the
   heading is dated: the `v1.0.0` tag sits one commit before the one that dated
   its heading, so the tagged `CHANGELOG.md` has no date.
5. Publish the GitHub release, with the body taken from the `CHANGELOG.md`
   section for that version.
6. Check the real tag. Run the "stay on a release" steps in `docs/setup.md` against it
   (`git clone --branch vX.Y.Z`, `scripts/install-cli.sh`, load the plugin by
   path), then confirm that `attention --version` and `hooks describe`'s
   `writer_version` both report the new version.
