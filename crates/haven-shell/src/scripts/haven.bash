# Haven shell integration (bash). See docs/PLAN.md section 9.3.
# bash has no clean injection point, so this is either chained via --rcfile
# (non-login shells) or sourced manually by the user (login shells).
case $- in *i*) ;; *) return 0 ;; esac
[[ -n "${__haven_integration_loaded:-}" ]] && return 0
__haven_integration_loaded=1

__haven_osc7() { printf '\e]7;file://%s%s\e\\' "${HOSTNAME}" "${PWD}"; }

__haven_precmd() {
    local status_code=$?
    if [[ -n "${__haven_executing:-}" ]]; then
        printf '\e]133;D;%s\e\\' "$status_code"
        unset __haven_executing
    fi
    __haven_osc7
    printf '\e]133;A\e\\'
}

# PS0 is emitted after a command is read but before it runs (bash >= 4.4),
# which is exactly the C mark. It avoids the fragility of a DEBUG trap.
#
# The command line comes with it, because that is what names a tab. bash
# exposes the text only in a DEBUG trap or in history, and the trap is the
# wrong place: it also fires for every PROMPT_COMMAND function, so a tab ends
# up named after the prompt rather than after the command. PS0 is expanded
# exactly once per interactive command line, which is precisely the gate
# wanted. With history off there is nothing to read and the mark carries no
# command, which falls back to the title rather than breaking.
__haven_cmdline() {
    local s
    s=$(HISTTIMEFORMAT= builtin history 1) || return 0
    # `history 1` prints "  123  the command": drop the leading space, the
    # number, and the padding after it.
    s=${s#"${s%%[![:space:]]*}"}
    s=${s#*[[:space:]]}
    s=${s#"${s%%[![:space:]]*}"}
    s=${s//'%'/%25}
    s=${s//';'/%3B}
    s=${s//$'\n'/%0A}
    s=${s//$'\e'/%1B}
    printf ';cmdline_url=%s' "$s"
}
PS0='\[\e]133;C$(__haven_cmdline)\e\\\]'"${PS0:-}"
__haven_executing_mark() { __haven_executing=1; }
PROMPT_COMMAND="__haven_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1="${PS1}"'\[\e]133;B\e\\\]'
trap '__haven_executing_mark' DEBUG
