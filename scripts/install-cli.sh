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
cargo build --release
cp -- "$root/target/release/attention" "$temporary"
chmod 755 "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
