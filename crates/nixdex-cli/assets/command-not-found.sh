#!/usr/bin/env bash

# Shell integration for nixdex / nix-locate.
# Source this file from your shell profile to get command-not-found suggestions.

command_not_found_handle () {
    # Do not run when inside Midnight Commander or within a pipe.
    if [ -n "${MC_SID-}" ] || ! [ -t 1 ]; then
        echo "$1: command not found" >&2
        return 127
    fi

    local toplevel=nixpkgs
    local cmd=$1
    local attrs
    attrs=$(@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root "/bin/$cmd")
    local len
    len=$(printf '%s\n' "$attrs" | grep -c .)

    case $len in
        0)
            echo "$cmd: command not found" >&2
            ;;
        1)
            if [ -n "${NIX_AUTO_INSTALL-}" ]; then
                cat >&2 <<EOF
The program '$cmd' is currently not installed. It is provided by
the package '$toplevel.$attrs', which I will now install for you.
EOF
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    if nix profile install "$toplevel#$attrs"; then
                        "$@"
                        return
                    fi
                else
                    if nix-env -iA "$toplevel.$attrs"; then
                        "$@"
                        return
                    fi
                fi
                cat >&2 <<EOF
Failed to install $toplevel.$attrs.
$cmd: command not found
EOF
            elif [ -n "${NIX_AUTO_RUN-}" ]; then
                if nix-build --no-out-link -A "$attrs" "<$toplevel>"; then
                    local escaped
                    printf -v escaped '%q ' "$@"
                    nix-shell -p "$attrs" --run "${escaped% }"
                    return
                else
                    cat >&2 <<EOF
Failed to build $toplevel.$attrs.
$cmd: command not found
EOF
                fi
            else
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    cat >&2 <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix profile install $toplevel#$attrs

Or run it once with:
  nix shell $toplevel#$attrs -c $cmd ...
EOF
                else
                    cat >&2 <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix-env -iA $toplevel.$attrs

Or run it once with:
  nix-shell -p $attrs --run '$cmd ...'
EOF
                fi
            fi
            ;;
        *)
            cat >&2 <<EOF
The program '$cmd' is currently not installed. It is provided by
several packages. You can install it by typing one of the following:
EOF
            while read -r attr; do
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    echo "  nix profile install $toplevel#$attr" >&2
                else
                    echo "  nix-env -iA $toplevel.$attr" >&2
                fi
            done <<EOF
$attrs
EOF
            cat >&2 <<EOF

Or run it once with:
EOF
            while read -r attr; do
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    echo "  nix shell $toplevel#$attr -c $cmd ..." >&2
                else
                    echo "  nix-shell -p $attr --run '$cmd ...'" >&2
                fi
            done <<EOF
$attrs
EOF
            ;;
    esac

    return 127
}

command_not_found_handler () {
    command_not_found_handle "$@"
    return
}
