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

[ "$failures" -eq 0 ]
