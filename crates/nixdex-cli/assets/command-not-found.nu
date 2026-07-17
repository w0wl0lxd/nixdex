{ |cmd_name|
  let comma_cmd = if (which , | length) > 0 {
    ","
  } else if (which comma | length) > 0 {
    "comma"
  } else {
    null
  }
  let install = { |pkgs|
    $pkgs | each {|pkg| $"  nix profile install nixpkgs#($pkg)" }
  }
  let run_once = { |pkgs|
    $pkgs | each {|pkg| $"  nix shell nixpkgs#($pkg) --command ($cmd_name) ..." }
  }
  let comma_lines = if $comma_cmd != null {
    ["" "Or run it once with:" $"  ($comma_cmd) ($cmd_name)"]
  } else {
    []
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
    ] | append $comma_lines
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
    ] | append $comma_lines
    $lines | str join "\n"
  }
  let pkgs = (@out@/bin/nix-locate --minimal --no-group --type x --type s --whole-name --at-root $"/bin/($cmd_name)" | lines)
  let len = ($pkgs | length)
  let ret = match $len {
    0 => null,
    1 => (do $single_pkg ($pkgs | get 0)),
    _ => (do $multiple_pkgs $pkgs),
  }
  return $ret
}
