# command-not-found handler for nushell — adapted from nix-community/nix-index.
# @out@ is replaced at install time with the package output path.
{ |cmd_name|
  let install = { |pkgs|
    $pkgs | each {|pkg| $"  nix shell nixpkgs#($pkg)" }
  }
  let run_once = { |pkgs|
    $pkgs | each {|pkg| $"  nix shell nixpkgs#($pkg) --command '($cmd_name) ...'" }
  }
  let single_pkg = { |pkg|
    let lines = [
      $"The program '($cmd_name)' is currently not installed."
      ""
      "You can install it by typing:"
      (do $install [$pkg] | get 0)
      ""
      "Or run it once with:"
      (do $run_once [$pkg] | get 0)
    ]
    $lines | str join "\n"
  }
  let multiple_pkgs = { |pkgs|
    let lines = [
      $"The program '($cmd_name)' is currently not installed. It is provided by several packages."
      ""
      "You can install it by typing one of the following:"
      (do $install $pkgs | str join "\n")
      ""
      "Or run it once with:"
      (do $run_once $pkgs | str join "\n")
    ]
    $lines | str join "\n"
  }
  let database = ($env | get -i NIXDEX_DATABASE | default ($env | get -i NIX_INDEX_DATABASE | default ""))
  let base_args = [--minimal --no-group --type x --type s --whole-name --at-root $"/bin/($cmd_name)"]
  let args = if ($database | is-empty) { $base_args } else { ($base_args | append [--db $database]) }
  let pkgs = (@out@/bin/nix-locate ...$args | lines)
  let len = ($pkgs | length)
  let ret = match $len {
    0 => null,
    1 => (do $single_pkg ($pkgs | get 0)),
    _ => (do $multiple_pkgs $pkgs),
  }
  return $ret
}
