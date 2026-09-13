#!/usr/bin/env sh
# Without arguments, write a custom stop marker. With PROVIDER EVENT, forward
# original hook stdin without constructing v2 JSON. Both need a launch claim.
# To opt into transient reply delivery, pass consumer flags after PROVIDER EVENT:
# hook.sh claude Stop --consumer /absolute/reply-sink.mjs --include-reply --consumer-timeout-ms 1000
# ATTENTION_REPLY_FILE selects the application-owned sink destination.

if [ -z "${WEZTERM_ATTENTION_ROOT:-}" ] || [ ! -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]; then
  printf '%s\n' 'wezterm-attention: WEZTERM_ATTENTION_ROOT does not name an executable checkout' >&2
  exit 3
fi

case $# in
  0) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" mark stop --source example ;;
  1) printf '%s\n' 'usage: hook.sh [PROVIDER EVENT [CONSUMER FLAGS]]' >&2; exit 2 ;;
  *) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks event "$@" ;;
esac
