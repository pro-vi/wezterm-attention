# Source this file from zsh after WEZTERM_ATTENTION_ROOT is set.
#
# Zsh exposes an AND-list such as `claude && codex` as one DEBUG event, so it
# cannot rotate the parent-shell launch id once per executed command. The v2
# contract therefore uses its explicit manual fallback on zsh.

# 1 while a hook runs the writer. The hooks set it only as a local, which zsh
# puts back however the function ends: Ctrl-C during a slow claim or
# publication must not leave it at 1 and switch the hooks off for the rest of
# the shell.
typeset -g _WEZTERM_ATTENTION_IN_HOOK=0
# What the command line that just ran was: "" when none ran since the last
# prompt (an empty line), "claim" when it held only the claim, else "other".
typeset -g _WEZTERM_ATTENTION_LAST_LINE=

# Returns the claim's failure only inside a WezTerm pane. Outside one there is
# no pane to claim, and `wezterm_attention_claim && claude` must still start
# the agent.
wezterm_attention_claim() {
  (( _WEZTERM_ATTENTION_IN_HOOK == 0 )) || return 0
  local in_pane=0
  [[ -n "${WEZTERM_PANE:-}" ]] && in_pane=1
  if [[ -z "${WEZTERM_ATTENTION_ROOT:-}" || ! -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]]; then
    (( in_pane )) && return 1
    return 0
  fi
  local _WEZTERM_ATTENTION_IN_HOOK=1 selected_launch claim_status=0
  unset WEZTERM_ATTENTION_LAUNCH_ID
  if (( in_pane )); then
    selected_launch=$("$WEZTERM_ATTENTION_ROOT/bin/attention" hooks claim) || claim_status=$?
  else
    selected_launch=$("$WEZTERM_ATTENTION_ROOT/bin/attention" hooks claim 2>/dev/null) || claim_status=$?
  fi
  if (( claim_status == 0 )) && [[ -n "$selected_launch" ]]; then
    export WEZTERM_ATTENTION_LAUNCH_ID=$selected_launch
  else
    unset WEZTERM_ATTENTION_LAUNCH_ID
    (( claim_status != 0 )) || claim_status=1
    (( in_pane )) || claim_status=0
  fi
  return $claim_status
}

# The third argument is the line as it runs, aliases expanded.
wezterm_attention_preexec() {
  setopt local_options extended_glob
  local line=${3:-$1}
  line=${${line##[[:space:]]#}%%[[:space:]]#}
  if [[ $line == wezterm_attention_claim ]]; then
    typeset -g _WEZTERM_ATTENTION_LAST_LINE=claim
  else
    typeset -g _WEZTERM_ATTENTION_LAST_LINE=other
  fi
}

wezterm_attention_precmd() {
  (( _WEZTERM_ATTENTION_IN_HOOK == 0 )) || return 0
  local last_line=$_WEZTERM_ATTENTION_LAST_LINE
  typeset -g _WEZTERM_ATTENTION_LAST_LINE=
  # Outside a WezTerm pane there is nothing to publish, and the writer would
  # say so at every prompt.
  [[ -n "${WEZTERM_PANE:-}" ]] || return 0
  [[ -n "${WEZTERM_ATTENTION_ROOT:-}" && -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ]] || return 0
  local _WEZTERM_ATTENTION_IN_HOOK=1
  # After `wezterm_attention_claim && claude` this prompt is the agent's
  # return, the last use of its launch id here; left exported, the id would
  # pass to the next program this shell starts, and an agent the claim did
  # not start would read as a child of one that has exited. A line holding
  # only the claim is the other way to claim, with the agent on the next
  # line, and an empty line runs nothing: both keep the id. The unset sits in
  # `always` so that Ctrl-C during the publication does not skip it.
  {
    "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks publish --quiet || true
  } always {
    [[ $last_line == other ]] && unset WEZTERM_ATTENTION_LAUNCH_ID
  }
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec wezterm_attention_preexec
add-zsh-hook precmd wezterm_attention_precmd
