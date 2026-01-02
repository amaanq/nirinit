{
  description = "nirinit";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    let
      inherit (inputs.nixpkgs) lib;
      inherit (inputs) self;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      eachSystem = f: lib.genAttrs systems (system: f inputs.nixpkgs.legacyPackages.${system});
    in
    {
      nixosModules = {
        nirinit =
          { config, pkgs, ... }:
          let
            inherit (lib)
              mkEnableOption
              mkPackageOption
              mkIf
              getExe
              ;
            cfg = config.services.nirinit;
          in
          {
            options = {
              services.nirinit = {
                enable = mkEnableOption "Nirinit";
                package = mkPackageOption self.packages.${pkgs.system} "nirinit" { };
                settings = lib.mkOption {
                  type = lib.types.submodule {
                    freeformType = (pkgs.formats.toml { }).type;
                    options = {
                      skip = lib.mkOption {
                        type = lib.types.submodule {
                          options = {
                            apps = lib.mkOption {
                              type = lib.types.listOf lib.types.str;
                              default = [ ];
                              description = "List of app IDs to skip during session restore";
                            };
                          };
                        };
                        default = { };
                        description = "Applications to skip";
                      };
                      launch = lib.mkOption {
                        type = lib.types.attrsOf lib.types.str;
                        default = { };
                        description = "Map app_id to actual launch command";
                      };
                    };
                  };
                  default = { };
                  description = "Configuration for nirinit";
                };
              };
            };
            config = mkIf cfg.enable {
              systemd.user.services.nirinit = {
                enable = true;
                description = "Nirinit";
                wantedBy = [ "graphical-session.target" ];
                partOf = [ "graphical-session.target" ];
                wants = [ "graphical-session.target" ];
                after = [ "graphical-session.target" ];
                serviceConfig = {
                  Type = "simple";
                  Restart = "always";
                  ExecStart = "${getExe cfg.package}";
                  PrivateTmp = true;
                };
              };
            };
          };

        default = self.nixosModules.nirinit;
      };

      homeModules = {
        nirinit =
          { osConfig, pkgs, ... }:
          let
            inherit (lib) mkIf;
            cfg = osConfig.services.nirinit;
          in
          {
            config = mkIf cfg.enable {
              xdg.configFile."nirinit/config.toml" = {
                source = (pkgs.formats.toml { }).generate "nirinit-config.toml" cfg.settings;
              };
            };
          };

        default = self.homeModules.nirinit;
      };

      packages = eachSystem (
        pkgs:
        let
          packageName = "nirinit";
        in
        {
          nirinit = pkgs.rustPlatform.buildRustPackage {
            pname = packageName;
            src = ./.;
            version = "0.1.4";

            cargoLock.lockFile = ./Cargo.lock;

            meta.mainProgram = packageName;
          };

          default = self.packages.${pkgs.system}.nirinit;
        }
      );
      devShells = eachSystem (
        pkgs:
        let
          fenixPkgs = inputs.fenix.packages.${pkgs.system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.nixfmt-rfc-style
              (fenixPkgs.complete.withComponents [
                "cargo"
                "clippy"
                "rust-src"
                "rustc"
                "rustfmt"
                "rust-analyzer"
              ])
            ];
          };
        }
      );
    };
}
