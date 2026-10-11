# Changelog

## [0.1.3] - 2025-10-02

### Features

There's a launch command mapping via the `[launch]` section in the config. You can use this to map problematic `app_id` values
to actual launch commands, in case the `app_id` for a given window is problematic. You can check the `app_id` using
`niri msg windows`.

### Example

```toml
[launch]
"chrome-discord.com__app-Default" = "discord-web-app"
```
