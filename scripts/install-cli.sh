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
# target directory is pinned. A configured build target still moves the
# binary to target/<triple>/release, so the copy takes the path cargo reports
# for the binary it just built rather than a fixed one, which could be stale.
messages=$(cargo build --release --target-dir "$root/target" --message-format=json-render-diagnostics)
built=$(printf '%s\n' "$messages" | sed -n \
  's/^{"reason":"compiler-artifact".*"target":{[^}]*"name":"attention"[^}]*}.*"executable":"\([^"\\]*\)".*/\1/p')
case $built in
  /*/attention) ;;
  *)
    printf 'install-cli.sh: cargo did not report where it wrote the attention binary; nothing was installed\n' >&2
    exit 1
    ;;
esac
cp -- "$built" "$temporary"
chmod 755 "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
printf 'installed %s\n' "$("$destination" --version)"
