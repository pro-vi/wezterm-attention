#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
result=$(mktemp "${TMPDIR:-/tmp}/wezterm-attention-smoke.XXXXXX")
trap 'rm -f "$result"' EXIT HUP INT TERM

WEZTERM_ATTENTION_SMOKE_RESULT="$result" \
WEZTERM_ATTENTION_TEST_ROOT="$root" \
wezterm --config-file "$root/tests/wezterm_protocol_smoke.lua" show-keys --lua >/dev/null

grep '^ok - wezterm-attention protocol/formatter smoke:' "$result"
