# Shell integration for nixdex / nix-locate under fish.
# Source this file from your fish profile to get command-not-found suggestions.

function __fish_command_not_found_handler --on-event fish_command_not_found
    if not isatty stdout
        echo "$argv[1]: command not found" >&2
        return 127
    end

    if test (count $argv) -eq 0
        return 127
    end

    set -l db ""
    if set -q NIXDEX_DATABASE
        set db "$NIXDEX_DATABASE"
    else if set -q NIX_INDEX_DATABASE
        set db "$NIX_INDEX_DATABASE"
    end

    if test -n "$db"
        @out@/bin/nixdex command-not-found --db "$db" $argv
    else
        @out@/bin/nixdex command-not-found $argv
    end
    return $status
end
