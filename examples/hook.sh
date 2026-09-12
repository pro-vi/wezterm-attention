#!/usr/bin/env sh
# Without arguments, write a custom stop marker. With PROVIDER EVENT, forward
# original hook stdin without constructing v2 JSON. Both need a launch claim.

if [ -z "${WEZTERM_ATTENTION_ROOT:-}" ] || [ ! -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]; then
  printf '%s\n' 'wezterm-attention: WEZTERM_ATTENTION_ROOT does not name an executable checkout' >&2
  exit 3
fi

case $# in
  0) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" mark stop --source example ;;
  2) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks event "$1" "$2" ;;
  *) printf '%s\n' 'usage: hook.sh [PROVIDER EVENT]' >&2; exit 2 ;;
esac
