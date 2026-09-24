#!/usr/bin/env sh
# Drives shell/wezterm-attention.bash in real interactive bash sessions.
#
# Usage: bash_integration_spec.sh BASH BASH_PREEXEC
#   BASH          the bash executable under test
#   BASH_PREEXEC  a copy of bash-preexec.sh (github.com/rcaloras/bash-preexec)
#
# Every session starts from an empty environment, a scratch HOME and its own rc
# file, and talks to a stand-in writer that records its calls.
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
bash_under_test=$1
bash_preexec=$2
integration="$root/shell/wezterm-attention.bash"
driver="$root/tests/python/interactive_shell.py"
python=${ATTENTION_TEST_PYTHON3:-$(command -v python3)}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-bash-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
failures=0
launch=00000000-0000-4000-8000-000000000201

mkdir -p "$scratch/home" "$scratch/work" "$scratch/root/bin" "$scratch/tools"
cat > "$scratch/root/bin/attention" <<EOF
#!/bin/sh
printf '%s|%s\n' "\$*" "\${WEZTERM_ATTENTION_LAUNCH_ID:-}" >> "$scratch/calls"
if [ "\$1 \$2" = "hooks claim" ]; then printf '%s\n' $launch; fi
exit 0
EOF
cat > "$scratch/tools/claude" <<'EOF'
#!/bin/sh
printf 'claude-ran launch=%s\n' "${WEZTERM_ATTENTION_LAUNCH_ID:-none}"
EOF
cat > "$scratch/tools/show-launch" <<'EOF'
#!/bin/sh
printf 'child-launch=%s\n' "${WEZTERM_ATTENTION_LAUNCH_ID:-none}"
EOF
chmod 755 "$scratch/root/bin/attention" "$scratch/tools/claude" "$scratch/tools/show-launch"

pass() { printf 'ok - %s\n' "$1"; }
fail() {
  printf 'not ok - %s\n' "$1"
  [ ! -f "$scratch/session.out" ] || sed 's/^/#   /' "$scratch/session.out"
  failures=$((failures + 1))
}

# Writes an rc file: the common setup, then each argument as one line.
rc() {
  {
    printf '%s\n' 'PS1="@P@ "' "cd '$scratch/work'" 'HISTFILE=/dev/null' \
      "export PATH='$scratch/tools':\"\$PATH\" WEZTERM_ATTENTION_ROOT='$scratch/root'"
    for line in "$@"; do printf '%s\n' "$line"; done
  } > "$scratch/rc"
}

# Types stdin into an interactive bash that reads the last rc file written.
# Leaves the output in session.out and the writer's calls in calls.
session() {
  rm -f "$scratch/calls" "$scratch/session.out"
  : > "$scratch/calls"
  env -i HOME="$scratch/home" PATH=/usr/bin:/bin TERM=dumb "$@" \
    "$python" "$driver" '@P@ ' 20 "$bash_under_test" --noprofile --rcfile "$scratch/rc" -i \
    > "$scratch/session.out" 2>&1
  session_status=$?
}

has() { grep -q -- "$1" "$scratch/session.out"; }
calls_matching() { grep -c -- "$1" "$scratch/calls"; }

printf '# %s\n' "$("$bash_under_test" -c 'printf "bash %s" "$BASH_VERSION"')"

rc "source '$integration'"
session WEZTERM_PANE=7 <<EOF
true
source '$integration'
echo alive
exit 0
EOF
# Four prompts: before each of the four lines typed.
if [ "$session_status" -eq 0 ] && has '^alive$' && [ "$(calls_matching '^hooks publish')" -eq 4 ]; then
  pass "sourcing again after the first prompt neither crashes nor adds a second hook"
else
  fail "sourcing again after the first prompt neither crashes nor adds a second hook (status $session_status)"
fi

user_preexec='preexec() { printf "user-preexec=%s\n" "$1"; }'
for order in before after "at a prompt after"; do
  typed=true
  case $order in
    before) rc "source '$integration'" "source '$bash_preexec'" "$user_preexec" ;;
    after) rc "source '$bash_preexec'" "source '$integration'" "$user_preexec" ;;
    *) rc "source '$bash_preexec'" "$user_preexec"; typed="source '$integration'" ;;
  esac
  session WEZTERM_PANE=7 <<EOF
$typed
claude
exit 0
EOF
  if [ "$session_status" -eq 0 ] && has "^claude-ran launch=$launch$" && has '^user-preexec=claude$' \
    && [ "$(calls_matching '^hooks claim|$')" -eq 1 ]; then
    pass "an agent is claimed when this file is sourced $order bash-preexec"
  else
    fail "an agent is claimed when this file is sourced $order bash-preexec (status $session_status)"
  fi
done

rc "PROMPT_COMMAND='printf \"theme-status=%s\\n\" \"\$?\"'" "source '$integration'"
session WEZTERM_PANE=7 <<'EOF'
false
exit 0
EOF
if has '^theme-status=1$'; then
  pass "a prompt command installed earlier still sees the last command's status"
else
  fail "a prompt command installed earlier still sees the last command's status"
fi

rc "trap 'case \$BASH_COMMAND in \"echo marker\") printf \"previous-trap status=%s last=%s\\n\" \"\$?\" \"\$_\";; esac' DEBUG" \
  "source '$integration'"
session WEZTERM_PANE=7 <<'EOF'
mkdir -p made && echo "last=$_"
true made; echo marker
claude
exit 0
EOF
if has '^last=made$' && has '^previous-trap status=0 last=made$' && has "^claude-ran launch=$launch$"; then
  pass "an earlier DEBUG trap keeps running beside the claim, and commands and that trap still see \$? and \$_"
else
  fail "an earlier DEBUG trap keeps running beside the claim, and commands and that trap still see \$? and \$_"
fi

rc "source '$integration'"
session <<'EOF'
true
exit 0
EOF
outside=$(calls_matching '^hooks publish')
session WEZTERM_PANE=7 <<'EOF'
true
exit 0
EOF
inside=$(calls_matching '^hooks publish')
if [ "$outside" -eq 0 ] && [ "$inside" -gt 0 ]; then
  pass "the prompt hook runs only inside a WezTerm pane"
else
  fail "the prompt hook runs only inside a WezTerm pane (outside $outside, inside $inside)"
fi

rc "source '$integration'"
session WEZTERM_PANE=7 <<'EOF'
claude
show-launch
exit 0
EOF
if has "^claude-ran launch=$launch$" && has '^child-launch=none$' \
  && [ "$(calls_matching "^hooks publish --quiet|$launch$")" -eq 1 ]; then
  pass "the claimed launch id reaches one prompt publication and no later program"
else
  fail "the claimed launch id reaches one prompt publication and no later program"
fi

# Reading a long literal command character by character took seconds before
# the command could start: in a UTF-8 locale each character read walks the
# string from its start.
long_status=0
env -i HOME="$scratch/home" PATH=/usr/bin:/bin LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  perl -e 'alarm shift; exec @ARGV or die "cannot run $ARGV[0]: $!\n"' 5 \
  "$bash_under_test" --noprofile --norc -c '
    source "$1"
    literal=$(printf "%065536d" 0)
    ! _wezterm_attention_supported_command "echo \"$literal\"" &&
      _wezterm_attention_supported_command "claude \"$literal\""
  ' _ "$integration" || long_status=$?
if [ "$long_status" -eq 0 ]; then
  pass "a command with a 64 KiB literal is classified without reading all of it"
else
  fail "a command with a 64 KiB literal is classified without reading all of it (status $long_status)"
fi

words_status=0
env -i HOME="$scratch/home" PATH=/usr/bin:/bin "$bash_under_test" --noprofile --norc -c '
  source "$1"
  for command in "claude" "M=stub claude" "MODEL=\"one two\" claude --resume" \
    "/opt/tools/claude -p x" "\"codex\" exec" "  pi" "A=a\\ b B= pi" "\"\" claude"; do
    _wezterm_attention_supported_command "$command" || { echo "rejected: $command"; exit 1; }
  done
  for command in "echo claude" "X=1" "" "claudex" "git commit -m claude"; do
    ! _wezterm_attention_supported_command "$command" || { echo "accepted: $command"; exit 1; }
  done
' _ "$integration" > "$scratch/session.out" 2>&1 || words_status=$?
if [ "$words_status" -eq 0 ]; then
  pass "the command word skips assignments and quoting"
else
  fail "the command word skips assignments and quoting"
fi

[ "$failures" -eq 0 ]
