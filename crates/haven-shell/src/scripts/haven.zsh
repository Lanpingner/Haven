# Haven shell integration (zsh). Sourced from a ZDOTDIR shim that chains the
# user's real .zshrc. See docs/PLAN.md section 9.3.
[[ -o interactive ]] || return 0
(( ${+__haven_integration_loaded} )) && return 0
typeset -g __haven_integration_loaded=1

__haven_osc7() { printf '\e]7;file://%s%s\e\\' "${HOST}" "${PWD}" }

__haven_precmd() {
    local status_code=$?
    # D carries the *previous* command's exit status, so it must come first and
    # must not fire before any command has run.
    (( ${+__haven_executing} )) && printf '\e]133;D;%s\e\\' "$status_code"
    unset __haven_executing
    __haven_osc7
    printf '\e]133;A\e\\'
}

__haven_preexec() {
    typeset -g __haven_executing=1
    printf '\e]133;C\e\\'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __haven_precmd
add-zsh-hook preexec __haven_preexec

# B marks the end of the prompt / start of user input. zsh has no "after the
# prompt was printed" hook, so it is appended to PS1 as a zero-width escape.
PS1="${PS1}"$'%{\e]133;B\e\\%}'
