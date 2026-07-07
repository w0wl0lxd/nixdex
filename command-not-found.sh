#!/bin/sh
# command-not-found handler for bash/zsh — adapted from nix-community/nix-index.
# @out@ is replaced at install time with the package output path.

command_not_found_handle () {
    # Do not run when inside Midnight Commander or within a pipe.
    if [ -n "${MC_SID-}" ] || ! [ -t 1 ]; then
        >&2 echo "$1: command not found"
        return 127
    fi

    toplevel=nixpkgs
    cmd=$1
    attrs=$(@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root "/bin/$cmd")
    len=$(echo -n "$attrs" | grep -c "^")

    case $len in
        0)
            >&2 echo "$cmd: command not found"
            ;;
        1)
            if [ -n "${NIX_AUTO_INSTALL-}" ]; then
                >&2 cat <<EOF
The program '$cmd' is currently not installed. It is provided by
the package '$toplevel.$attrs', which I will now install for you.
EOF
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    nix profile install "$toplevel#$attrs"
                else
                    nix-env -iA "$toplevel.$attrs"
                fi
                if [ "$?" -eq 0 ]; then
                    "$@"
                    return $?
                else
                    >&2 cat <<EOF
Failed to install $toplevel.$attrs.
$cmd: command not found
EOF
                fi
            elif [ -n "${NIX_AUTO_RUN-}" ]; then
                nix-build --no-out-link -A "$attrs" "<$toplevel>"
                if [ "$?" -eq 0 ]; then
                    nix-shell -p "$attrs" --run "$*"
                    return $?
                else
                    >&2 cat <<EOF
Failed to install $toplevel.$attrs.
$cmd: command not found
EOF
                fi
            else
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    >&2 cat <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix profile install $toplevel#$attrs

Or run it once with:
  nix shell $toplevel#$attrs -c $cmd ...
EOF
                else
                    >&2 cat <<EOF
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
            >&2 cat <<EOF
The program '$cmd' is currently not installed. It is provided by
several packages. You can install it by typing one of the following:
EOF

            printf '%s\n' "$attrs" | while read -r attr; do
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    >&2 echo "  nix profile install $toplevel#$attr"
                else
                    >&2 echo "  nix-env -iA $toplevel.$attr"
                fi
            done

            >&2 cat <<EOF

Or run it once with:
EOF

            printf '%s\n' "$attrs" | while read -r attr; do
                if [ -e "$HOME/.nix-profile/manifest.json" ]; then
                    >&2 echo "  nix shell $toplevel#$attr -c $cmd ..."
                else
                    >&2 echo "  nix-shell -p $attr --run '$cmd ...'"
                fi
            done
            ;;
    esac

    return 127
}

# zsh uses a different hook name with the same behaviour.
command_not_found_handler () {
    command_not_found_handle "$@"
    return $?
}
