# nirinit

Session manager for [Niri](https://github.com/YaLTeR/niri) that automatically saves
and restores your window layout.

## Features

- Auto-saves session every 5 minutes (configurable)
- Restores windows to their workspaces on startup
- Preserves workspace names, indices, outputs, and window sizes
- Map `app_id` to custom launch commands
- Skip specific apps from being restored

## Installation

### NixOS + Home Manager

Add nirinit to your flake inputs:

```nix
{
  inputs.nirinit = {
    url = "github:amaanq/nirinit";
    inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

Import the NixOS module and configure:

```nix
# configuration.nix
{ inputs, ... }:
{
  imports = [ inputs.nirinit.nixosModules.nirinit ];

  services.nirinit = {
    enable = true;
    settings = {
      # Map app_id to launch command (useful for PWAs, flatpaks, etc.)
      launch = {
        "chromium-example.com__-Default" = "example-web-app";
      };
      # Apps to skip during restore
      skip.apps = [ "steam" ];
    };
  };
}
```

The config file will live in the nix store and its path is specified using the `--config` flag in the systemd service. 

### Manual

```bash
cargo install --path .

# Run as systemd user service or manually
nirinit --save-interval 300
```

## Configuration

The config file is located at `$XDG_CONFIG_HOME/nirinit/config.toml`
(typically `~/.config/nirinit/config.toml`).

```toml
[skip]
apps = ["steam"]

[launch]
# Map app_id to the actual command to spawn
"chromium-example.com__-Default" = "example-web-app"
```

## Session File

The session file is located at `$XDG_DATA_HOME/nirinit/session.json`
(typically `~/.local/share/nirinit/session.json`).

You shouldn't need to touch this. However, if session restore is acting up,
deleting it is a safe way to start fresh and might fix issues.

## License

MPL-2.0
