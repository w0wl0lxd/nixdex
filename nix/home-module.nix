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

    enableBashIntegration = lib.hm.shell.mkBashIntegrationOption { inherit config; };
    enableZshIntegration = lib.hm.shell.mkZshIntegrationOption { inherit config; };
    enableFishIntegration = lib.hm.shell.mkFishIntegrationOption { inherit config; };
    enableNushellIntegration = lib.hm.shell.mkNushellIntegrationOption { inherit config; };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.zsh.initContent = lib.mkIf cfg.enableZshIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      function __fish_command_not_found_handler --on-event fish_command_not_found
        bash -c 'source ${cfg.package}/etc/profile.d/command-not-found.sh; command_not_found_handle "$@"' -- $argv
      end
    '';

    programs.nushell.settings.hooks.command_not_found = lib.mkIf cfg.enableNushellIntegration (
      lib.hm.nushell.mkNushellInline "source ${cfg.package}/etc/profile.d/command-not-found.nu"
    );
  };
}
