# Haven shell integration (fish). Loaded from a vendor conf.d directory, so the
# user's own config is never touched. See docs/PLAN.md section 9.3.
#
# fish >= 3.7 emits the full OSC 133 A/B/C/D set and OSC 7 itself, so this file
# must do nothing there: adding our marks on top produces two command-end marks
# per command, which breaks prompt navigation for any consumer. Haven detects
# that case and does not even install this file; the guard below is defence in
# depth for versions we have not seen.
status is-interactive; or exit 0
if set -q __haven_integration_loaded
    exit 0
end

set -l v (string split '.' -- $version)
if test (count $v) -ge 2 -a \( "$v[1]" -gt 3 -o \( "$v[1]" -eq 3 -a "$v[2]" -ge 7 \) \)
    exit 0
end

set -g __haven_integration_loaded 1

function __haven_osc7 --on-variable PWD
    printf '\e]7;file://%s%s\e\\' (hostname) "$PWD"
end

# Marks are emitted from events, never by copying and wrapping fish_prompt:
# conf.d runs *before* the user's config.fish, so a wrapper installed here is
# overwritten the moment the user defines their own prompt.
#
# Known limitation on fish < 3.7: there is no hook that fires after the prompt
# has been drawn, so B (prompt end / input start) is not emitted. Everything
# that needs B — selecting a command's output — therefore requires fish >= 3.7,
# where it is native anyway.
function __haven_prompt_start --on-event fish_prompt
    printf '\e]133;A\e\\'
end

function __haven_preexec --on-event fish_preexec
    printf '\e]133;C\e\\'
end

function __haven_postexec --on-event fish_postexec
    printf '\e]133;D;%s\e\\' $status
end

__haven_osc7
