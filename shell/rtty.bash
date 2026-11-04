# rtty's shell integration for bash. rtty starts the login shell as `bash --rcfile <this file>`,
# since a login shell reads no rc file it is given: an interactive shell that is not a login one,
# so this first reads what a login shell would. Nothing of rtty's is left in the environment for
# the shell's children (another bash, the shells tutti starts).
[[ -r /etc/profile ]] && builtin source /etc/profile
for _rtty_file in ~/.bash_profile ~/.bash_login ~/.profile; do
  [[ -r $_rtty_file ]] && { builtin source "$_rtty_file"; break; }
done
unset _rtty_file

# Before each prompt, first, so the status is the command's: the end of the last command and its
# status (OSC 133;D) and the directory (OSC 7). Last, after a theme's hook: the prompt's start
# (A) and end (B) put around PS1 unless the prompt marks them itself. Where a command's output
# starts (C) takes PS0, bash 4.4 or newer; an older bash (macOS's /bin/bash is 3.2) marks it from
# a DEBUG trap, unless the user has one of their own.
_rtty_end() {
  local ret=$? LC_ALL=C c dir= i n
  for ((i = 0; i < ${#PWD}; i++)); do
    c=${PWD:i:1}
    if [[ $c != [A-Za-z0-9/._~-] ]]; then
      # bash 3.2 reads a byte past 0x7f as negative.
      printf -v n %d "'$c"
      printf -v c %%%02X $((n & 255))
    fi
    dir+=$c
  done
  printf '\e]133;D;%s\a\e]7;file://%s%s\a' "$ret" "$HOSTNAME" "$dir"
  return $ret
}
_rtty_prompt() {
  local ret=$?
  [[ $PS1 == *'133;A'* ]] || PS1='\[\e]133;A\a\]'$PS1
  [[ $PS1 == *'133;B'* ]] || PS1+='\[\e]133;B\a\]'
  _rtty_ready=1
  return $ret
}
# Newlines, not semicolons: the user's own may end in one.
PROMPT_COMMAND=$'_rtty_end\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}_rtty_prompt
if ((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 404)); then
  PS0+='\e]133;C\a'
elif [[ -z $(trap -p DEBUG) ]]; then
  # Before the first command after a prompt, unless that is the prompt's own: Enter on an empty
  # line runs no command.
  _rtty_preexec() {
    if [[ -n $_rtty_ready ]]; then
      _rtty_ready=
      [[ $BASH_COMMAND == _rtty_end ]] || printf '\e]133;C\a'
    fi
    return 0
  }
  trap _rtty_preexec DEBUG
fi
