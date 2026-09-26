#!/usr/bin/env sh
# Drives shell/wezterm-attention.zsh in zsh with an empty environment and a
# stand-in writer that records its calls.
#
# Usage: zsh_integration_spec.sh [ZSH]
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
zsh_under_test=${1:-zsh}
integration="$root/shell/wezterm-attention.zsh"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-zsh-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
failures=0

mkdir -p "$scratch/root/bin"
cat > "$scratch/root/bin/attention" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >> "$scratch/calls"
printf '%s\n' 'attention: identity_unpublished: stand-in failure' >&2
exit 3
EOF
chmod 755 "$scratch/root/bin/attention"

check() {
  name=$1
  shift
  rm -f "$scratch/calls"
  : > "$scratch/calls"
  if env -i HOME="$scratch" PATH=/usr/bin:/bin "$@" "$zsh_under_test" -f -c "$script" \
    > "$scratch/out" 2>&1; then
    printf 'ok - %s\n' "$name"
  else
    printf 'not ok - %s\n' "$name"
    sed 's/^/#   /' "$scratch/out"
    failures=$((failures + 1))
  fi
}

printf '# %s\n' "$("$zsh_under_test" --version)"

script="source '$integration'
wezterm_attention_claim 2>'$scratch/claim.err' || exit 1
[[ -z \${WEZTERM_ATTENTION_LAUNCH_ID:-} && ! -s '$scratch/claim.err' ]] || exit 1
unset WEZTERM_ATTENTION_ROOT
wezterm_attention_claim"
check "outside a pane a failed claim still lets the agent start, quietly" \
  WEZTERM_ATTENTION_ROOT="$scratch/root" WEZTERM_ATTENTION_LAUNCH_ID=00000000-0000-4000-8000-000000000999

script="source '$integration'
if wezterm_attention_claim; then exit 1; fi
unset WEZTERM_ATTENTION_ROOT
if wezterm_attention_claim; then exit 1; fi
exit 0"
check "inside a pane a failed claim is reported to the caller" \
  WEZTERM_ATTENTION_ROOT="$scratch/root" WEZTERM_PANE=7

script="source '$integration'
source '$integration'
[[ \"\${precmd_functions[*]}\" == wezterm_attention_precmd ]] || exit 1
wezterm_attention_precmd 2>'$scratch/precmd.err'
[[ ! -s '$scratch/calls' && ! -s '$scratch/precmd.err' ]]"
check "the prompt hook is installed once and stays silent outside a pane" \
  WEZTERM_ATTENTION_ROOT="$scratch/root"

script="source '$integration'
wezterm_attention_precmd 2>/dev/null
grep -q '^hooks publish --quiet\$' '$scratch/calls'"
check "the prompt hook publishes inside a pane" WEZTERM_ATTENTION_ROOT="$scratch/root" WEZTERM_PANE=7

# A stand-in writer whose claim succeeds, for the launch id's lifetime.
launch=00000000-0000-4000-8000-000000000123
mkdir -p "$scratch/claiming/bin"
cat > "$scratch/claiming/bin/attention" <<EOF
#!/bin/sh
[ "\$*" = "hooks claim" ] && printf '%s\n' $launch
exit 0
EOF
chmod 755 "$scratch/claiming/bin/attention"
# Prints NAME=<the launch id this program inherited>.
report_launch="sh -c 'printf \"%s=%s\\\\n\" \"\$0\" \"\${WEZTERM_ATTENTION_LAUNCH_ID:-}\"'"

# Prompt hooks run only in an interactive shell, so these type their lines
# into one.
check_typed() {
  name=$1
  expected=$2
  rm -f "$scratch/seen"
  : > "$scratch/seen"
  if printf '%s\n' "$lines" | env -i HOME="$scratch" PATH=/usr/bin:/bin \
      WEZTERM_ATTENTION_ROOT="$scratch/claiming" WEZTERM_PANE=7 \
      "$zsh_under_test" -f -i > "$scratch/out" 2>&1 \
      && [ "$(cat "$scratch/seen")" = "$expected" ]; then
    printf 'ok - %s\n' "$name"
  else
    printf 'not ok - %s\n' "$name"
    sed 's/^/#   seen: /' "$scratch/seen"
    failures=$((failures + 1))
  fi
}

lines="source '$integration'
wezterm_attention_claim && $report_launch agent >> '$scratch/seen'
$report_launch next >> '$scratch/seen'
exit"
check_typed "a claim and its agent on one line leave no launch id behind" "agent=$launch
next="

lines="source '$integration'
  wezterm_attention_claim

$report_launch agent >> '$scratch/seen'
$report_launch next >> '$scratch/seen'
exit"
check_typed "a claim alone on its line keeps the launch id for the agent on the next" \
  "agent=$launch
next="

# The preexec hook runs before every command, and a pasted heredoc can make
# the line hundreds of kilobytes long. Stripping trailing blanks with a
# pattern tries every suffix of the line, which takes time quadratic in it.
cat > "$scratch/long-line.zsh" <<'EOF'
source "$1"
# Zsh runs preexec hooks before each line of a script too, which would
# overwrite what the calls below record.
add-zsh-hook -d preexec wezterm_attention_preexec
line="  cat <<X ${(l:524288::a:)}  "
wezterm_attention_preexec "$line" "$line" "$line"
[[ $_WEZTERM_ATTENTION_LAST_LINE == other ]] || exit 1
line=$' \t wezterm_attention_claim \n'
wezterm_attention_preexec "$line" "$line" "$line"
[[ $_WEZTERM_ATTENTION_LAST_LINE == claim ]]
EOF
long_status=0
env -i HOME="$scratch" PATH=/usr/bin:/bin \
  perl -e 'alarm shift; exec @ARGV or die "cannot run $ARGV[0]: $!\n"' 5 \
  "$zsh_under_test" -f "$scratch/long-line.zsh" "$integration" > "$scratch/out" 2>&1 || long_status=$?
if [ "$long_status" -eq 0 ]; then
  printf 'ok - %s\n' "a 512 KiB command line is classified in under five seconds"
else
  printf 'not ok - %s\n' "a 512 KiB command line is classified in under five seconds (status $long_status)"
  sed 's/^/#   /' "$scratch/out"
  failures=$((failures + 1))
fi

# Ctrl-C while the writer runs stops that one claim or publication, and the
# ones after it still happen. A person types Ctrl-C at a terminal, so these
# drive zsh through one, with startup files in a scratch ZDOTDIR.
driver="$root/tests/python/interactive_shell.py"
python=${ATTENTION_TEST_PYTHON3:-$(command -v python3)}
mkdir -p "$scratch/slow/bin" "$scratch/tools" "$scratch/zdot"
cat > "$scratch/slow/bin/attention" <<EOF
#!/bin/sh
printf '%s|%s\n' "\$*" "\${WEZTERM_ATTENTION_LAUNCH_ID:-}" >> "$scratch/calls"
# The first call named in slow-call after the number of them in slow-skip
# hangs until it is interrupted, and records it when it was not.
if [ "\$1 \$2" = "\$(cat "$scratch/slow-call")" ] && [ ! -e "$scratch/slowed" ]; then
  echo x >> "$scratch/slow-seen"
fi
if [ "\$1 \$2" = "\$(cat "$scratch/slow-call")" ] && [ ! -e "$scratch/slowed" ] \
  && [ "\$(wc -l < "$scratch/slow-seen")" -gt "\$(cat "$scratch/slow-skip")" ]; then
  : > "$scratch/slowed"
  printf 'writer-waiting\n' >&2
  sleep 10
  : > "$scratch/slow-finished"
fi
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
chmod 755 "$scratch/slow/bin/attention" "$scratch/tools/claude" "$scratch/tools/show-launch"
printf '%s\n' 'unsetopt global_rcs' > "$scratch/zdot/.zshenv"
printf '%s\n' "PS1='@P@ '" 'HISTFILE=/dev/null' \
  "export PATH='$scratch/tools':\"\$PATH\" WEZTERM_ATTENTION_ROOT='$scratch/slow'" \
  "source '$integration'" > "$scratch/zdot/.zshrc"
for slow_call in "hooks claim" "hooks publish"; do
  printf '%s\n' "$slow_call" > "$scratch/slow-call"
  printf '0\n' > "$scratch/slow-skip"
  rm -f "$scratch/calls" "$scratch/slowed" "$scratch/slow-finished" "$scratch/slow-seen"
  : > "$scratch/calls"
  typed_status=0
  env -i HOME="$scratch" ZDOTDIR="$scratch/zdot" PATH=/usr/bin:/bin TERM=dumb WEZTERM_PANE=7 \
    "$python" "$driver" --interrupt-on writer-waiting '@P@ ' 20 "$zsh_under_test" -i \
    > "$scratch/out" 2>&1 <<'EOF' || typed_status=$?
wezterm_attention_claim && claude
wezterm_attention_claim && claude
show-launch
exit
EOF
  if [ "$typed_status" -eq 0 ] && [ -e "$scratch/slowed" ] && [ ! -e "$scratch/slow-finished" ] \
    && grep -q "^claude-ran launch=$launch$" "$scratch/out" && grep -q '^child-launch=none$' "$scratch/out" \
    && grep -q "^hooks publish --quiet|$launch$" "$scratch/calls"; then
    printf 'ok - %s\n' "an interrupted $slow_call leaves later claims and publications working"
  else
    printf 'not ok - %s\n' "an interrupted $slow_call leaves later claims and publications working (status $typed_status)"
    sed 's/^/#   /' "$scratch/out"
    failures=$((failures + 1))
  fi
done

# The publication at the agent's return is the second one: the first runs at
# the shell's first prompt. Stopping it must still drop the agent's launch id,
# or the next program would inherit it.
printf '%s\n' "hooks publish" > "$scratch/slow-call"
printf '1\n' > "$scratch/slow-skip"
rm -f "$scratch/calls" "$scratch/slowed" "$scratch/slow-finished" "$scratch/slow-seen"
: > "$scratch/calls"
typed_status=0
env -i HOME="$scratch" ZDOTDIR="$scratch/zdot" PATH=/usr/bin:/bin TERM=dumb WEZTERM_PANE=7 \
  "$python" "$driver" --interrupt-on writer-waiting '@P@ ' 20 "$zsh_under_test" -i \
  > "$scratch/out" 2>&1 <<'EOF' || typed_status=$?
wezterm_attention_claim && claude
show-launch
exit
EOF
if [ "$typed_status" -eq 0 ] && [ -e "$scratch/slowed" ] && [ ! -e "$scratch/slow-finished" ] \
  && grep -q "^claude-ran launch=$launch$" "$scratch/out" && grep -q '^child-launch=none$' "$scratch/out"; then
  printf 'ok - %s\n' "an interrupted publication at the agent's return still drops its launch id"
else
  printf 'not ok - %s\n' "an interrupted publication at the agent's return still drops its launch id (status $typed_status)"
  sed 's/^/#   /' "$scratch/out"
  failures=$((failures + 1))
fi

[ "$failures" -eq 0 ]
