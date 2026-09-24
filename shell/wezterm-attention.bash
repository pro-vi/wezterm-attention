# Source this file from bash after WEZTERM_ATTENTION_ROOT is set.

# `source ~/.bashrc` after an edit sources this file again. A second copy of
# the hooks would treat the first copy's DEBUG trap as someone else's and call
# it from itself, which recurses until bash crashes, so a repeat installs
# nothing new. It only puts back this file's prompt command, which an rc file
# that assigns PROMPT_COMMAND instead of appending to it has just removed;
# without it nothing publishes and a claimed launch id is never unset.
if [ -n "${_WEZTERM_ATTENTION_LOADED:-}" ]; then
  _wezterm_attention_add_prompt_command
  return 0
fi
_WEZTERM_ATTENTION_LOADED=1

: "${WEZTERM_ATTENTION_COMMANDS:=claude codex pi}"
# 1 while a hook runs the writer. The hooks set it only as a local, which bash
# puts back however the function ends: Ctrl-C during a slow claim must not
# leave it at 1 and switch the hooks off for the rest of the shell.
_WEZTERM_ATTENTION_IN_HOOK=0

# Sets _wezterm_attention_command_word to the first word of a simple command
# that is not a NAME=value assignment, with shell quoting removed. It stops at
# that word: the rest of the command can be kilobytes of literal text, and
# every character read costs time before the command starts.
_wezterm_attention_find_command_word() {
  _wezterm_attention_command_word=
  local input=$1 token= quote= character index escaped=0
  for ((index = 0; index < ${#input}; index++)); do
    character=${input:index:1}
    if [ "$escaped" -eq 1 ]; then token+=$character; escaped=0; continue; fi
    if [ "$character" = "\\" ]; then escaped=1; continue; fi
    if [ -n "$quote" ]; then
      if [ "$character" = "$quote" ]; then quote=; else token+=$character; fi
      continue
    fi
    if [ "$character" = "\"" ] || [ "$character" = "'" ]; then quote=$character; continue; fi
    if [[ "$character" == [[:space:]] ]]; then
      [ -n "$token" ] || continue
      [[ "$token" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] || break
      token=
    else
      token+=$character
    fi
  done
  [[ "$token" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] || _wezterm_attention_command_word=$token
}

_wezterm_attention_supported_command() {
  local first
  _wezterm_attention_find_command_word "$1"
  first=${_wezterm_attention_command_word##*/}
  first=${first#\"}; first=${first%\"}
  first=${first#\'}; first=${first%\'}
  [ -n "$first" ] || return 1
  local supported
  for supported in $WEZTERM_ATTENTION_COMMANDS; do
    [ "$first" = "$supported" ] && return 0
  done
  return 1
}

wezterm_attention_preexec() {
  [ "$_WEZTERM_ATTENTION_IN_HOOK" -eq 0 ] || return 0
  [ "${BASH_SUBSHELL:-0}" -eq 0 ] || return 0
  _wezterm_attention_supported_command "${1:-$BASH_COMMAND}" || return 0
  [ -n "${WEZTERM_ATTENTION_ROOT:-}" ] && [ -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ] || return 0
  local _WEZTERM_ATTENTION_IN_HOOK=1 selected_launch= claim_status=0
  unset WEZTERM_ATTENTION_LAUNCH_ID
  selected_launch=$("$WEZTERM_ATTENTION_ROOT/bin/attention" hooks claim) || claim_status=$?
  if [ "$claim_status" -eq 0 ] && [ -n "$selected_launch" ]; then
    export WEZTERM_ATTENTION_LAUNCH_ID=$selected_launch
  else
    unset WEZTERM_ATTENTION_LAUNCH_ID
  fi
  return 0
}

wezterm_attention_precmd() {
  [ "$_WEZTERM_ATTENTION_IN_HOOK" -eq 0 ] || return 0
  # Outside a WezTerm pane there is nothing to publish, and the writer would
  # say so at every prompt.
  [ -n "${WEZTERM_PANE:-}" ] || return 0
  [ -n "${WEZTERM_ATTENTION_ROOT:-}" ] && [ -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ] || return 0
  local _WEZTERM_ATTENTION_IN_HOOK=1
  "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks publish --quiet || true
  # That publication recorded the claimed agent's return to this prompt, which
  # is the last use of its launch id here. Left exported, the id would pass to
  # the next program this shell starts, and an agent the claim did not detect
  # would read as a child of one that has already exited.
  unset WEZTERM_ATTENTION_LAUNCH_ID
}

# Sets $? to $1. Its last argument also becomes $_ for the next command.
_wezterm_attention_set_status() {
  return "$1"
}

_WEZTERM_ATTENTION_PREVIOUS_DEBUG=
# How many names FUNCNAME holds in a function the shell itself calls, from
# PROMPT_COMMAND or the DEBUG trap. That is 1, except that after Ctrl-C stops a
# function inside a trap, bash 3.2 keeps the stopped functions' names on
# FUNCNAME for the rest of the shell. The prompt command measures it again at
# each prompt, so an interrupted claim does not stop every later one.
_WEZTERM_ATTENTION_TOP_DEPTH=1
_wezterm_attention_debug_dispatch() {
  local last_status=$? command_text=$1 last_argument=$2
  [ "${#FUNCNAME[@]}" -eq "$_WEZTERM_ATTENTION_TOP_DEPTH" ] && wezterm_attention_preexec "$command_text"
  if [ -n "$_WEZTERM_ATTENTION_PREVIOUS_DEBUG" ]; then
    # The trap this one replaced sees the $? and $_ it would have seen alone.
    _wezterm_attention_set_status "$last_status" "$last_argument"
    eval "$_WEZTERM_ATTENTION_PREVIOUS_DEBUG"
  fi
}

# Takes the output of `trap -p DEBUG` and returns 0 when the caller should now
# set the DEBUG trap. Both halves happen at the top level, in
# _WEZTERM_ATTENTION_PROMPT_INSTALL below: inside a function bash hides the
# DEBUG trap from `trap -p`, and bash 3.2 puts back the DEBUG trap a function
# replaced when that function returns.
_wezterm_attention_install_hooks() {
  [ -n "$_WEZTERM_ATTENTION_PROMPT_INSTALL" ] || return 1
  _WEZTERM_ATTENTION_PROMPT_INSTALL=
  # bash-preexec owns the DEBUG trap and calls the trap it found from inside
  # its own function, where the dispatcher's top-level check never passes.
  # It calls preexec functions once per command line instead, with that line.
  if [ -n "${bash_preexec_imported:-}${__bp_imported:-}" ]; then
    preexec_functions+=(wezterm_attention_preexec)
    return 1
  fi
  local definition=$1 quoted
  case $definition in
    *_wezterm_attention_debug_dispatch*) definition= ;;
  esac
  if [ -n "$definition" ]; then
    quoted=${definition#trap -- }
    quoted=${quoted% DEBUG}
    eval "_WEZTERM_ATTENTION_PREVIOUS_DEBUG=$quoted"
  fi
  return 0
}

# Evaluating this at the top level installs the hooks without waiting for a
# prompt. $_ is passed to the trap last so that it is also the trap command's
# last argument, which is what bash leaves in $_ for the command it ran before.
_WEZTERM_ATTENTION_PROMPT_INSTALL='_wezterm_attention_install_hooks "$(trap -p DEBUG)" &&
  trap '"'"'_wezterm_attention_debug_dispatch "$BASH_COMMAND" "$_"'"'"' DEBUG'

# Installing at the first prompt rather than here lets a DEBUG trap set later
# in the startup files be kept and called. The status of the command before the
# prompt is handed on, so prompt commands after these still see it.
_wezterm_attention_prompt_command() {
  _WEZTERM_ATTENTION_TOP_DEPTH=${#FUNCNAME[@]}
  wezterm_attention_precmd
  return "$_WEZTERM_ATTENTION_LAST_STATUS"
}
_wezterm_attention_add_prompt_command() {
  case "${PROMPT_COMMAND[*]:-}" in
    *_wezterm_attention_prompt_command*) return 0 ;;
  esac
  PROMPT_COMMAND="_WEZTERM_ATTENTION_LAST_STATUS=\$?;eval \"\$_WEZTERM_ATTENTION_PROMPT_INSTALL\";_wezterm_attention_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
}
_wezterm_attention_add_prompt_command
