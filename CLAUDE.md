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
   `writer_version` both report the new version. In a scratch folder:
   - `git clone --branch vX.Y.Z https://github.com/pro-vi/wezterm-attention <clone>`.
     Git 2.50.1 warns that the annotated tag "is not a commit"; the checkout is
     still the tagged commit.
   - `sh <clone>/scripts/install-cli.sh`, then `<clone>/bin/attention --version`
     and `<clone>/bin/attention hooks describe --provider claude --json`, whose
     `result.writer_version` holds the version.
   - Write a config that loads the plugin as `docs/setup.md` shows,
     `loadfile(clone .. "/plugin/init.lua")("wezterm-attention", clone .. "/plugin/init.lua")`,
     calls `apply_to_config` and then `wezterm.log_info` with a marker, and
     run `wezterm --config-file <that config> show-keys`. Only the marker in
     stderr means the plugin loaded: when the config raises an error,
     WezTerm 20260905-195314-b99b1ca2's `show-keys` still exits 0 and prints
     nothing to stderr.
