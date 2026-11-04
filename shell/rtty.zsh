# rtty's shell integration for zsh. rtty writes this file as .zshenv in a directory of its own
# and starts the login shell with ZDOTDIR there, so zsh reads it first. It hands ZDOTDIR back at
# once: zsh goes on to read the user's own startup files, and the shell's children (another zsh,
# the shells tutti starts) inherit nothing of rtty's.
if [[ -n ${RTTY_ZDOTDIR+set} ]]; then
  ZDOTDIR=$RTTY_ZDOTDIR
  unset RTTY_ZDOTDIR
else
  unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && builtin source ${ZDOTDIR:-$HOME}/.zshenv

# Before each prompt: the end of the last command and its status (OSC 133;D), the directory
# (OSC 7), and the prompt's start (A) and end (B) put around PS1 unless the prompt marks them
# itself. Before each command: where its output starts (C). The prompt hook moves itself last,
# after a theme's that rebuilds PS1; zsh hands every hook the command's status. The end goes in
# PROMPT_SP's mark, which zsh draws right where the output ended and before any hook runs, so
# neither that mark nor a hook's own marks fall inside the output; the hook marks it only where
# PROMPT_SP is off.
if [[ -o interactive ]]; then
  _rtty_precmd() {
    local ret=$? marked=
    [[ -o prompt_cr && -o prompt_sp ]] && marked=1
    emulate -L zsh
    local LC_ALL=C c dir=
    for c in ${(s::)PWD}; do
      [[ $c == [A-Za-z0-9/._~-] ]] || printf -v c '%%%02X' "'$c"
      dir+=$c
    done
    [[ -n $marked ]] || printf '\e]133;D;%s\a' $ret
    printf '\e]7;file://%s%s\a' $HOST $dir
    [[ $PS1 == *$'\e]133;A'* ]] || PS1=$'%{\e]133;A\a%}'$PS1
    [[ $PS1 == *$'\e]133;B'* ]] || PS1+=$'%{\e]133;B\a%}'
    [[ $PROMPT_EOL_MARK == *$'\e]133;D'* ]] ||
      PROMPT_EOL_MARK=$'%{\e]133;D;%?\a%}'${PROMPT_EOL_MARK-'%B%S%#%s%b'}
    [[ ${precmd_functions[-1]} == _rtty_precmd ]] ||
      precmd_functions=(${precmd_functions:#_rtty_precmd} _rtty_precmd)
  }
  _rtty_preexec() {
    printf '\e]133;C\a'
  }
  precmd_functions+=(_rtty_precmd)
  preexec_functions+=(_rtty_preexec)
fi
