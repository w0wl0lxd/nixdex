{ package, ... }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.nixdex;
in
{
  options.programs.nixdex = {
    enable = lib.mkEnableOption "nixdex / nix-locate command-not-found integration";

    package = lib.mkOption {
      type = lib.types.package;
      default = package;
      description = "The nixdex package to use.";
    };

    enableBashIntegration = lib.mkEnableOption "Bash command-not-found integration" // {
      default = true;
    };

    enableZshIntegration = lib.mkEnableOption "Zsh command-not-found integration" // {
      default = true;
    };

    enableFishIntegration = lib.mkEnableOption "Fish command-not-found integration" // {
      default = true;
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    programs.bash.interactiveShellInit = lib.mkIf cfg.enableBashIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.zsh.interactiveShellInit = lib.mkIf cfg.enableZshIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      function __fish_command_not_found_handler --on-event fish_command_not_found
        bash -c 'source ${cfg.package}/etc/profile.d/command-not-found.sh; command_not_found_handle "$@"' -- $argv
      end
    '';
  };
}
