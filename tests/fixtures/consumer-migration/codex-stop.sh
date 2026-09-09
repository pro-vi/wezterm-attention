#!/usr/bin/env sh
set -eu

if [ -z "${WEZTERM_ATTENTION_ROOT:-}" ] || [ ! -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]; then
  printf '%s\n' 'wezterm-attention: WEZTERM_ATTENTION_ROOT does not name an executable checkout' >&2
  exit 3
fi

exec "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks event codex Stop
