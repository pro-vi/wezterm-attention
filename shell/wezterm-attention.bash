# Source this file from bash after WEZTERM_ATTENTION_ROOT is set.

: "${WEZTERM_ATTENTION_COMMANDS:=claude codex pi}"
_WEZTERM_ATTENTION_IN_HOOK=0

_wezterm_attention_split_words() {
  _wezterm_attention_words=()
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
      if [ -n "$token" ]; then _wezterm_attention_words+=("$token"); token=; fi
    else
      token+=$character
    fi
  done
  [ -z "$token" ] || _wezterm_attention_words+=("$token")
}

_wezterm_attention_supported_command() {
  local first word
  _wezterm_attention_split_words "$1"
  for word in "${_wezterm_attention_words[@]}"; do
    [[ "$word" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] && continue
    first=${word##*/}
    first=${first#\"}; first=${first%\"}
    first=${first#\'}; first=${first%\'}
    break
  done
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
  _WEZTERM_ATTENTION_IN_HOOK=1
  local selected_launch= claim_status=0
  unset WEZTERM_ATTENTION_LAUNCH_ID
  selected_launch=$("$WEZTERM_ATTENTION_ROOT/bin/attention" hooks claim) || claim_status=$?
  if [ "$claim_status" -eq 0 ] && [ -n "$selected_launch" ]; then
    export WEZTERM_ATTENTION_LAUNCH_ID=$selected_launch
  else
    unset WEZTERM_ATTENTION_LAUNCH_ID
  fi
  _WEZTERM_ATTENTION_IN_HOOK=0
  return 0
}

wezterm_attention_precmd() {
  [ "$_WEZTERM_ATTENTION_IN_HOOK" -eq 0 ] || return 0
  [ -n "${WEZTERM_ATTENTION_ROOT:-}" ] && [ -x "$WEZTERM_ATTENTION_ROOT/bin/attention" ] || return 0
  _WEZTERM_ATTENTION_IN_HOOK=1
  "$WEZTERM_ATTENTION_ROOT/bin/attention" hooks publish --quiet || true
  _WEZTERM_ATTENTION_IN_HOOK=0
}

_WEZTERM_ATTENTION_DEBUG_INSTALLED=0
_WEZTERM_ATTENTION_PREVIOUS_DEBUG_DEFINITION=
_WEZTERM_ATTENTION_PREVIOUS_DEBUG=
_wezterm_attention_debug_dispatch() {
  local command_text=$1
  [ "${#FUNCNAME[@]}" -eq 1 ] && wezterm_attention_preexec "$command_text"
  [ -z "$_WEZTERM_ATTENTION_PREVIOUS_DEBUG" ] || eval "$_WEZTERM_ATTENTION_PREVIOUS_DEBUG"
}
_WEZTERM_ATTENTION_PROMPT_INSTALL='if [ "$_WEZTERM_ATTENTION_DEBUG_INSTALLED" -eq 0 ]; then _WEZTERM_ATTENTION_PREVIOUS_DEBUG_DEFINITION=$(trap -p DEBUG); trap - DEBUG; if [ -n "$_WEZTERM_ATTENTION_PREVIOUS_DEBUG_DEFINITION" ]; then _WEZTERM_ATTENTION_PREVIOUS_DEBUG_QUOTED=${_WEZTERM_ATTENTION_PREVIOUS_DEBUG_DEFINITION#trap -- }; _WEZTERM_ATTENTION_PREVIOUS_DEBUG_QUOTED=${_WEZTERM_ATTENTION_PREVIOUS_DEBUG_QUOTED% DEBUG}; eval "_WEZTERM_ATTENTION_PREVIOUS_DEBUG=$_WEZTERM_ATTENTION_PREVIOUS_DEBUG_QUOTED"; fi; trap "_wezterm_attention_debug_dispatch \"\$BASH_COMMAND\"" DEBUG; _WEZTERM_ATTENTION_DEBUG_INSTALLED=1; fi'
PROMPT_COMMAND="$_WEZTERM_ATTENTION_PROMPT_INSTALL;wezterm_attention_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
