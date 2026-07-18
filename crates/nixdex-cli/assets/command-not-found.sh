#!/usr/bin/env bash

# Shell integration for nixdex / nix-locate.
# Source this file from your shell profile to get command-not-found suggestions.
# @out@ is replaced at install time with the package output path.

comma_cmd() {
  if command -v , >/dev/null 2>&1; then
    printf '%s' ','
  elif command -v comma >/dev/null 2>&1; then
    printf '%s' 'comma'
  fi
}

# Print any extra nix-locate arguments derived from NIXDEX_DATABASE /
# NIX_INDEX_DATABASE, one per line, so the caller can collect them with mapfile.
nixdex_db_args() {
  if [ -n "${NIXDEX_DATABASE-}" ]; then
    printf '%s\n' '--db' "$NIXDEX_DATABASE"
  elif [ -n "${NIX_INDEX_DATABASE-}" ]; then
    printf '%s\n' '--db' "$NIX_INDEX_DATABASE"
  fi
}

command_not_found_handle() {
  # Do not run when inside Midnight Commander or within a pipe.
  if [ -n "${MC_SID-}" ] || ! [ -t 1 ]; then
    echo "$1: command not found" >&2
    return 127
  fi

  local toplevel=nixpkgs
  local cmd=$1
  local comma
  comma=$(comma_cmd)

  local db_args=()
  mapfile -t db_args < <(nixdex_db_args)

  local attrs
  attrs=$(@out@/bin/nix-locate "${db_args[@]}" --minimal --no-group --type x --type s --whole-name --at-root "/bin/$cmd")
  local len
  len=$(printf '%s\n' "$attrs" | grep -c .)

  # Run the selected package with the original command line.
  run_selected() {
    local selected=$1
    shift
    if [ -e "$HOME/.nix-profile/manifest.json" ]; then
      nix shell "$toplevel#$selected" -c "$@"
    else
      local escaped
      printf -v escaped '%q ' "$@"
      nix-shell -p "$selected" --run "${escaped% }"
    fi
  }

  # Prompt the user to choose one of the candidate packages.
  prompt_and_run() {
    local selected answer i attr

    if [ "$len" -eq 1 ]; then
      selected=$(printf '%s\n' "$attrs" | head -n 1)
      cat >&2 <<EOF
The program '$cmd' is currently not installed. It is provided by
the package '$toplevel.$selected'.
EOF
      printf 'Should it be used? ([Y]es | [n]one): ' >&2
      read -r answer
      case "$answer" in
      [yY] | "")
        run_selected "$selected" "$@"
        return $?
        ;;
      *)
        echo "$cmd: command not found" >&2
        return 127
        ;;
      esac
    fi

    cat >&2 <<EOF
The program '$cmd' is currently not installed. It is provided by
several packages:
EOF
    i=1
    while IFS= read -r attr; do
      echo "  $i. $toplevel.$attr" >&2
      i=$((i + 1))
    done <<EOF
$attrs
EOF
    printf 'Should the first output be used? ([Y]es | [number] | [n]one): ' >&2
    read -r answer
    case "$answer" in
    [yY] | "")
      selected=$(printf '%s\n' "$attrs" | head -n 1)
      ;;
    [nN])
      echo "$cmd: command not found" >&2
      return 127
      ;;
    *[!0-9]*)
      echo "$cmd: command not found" >&2
      return 127
      ;;
    *)
      selected=$(printf '%s\n' "$attrs" | sed -n "${answer}p")
      ;;
    esac

    if [ -n "$selected" ]; then
      run_selected "$selected" "$@"
      return $?
    fi
    echo "$cmd: command not found" >&2
    return 127
  }

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
    elif [ -n "${NIX_AUTO_RUN_INTERACTIVE-}" ]; then
      prompt_and_run "$@"
      return $?
    else
      if [ -e "$HOME/.nix-profile/manifest.json" ]; then
        if [ -n "$comma" ]; then
          cat >&2 <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix profile install $toplevel#$attrs

Or run it once with:
  nix shell $toplevel#$attrs -c $cmd ...

Or run it once with:
  $comma $cmd
EOF
        else
          cat >&2 <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix profile install $toplevel#$attrs

Or run it once with:
  nix shell $toplevel#$attrs -c $cmd ...
EOF
        fi
      else
        if [ -n "$comma" ]; then
          cat >&2 <<EOF
The program '$cmd' is currently not installed. You can install it
by typing:
  nix-env -iA $toplevel.$attrs

Or run it once with:
  nix-shell -p $attrs --run '$cmd ...'

Or run it once with:
  $comma $cmd
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
    fi
    ;;
  *)
    if [ -n "${NIX_AUTO_RUN_INTERACTIVE-}" ]; then
      prompt_and_run "$@"
      return $?
    else
      cat >&2 <<EOF
The program '$cmd' is currently not installed. It is provided by
several packages. You can install it by typing one of the following:
EOF
      while IFS= read -r attr; do
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
      while IFS= read -r attr; do
        if [ -e "$HOME/.nix-profile/manifest.json" ]; then
          echo "  nix shell $toplevel#$attr -c $cmd ..." >&2
        else
          echo "  nix-shell -p $attr --run '$cmd ...'" >&2
        fi
      done <<EOF
$attrs
EOF

      if [ -n "$comma" ]; then
        cat >&2 <<EOF

Or run it once with:
  $comma $cmd
EOF
      fi
    fi
    ;;
  esac

  return 127
}

command_not_found_handler() {
  command_not_found_handle "$@"
  return $?
}
