#!/usr/bin/env sh
# Checks the shell entry points that ship with the checkout: the bin/attention
# launcher and scripts/install-cli.sh.
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-scripts-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
failures=0

pass() { printf 'ok - %s\n' "$1"; }
fail() {
  printf 'not ok - %s\n' "$1"
  [ ! -f "$scratch/out" ] || sed 's/^/#   /' "$scratch/out"
  failures=$((failures + 1))
}

# Runs a command with an empty environment; leaves its status in `status`
# and its stdout and stderr in out.
run() {
  status=0
  env -i HOME="$scratch" PATH=/usr/bin:/bin "$@" > "$scratch/out" 2>&1 || status=$?
}

# A checkout with the launcher and a stand-in Rust binary.
mkdir -p "$scratch/checkout/bin" "$scratch/checkout/libexec"
cp "$root/bin/attention" "$scratch/checkout/bin/attention"
printf '#!/bin/sh\nprintf "rust-binary %%s\\n" "$*"\n' > "$scratch/checkout/libexec/attention-rs"
chmod 755 "$scratch/checkout/libexec/attention-rs"
mkdir -p "$scratch/linked"
ln -s ../checkout/bin "$scratch/linked/bin"
ln -s ../checkout/bin/attention "$scratch/linked/attention"

run "$scratch/linked/bin/attention" doctor
if [ "$status" -eq 0 ] && grep -q '^rust-binary doctor$' "$scratch/out"; then
  pass "the launcher finds the binary through a symlinked bin directory"
else
  fail "the launcher finds the binary through a symlinked bin directory (status $status)"
fi
run "$scratch/linked/attention" doctor
if [ "$status" -eq 0 ] && grep -q '^rust-binary doctor$' "$scratch/out"; then
  pass "the launcher finds the binary through a relative symlink to itself"
else
  fail "the launcher finds the binary through a relative symlink to itself (status $status)"
fi

# A checkout that has not built its binary yet.
mkdir -p "$scratch/unbuilt/bin"
cp "$root/bin/attention" "$scratch/unbuilt/bin/attention"
missing="attention: Rust binary is missing; run $(cd -P "$scratch/unbuilt" && pwd -P)/scripts/install-cli.sh and retry."
hook_ok=1
for arguments in "hooks event claude Stop" "hooks claim" "hooks publish --quiet"; do
  # shellcheck disable=SC2086
  run "$scratch/unbuilt/bin/attention" $arguments
  [ "$status" -eq 0 ] && grep -qxF "$missing" "$scratch/out" || hook_ok=0
done
run "$scratch/unbuilt/bin/attention" hooks event claude Stop --strict
[ "$status" -eq 1 ] || hook_ok=0
if [ "$hook_ok" -eq 1 ]; then
  pass "a hook run without the binary exits 0, or 1 under --strict, and says why"
else
  fail "a hook run without the binary exits 0, or 1 under --strict, and says why (last status $status)"
fi
run "$scratch/unbuilt/bin/attention" doctor
if [ "$status" -eq 1 ] && grep -qxF "$missing" "$scratch/out"; then
  pass "any other command without the binary still fails"
else
  fail "any other command without the binary still fails (status $status)"
fi

# The installer copies what cargo just built, wherever the target directory
# is configured to be, never a stale binary left in ./target.
cargo=$(command -v cargo)
mkdir -p "$scratch/crate/scripts" "$scratch/crate/src" "$scratch/crate/target/release" "$scratch/elsewhere"
cp "$root/scripts/install-cli.sh" "$root/scripts/build-attention.sh" "$scratch/crate/scripts/"
printf '[package]\nname = "attention"\nversion = "0.0.0"\nedition = "2021"\n' > "$scratch/crate/Cargo.toml"
printf 'fn main() {}\n' > "$scratch/crate/build.rs"
printf 'fn main() { println!("fresh build"); }\n' > "$scratch/crate/src/main.rs"
printf '#!/bin/sh\necho stale build\n' > "$scratch/crate/target/release/attention"
chmod 755 "$scratch/crate/target/release/attention"
run PATH="${cargo%/*}:/usr/bin:/bin" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_TARGET_DIR="$scratch/elsewhere" \
  sh "$scratch/crate/scripts/install-cli.sh"
if [ "$status" -eq 0 ] && grep -q '^installed fresh build$' "$scratch/out"; then
  pass "the installer copies the binary it built when CARGO_TARGET_DIR points elsewhere"
else
  fail "the installer copies the binary it built when CARGO_TARGET_DIR points elsewhere (status $status)"
fi
# A configured build target puts the binary under target/<triple>/release,
# beside a stale one in target/release.
printf '#!/bin/sh\necho stale build\n' > "$scratch/crate/target/release/attention"
host=$(PATH="${cargo%/*}:/usr/bin:/bin" rustc -vV | sed -n 's/^host: //p')
run PATH="${cargo%/*}:/usr/bin:/bin" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_BUILD_TARGET="$host" \
  sh "$scratch/crate/scripts/install-cli.sh"
if [ -n "$host" ] && [ "$status" -eq 0 ] && grep -q '^installed fresh build$' "$scratch/out"; then
  pass "the installer copies the binary it built when a build target is configured"
else
  fail "the installer copies the binary it built when a build target is configured (status $status)"
fi

# A crate whose build reports no attention binary installs nothing.
mkdir -p "$scratch/other/scripts" "$scratch/other/src"
cp "$root/scripts/install-cli.sh" "$root/scripts/build-attention.sh" "$scratch/other/scripts/"
printf '[package]\nname = "other"\nversion = "0.0.0"\nedition = "2021"\n' > "$scratch/other/Cargo.toml"
printf 'fn main() {}\n' > "$scratch/other/build.rs"
printf 'fn main() {}\n' > "$scratch/other/src/main.rs"
run PATH="${cargo%/*}:/usr/bin:/bin" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" sh "$scratch/other/scripts/install-cli.sh"
if [ "$status" -eq 1 ] && [ ! -e "$scratch/other/libexec/attention-rs" ] \
  && grep -q 'did not report where it wrote the attention binary' "$scratch/out"; then
  pass "the installer refuses when cargo reports no attention binary"
else
  fail "the installer refuses when cargo reports no attention binary (status $status)"
fi

# The gate measures the binaries this build reports, not a fixed path under
# the target directory. A stand-in cargo reports one beside a stale
# target/release/attention.
reported="$scratch/reporting/target/other/release/attention"
mkdir -p "$scratch/reporting/bin" "$scratch/reporting/target/release" "${reported%/*}"
printf '#!/bin/sh\necho stale build\n' > "$scratch/reporting/target/release/attention"
printf '#!/bin/sh\necho fresh build\n' > "$reported"
cat > "$scratch/reporting/bin/cargo" <<EOF
#!/bin/sh
printf '%s\n' '{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"attention"},"executable":"$reported"}'
printf '%s\n' '{"reason":"build-finished","success":true}'
EOF
chmod 755 "$scratch/reporting/bin/cargo" "$scratch/reporting/target/release/attention" "$reported"
run PATH="$scratch/reporting/bin:/usr/bin:/bin" sh "$root/scripts/build-attention.sh" \
  "$scratch/reporting" "$scratch/reporting/target"
if [ "$status" -eq 0 ] && [ "$(cat "$scratch/out")" = "$reported" ] \
  && ! grep -n 'release/attention' "$root/tests/gate.sh" > "$scratch/out" \
  && grep -q 'scripts/build-attention.sh' "$root/tests/gate.sh"; then
  pass "the gate measures the binaries cargo reports building"
else
  fail "the gate measures the binaries cargo reports building (status $status)"
fi

[ "$failures" -eq 0 ]
