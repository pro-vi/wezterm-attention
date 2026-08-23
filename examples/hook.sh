#!/usr/bin/env bash
#
# Shell one-liner to write an attention marker.
# Drop this into any hook or script that runs inside WezTerm.

# WEZTERM_PANE is a non-negative integer set by WezTerm; bail if it's unset or
# malformed so a stray value can't build a path outside the marker directory.
case "$WEZTERM_PANE" in
  '' | *[!0-9]*) exit 0 ;;
esac

MARKER_DIR="${HOME}/.local/state/wezterm-attention"
mkdir -p "$MARKER_DIR"

# Write a "stop" marker (tab shows ✓ in mint)
# Atomic: write to .tmp then rename to avoid partial reads
if command -v uuidgen >/dev/null 2>&1; then
  REVISION="$(uuidgen)"
else
  REVISION="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
fi
printf '{"type":"stop","revision":"%s","updated_at":%s}\n' "$REVISION" "$(date +%s)" > "${MARKER_DIR}/${WEZTERM_PANE}.tmp"
mv "${MARKER_DIR}/${WEZTERM_PANE}.tmp" "${MARKER_DIR}/${WEZTERM_PANE}"

# Other payloads (include a fresh revision when writing them):
#   {"type":"notify"}                # tab shows ! in rose
#   {"type":"thinking","frame":0}  # animated spinner
#   {"type":"review"}                # tab shows ◆ in gold
