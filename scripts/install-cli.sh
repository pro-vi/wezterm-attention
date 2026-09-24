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
# CARGO_TARGET_DIR or build.target-dir would move the build elsewhere, so the
# target directory is pinned. The copy takes the path cargo reports for the
# binary it just built rather than a fixed one, which could be stale.
built=$(sh "$root/scripts/build-attention.sh" "$root" "$root/target") || {
  printf 'install-cli.sh: nothing was installed\n' >&2
  exit 1
}
cp -- "$built" "$temporary"
chmod 755 "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
printf 'installed %s\n' "$("$destination" --version)"
