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

    database = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Path to the nixdex database used by the command-not-found handler.
        Setting this to a small, `/bin/`-filtered database makes shell
        command suggestions almost instant. If unset, the handler falls back
        to `NIX_INDEX_DATABASE`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.sessionVariables = lib.optionalAttrs (cfg.database != null) {
      NIXDEX_DATABASE = cfg.database;
    };

    programs.bash.interactiveShellInit = lib.mkIf cfg.enableBashIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.zsh.interactiveShellInit = lib.mkIf cfg.enableZshIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.sh
    '';

    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      source ${cfg.package}/etc/profile.d/command-not-found.fish
    '';
  };
}
