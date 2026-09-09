# Source this file from zsh after WEZTERM_ATTENTION_ROOT is set.
#
# Zsh exposes an AND-list such as `claude && codex` as one DEBUG event, so it
# cannot rotate the parent-shell launch id once per executed command. The v2
# contract therefore uses its explicit manual fallback on zsh.

typeset -g _WEZTERM_ATTENTION_IN_HOOK=0

wezterm_attention_claim() {
  (( _WEZTERM_ATTENTION_IN_HOOK == 0 )) || return 0
  [[ -n "${WEZTERM_ATTENTION_ROOT:-}" && -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]] || return 1
  typeset -g _WEZTERM_ATTENTION_IN_HOOK=1
  local selected_launch claim_status=0
  unset WEZTERM_ATTENTION_LAUNCH_ID
  selected_launch=$("$WEZTERM_ATTENTION_ROOT/bin/attention" hooks claim) || claim_status=$?
  if (( claim_status == 0 )) && [[ -n "$selected_launch" ]]; then
    export WEZTERM_ATTENTION_LAUNCH_ID=$selected_launch
  else
    unset WEZTERM_ATTENTION_LAUNCH_ID
    (( claim_status != 0 )) || claim_status=1
  fi
  typeset -g _WEZTERM_ATTENTION_IN_HOOK=0
  return $claim_status
}

wezterm_attention_precmd() {
  (( _WEZTERM_ATTENTION_IN_HOOK == 0 )) || return 0
  [[ -n "${WEZTERM_ATTENTION_ROOT:-}" && -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]] || return 0
  typeset -g _WEZTERM_ATTENTION_IN_HOOK=1
  "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks publish --quiet || true
  typeset -g _WEZTERM_ATTENTION_IN_HOOK=0
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd wezterm_attention_precmd
