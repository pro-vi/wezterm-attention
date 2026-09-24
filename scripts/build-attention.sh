#!/usr/bin/env sh
# Usage: build-attention.sh CRATE_DIR TARGET_DIR
#
# Builds the crate in CRATE_DIR for release into TARGET_DIR and prints the path
# of the attention binary cargo reports writing. A configured build target
# moves the binary to TARGET_DIR/<triple>/release, where a fixed path would
# find nothing or a stale build, so callers take the path from here.
set -eu

messages=$(cargo build --release --manifest-path "$1/Cargo.toml" --target-dir "$2" \
  --message-format=json-render-diagnostics)
built=$(printf '%s\n' "$messages" | sed -n \
  's/^{"reason":"compiler-artifact".*"target":{[^}]*"name":"attention"[^}]*}.*"executable":"\([^"\\]*\)".*/\1/p')
case $built in
  /*/attention) printf '%s\n' "$built" ;;
  *)
    printf 'build-attention.sh: cargo did not report where it wrote the attention binary\n' >&2
    exit 1
    ;;
esac
