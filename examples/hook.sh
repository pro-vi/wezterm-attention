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
printf '{"type":"stop","updated_at":%s}\n' "$(date +%s)" > "${MARKER_DIR}/${WEZTERM_PANE}.tmp"
mv "${MARKER_DIR}/${WEZTERM_PANE}.tmp" "${MARKER_DIR}/${WEZTERM_PANE}"

# Other types:
#   echo '{"type":"notify"}'           # tab shows ! in rose
#   echo '{"type":"thinking","frame":0}'  # animated spinner
#   echo '{"type":"review"}'           # tab shows ◆ in gold
