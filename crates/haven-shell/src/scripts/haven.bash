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

# The command line, for the `C` mark, because that is what names a tab.
#
# bash exposes the text of the line only in a DEBUG trap or in history, and
# `$BASH_COMMAND` in the trap is per *command*, not per line — `echo a; echo
# b` would name a tab `echo a`. History has the line as typed. With history
# off there is nothing to read and the mark carries no command, which falls
# back to the title rather than breaking.
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

__haven_executing_mark() { __haven_executing=1; }

# How the `C` mark gets emitted, which depends on how old bash is.
#
# `PS0` is expanded after a command line is read and before it runs, exactly
# once per line — precisely the gate wanted. It arrived in bash 4.4.
#
# macOS still ships bash 3.2, the last GPLv2 release, from 2007. There `PS0`
# does not exist, so the `C` mark simply never fired: prompt marks worked,
# `OSC 133;D` worked, and every pane reported no command at all. Which is
# most of what shell integration is for.
__haven_has_ps0() {
    # `HAVEN_NO_PS0` forces the old path, which is how the old path is
    # tested on a machine that has a new bash. There is no bash 3.2 to hand
    # on most of them, and an untested fallback is not a fallback.
    [[ -n "${HAVEN_NO_PS0:-}" ]] && return 1
    local major=${BASH_VERSINFO[0]:-0} minor=${BASH_VERSINFO[1]:-0}
    (( major > 4 || (major == 4 && minor >= 4) ))
}

if __haven_has_ps0; then
    PS0='\[\e]133;C$(__haven_cmdline)\e\\\]'"${PS0:-}"
    PROMPT_COMMAND="__haven_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
    trap '__haven_executing_mark' DEBUG
else
    # Without `PS0`, the DEBUG trap is the only thing that runs between
    # reading a line and running it. It cannot be used naively: it also
    # fires for every command inside `PROMPT_COMMAND`, so the mark would
    # carry the prompt's own work and a tab would be named after it.
    #
    # So it is armed, and the thing that arms it is appended to
    # `PROMPT_COMMAND` — last, after anything the user had there. The next
    # DEBUG after that is the line the user typed. Arming also fires DEBUG
    # for the arming command itself, which is harmless: it is not armed yet.
    __haven_arm() { __haven_armed=1; }
    __haven_debug() {
        __haven_executing=1
        if [[ -n "${__haven_armed:-}" ]]; then
            unset __haven_armed
            printf '\e]133;C%s\e\\' "$(__haven_cmdline)"
        fi
    }
    PROMPT_COMMAND="__haven_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; __haven_arm"
    trap '__haven_debug' DEBUG
fi
PS1="${PS1}"'\[\e]133;B\e\\\]'
