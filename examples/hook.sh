#!/usr/bin/env sh
# Without arguments, write a custom stop marker. With PROVIDER EVENT, forward
# original hook stdin without constructing v2 JSON. Both need a launch claim.
# For an agent that claims its own pane, register it as
# `WEZTERM_ATTENTION_HOST_PID=$PPID exec sh hook.sh PROVIDER EVENT`, so the
# writer this script execs stays the agent's direct child.
# To opt into transient reply delivery, pass consumer flags after PROVIDER EVENT:
# hook.sh claude Stop --consumer /absolute/reply-sink.mjs --include-reply --consumer-timeout-ms 1000
# ATTENTION_REPLY_FILE selects the application-owned sink destination.

# Claude Code and Codex read exit 2 as "block", so this script's own failures
# exit 0, or 1 when --strict asks for failures to be reported.
failure_status=0
for argument in "$@"; do
  [ "$argument" != --strict ] || failure_status=1
done

if [ -z "${WEZTERM_ATTENTION_ROOT:-}" ] || [ ! -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]; then
  printf '%s\n' 'wezterm-attention: WEZTERM_ATTENTION_ROOT does not name an executable checkout' >&2
  exit "$failure_status"
fi

case $# in
  0) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" mark stop --source example ;;
  1) printf '%s\n' 'usage: hook.sh [PROVIDER EVENT [CONSUMER FLAGS]]' >&2; exit "$failure_status" ;;
  *) exec "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks event "$@" ;;
esac
