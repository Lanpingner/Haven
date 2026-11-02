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
PS0='\[\e]133;C\e\\\]'"${PS0:-}"
__haven_executing_mark() { __haven_executing=1; }
PROMPT_COMMAND="__haven_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1="${PS1}"'\[\e]133;B\e\\\]'
trap '__haven_executing_mark' DEBUG
