#!/usr/bin/env bash
#
# Shell one-liner to write an attention marker.
# Drop this into any hook or script that runs inside WezTerm.

MARKER_DIR="${HOME}/.claude/wezterm-attention"
mkdir -p "$MARKER_DIR"

# Write a "stop" marker (tab shows ✓ in mint)
echo '{"type":"stop"}' > "${MARKER_DIR}/${WEZTERM_PANE}"

# Other types:
#   echo '{"type":"notify"}'  > "${MARKER_DIR}/${WEZTERM_PANE}"   # tab shows ! in rose
#   echo '{"type":"thinking","frame":0}' > ...                     # animated spinner
#   echo '{"type":"review"}'  > "${MARKER_DIR}/${WEZTERM_PANE}"   # tab shows ◆ in gold
