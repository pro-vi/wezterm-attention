#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination="$root/libexec/attention-rs"
temporary="$root/libexec/.attention-rs.$$"

cleanup() {
  rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

mkdir -p -- "$root/libexec"
cd "$root"
# The build script records the commit; touching it makes cargo run it again
# even when no source file changed since the last build.
touch -- "$root/build.rs"
# CARGO_TARGET_DIR or build.target-dir would move the build elsewhere, and the
# copy below would take a stale binary from an earlier build.
cargo build --release --target-dir "$root/target"
cp -- "$root/target/release/attention" "$temporary"
chmod 755 "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
printf 'installed %s\n' "$("$destination" --version)"
