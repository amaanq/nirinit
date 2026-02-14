use std::{
   collections::HashMap,
   fs,
   io,
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Arc,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
   time::{
      Duration,
      Instant,
   },
};

use anstyle::{
   AnsiColor,
   Color,
   Style,
};
use clap::{
   Parser,
   builder,
   crate_authors,
};
use color_eyre::eyre;
use eyre::{
   Context as _,
   OptionExt as _,
};
use log::{
   debug,
   error,
   info,
   warn,
};
use niri_ipc::{
   Action,
   Reply,
   Request,
   Response,
   SizeChange,
   Window,
   Workspace,
   WorkspaceReferenceArg,
   socket::Socket,
};
use serde::{
   Deserialize,
   Serialize,
};
use signal_hook::{
   consts::TERM_SIGNALS,
   flag,
};
use thiserror::Error;

mod logger;

const APP_NAME: &str = env!("CARGO_PKG_NAME");

const WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Error)]
pub enum NiriError {
   #[error("Failed to communicate with Niri via IPC: {0}")]
   Reply(String),
   #[error("Failed to connect to Niri's IPC socket: {0}")]
   Connect(io::Error),
   #[error("Failed to send data to Niri's IPC socket: {0}")]
   Send(io::Error),
}

type NiriResult<T> = Result<T, NiriError>;

/// Window data for session persistence (excludes title field)
#[derive(Serialize, Deserialize)]
struct SessionWindow<'niri> {
   id:               u64,
   /// The application id of the window, see <https://wayland-book.com/xdg-shell-basics/xdg-toplevel.html>
   app_id:           Option<String>,
   /// The launch command to spawn this window (mapped from `app_id` via config,
   /// otherwise `app_id` if no mapping exists)
   launch_command:   Option<String>,
   /// Index of the workspace on the corresponding monitor
   workspace_idx:    Option<u8>,
   /// Name of the workspace, in case of a named workspace
   workspace_name:   Option<&'niri str>,
   /// Output the workspace is on
   workspace_output: Option<&'niri str>,
   /// Whether the window is focused or not
   is_focused:       bool,
   /// Window size (width, height) in logical pixels
   /// TODO: Remove [`Option`] in a month
   #[serde(default)]
   window_size:      Option<(i32, i32)>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
   #[serde(default)]
   skip:   Skip,
   /// Map `app_id` to actual launch command (e.g.,
   /// "thorium-discord.com__app-Default" -> "discord-web-app")
   #[serde(default)]
   launch: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Skip {
   #[serde(default)]
   apps: Vec<String>,
}

#[derive(Parser)]
#[command(
    author=crate_authors!("\n"),
    styles=get_styles(),
    version,
    about,
    long_about = None,
    help_template = concat!(
        "\n",
        "{before-help}{name} {version}\n",
        "{author-with-newline}\n",
        "{about-with-newline}\n",
        "{usage-heading} {usage}\n",
        "\n",
        "{all-args}{after-help}\n",
        "\n"
    )
)]
struct Args {
   /// Save interval in seconds
   #[arg(long, default_value = "300")]
   save_interval: u64,

   /// Path to config file
   #[arg(long, short)]
   config: Option<PathBuf>,

   /// Enable debug output
   #[arg(long, short)]
   debug: bool,
}

fn load_config(config_override: Option<&Path>) -> eyre::Result<Config> {
   let config_path = config_file(config_override)?;

   let config = fs::read_to_string(&config_path)
      .wrap_err_with(|| format!("The config file doesn't exist at {}", config_path.display()))?;

   Ok(toml::from_str(&config)?)
}

fn niri_windows() -> NiriResult<Vec<Window>> {
   let mut socket = Socket::connect().map_err(NiriError::Connect)?;
   match socket
      .send(Request::Windows)
      .map_err(NiriError::Send)?
      .map_err(NiriError::Reply)?
   {
      Response::Windows(windows) => Ok(windows),
      other => {
         Err(NiriError::Reply(format!(
            "Unexpected response from Niri: {other:?}"
         )))
      },
   }
}

fn niri_workspaces() -> NiriResult<Vec<Workspace>> {
   let mut socket = Socket::connect().map_err(NiriError::Connect)?;
   match socket
      .send(Request::Workspaces)
      .map_err(NiriError::Send)?
      .map_err(NiriError::Reply)?
   {
      Response::Workspaces(workspaces) => Ok(workspaces),
      other => {
         Err(NiriError::Reply(format!(
            "Unexpected response from Niri: {other:?}"
         )))
      },
   }
}

fn data_file() -> eyre::Result<PathBuf> {
   let data_dir = dirs::data_dir()
      .ok_or_eyre("Failed to locate the data directory ($XDG_DATA_HOME)")?
      .join(APP_NAME);
   fs::create_dir_all(&data_dir)
      .wrap_err_with(|| format!("Failed to create data directory: {}", data_dir.display()))?;
   Ok(data_dir.join("session.json"))
}

/// Resolves the config file path by checking the `--config` flag, then the
/// `NIRINIT_CONFIG` environment variable, and finally falling back to
/// `$XDG_CONFIG_HOME/nirinit/config.toml`.
fn config_file(config_override: Option<&Path>) -> eyre::Result<PathBuf> {
   if let Some(path) = config_override {
      return Ok(path.to_path_buf());
   }

   if let Ok(config_path) = std::env::var("NIRINIT_CONFIG") {
      return Ok(PathBuf::from(config_path));
   }

   let config_dir = dirs::config_dir()
      .ok_or_eyre("Failed to locate the config directory ($XDG_CONFIG_HOME)")?
      .join(APP_NAME);
   fs::create_dir_all(&config_dir).wrap_err_with(|| {
      format!(
         "Failed to create config directory: {}",
         config_dir.display()
      )
   })?;
   Ok(config_dir.join("config.toml"))
}

fn find_workspace_for_window<'niri>(
   window: &Window,
   workspaces: &'niri [Workspace],
) -> Option<&'niri Workspace> {
   workspaces
      .iter()
      .find(|w| window.workspace_id == Some(w.id))
}

/// Save the session
fn save_session(file_path: &Path, config: &Config) -> eyre::Result<()> {
   let windows = niri_windows()?;
   let workspaces = niri_workspaces()?;

   let session_windows = windows
      .into_iter()
      .map(|window| {
         let workspace = find_workspace_for_window(&window, &workspaces);

         // Map app_id to launch command if it exists in the config
         let launch_command = window.app_id.as_ref().and_then(|app_id| {
            config
               .launch
               .get(app_id)
               .cloned()
               .or_else(|| Some(app_id.clone()))
         });

         SessionWindow {
            id: window.id,
            app_id: window.app_id,
            launch_command,
            workspace_idx: workspace.map(|w| w.idx),
            workspace_name: workspace.and_then(|w| w.name.as_deref()),
            workspace_output: workspace.and_then(|w| w.output.as_deref()),
            is_focused: window.is_focused,
            window_size: Some(window.layout.window_size),
         }
      })
      .collect::<Vec<_>>();

   let json_data = serde_json::to_string_pretty(&session_windows)
      .wrap_err("Failed to serialize session data")?;

   fs::write(file_path, json_data)
      .wrap_err_with(|| format!("Failed to write to session file: {}", file_path.display()))?;
   debug!("saved session to {}", file_path.display());
   Ok(())
}

fn spawn_and_move_window<'niri>(
   launch_command: &str,
   app_id: &str,
   workspace_idx: Option<u8>,
   workspace_name: Option<&'niri str>,
   workspace_output: Option<&'niri str>,
   window_size: Option<(i32, i32)>,
) -> eyre::Result<()> {
   let command = vec![launch_command.to_owned()];

   let mut socket = Socket::connect().wrap_err("Failed to connect to Niri IPC socket")?;

   let reply = socket
      .send(Request::Action(Action::Spawn { command }))
      .map_err(NiriError::Send)?;

   let Reply::Ok(Response::Handled) = reply else {
      error!("failed to spawn command `{launch_command}`");
      return Ok(());
   };

   // Prioritize named workspaces
   let workspace_reference = if let Some(name) = workspace_name {
      WorkspaceReferenceArg::Name(name.to_owned())
   } else if let Some(idx) = workspace_idx {
      WorkspaceReferenceArg::Index(idx)
   } else {
      return Ok(());
   };

   for _ in 0..20 {
      thread::sleep(WINDOW_POLL_INTERVAL);

      let windows = niri_windows()?;

      let Some(new_window) = windows.iter().find(|w| w.app_id.as_deref() == Some(app_id)) else {
         continue;
      };

      if let Some(output) = workspace_output
         && let Err(err) = socket.send(Request::Action(Action::MoveWindowToMonitor {
            id:     Some(new_window.id),
            output: output.to_owned(),
         }))
      {
         warn!(
            "failed to move window {}: {err}",
            new_window
               .app_id
               .as_ref()
               .map_or_else(String::new, |app_id| format!("(app_id: {app_id})")),
         );
      }

      // Move window to the correct workspace
      // This will automatically create the workspace if it doesn't exist
      socket
         .send(Request::Action(Action::MoveWindowToWorkspace {
            window_id: Some(new_window.id),
            reference: workspace_reference,
            focus:     false,
         }))
         .map_err(NiriError::Send)?
         .map_err(NiriError::Reply)?;

      if let Some((width, height)) = window_size {
         if let Err(err) = socket.send(Request::Action(Action::SetWindowWidth {
            id:     Some(new_window.id),
            change: SizeChange::SetFixed(width),
         })) {
            warn!(
               "failed to restore window width for {}: {err}",
               new_window.app_id.as_deref().unwrap_or("unknown")
            );
         }

         if let Err(err) = socket.send(Request::Action(Action::SetWindowHeight {
            id:     Some(new_window.id),
            change: SizeChange::SetFixed(height),
         })) {
            warn!(
               "failed to restore window height for {}: {err}",
               new_window.app_id.as_deref().unwrap_or("unknown")
            );
         }
      }

      return Ok(());
   }

   warn!("window for `{launch_command}` did not appear within 5s");

   Ok(())
}

fn restore_session(config: &Config, session_path: &Path) -> eyre::Result<()> {
   if !session_path.exists() {
      save_session(session_path, config)?;
      return Ok(());
   }

   info!("restoring previous session");

   let session_data = fs::read_to_string(session_path).wrap_err("Failed to read session file")?;
   if session_data.is_empty() {
      info!("session file at {} is empty", session_path.display());
      return Ok(());
   }

   let windows = serde_json::from_str::<Vec<SessionWindow>>(&session_data)
      .wrap_err("Failed to load session data")?;

   // Sort windows by workspace index to ensure lower-indexed workspaces get
   // created first
   let mut sorted_windows = windows;
   sorted_windows.sort_by_key(|w| (w.workspace_output, w.workspace_idx));

   for window in sorted_windows {
      // Check if the launch command should be skipped
      if let Some(ref launch_command) = window.launch_command {
         if config.skip.apps.contains(launch_command) {
            info!("skipping command: {launch_command}");
            continue;
         }

         if let Some(ref app_id) = window.app_id {
            spawn_and_move_window(
               launch_command,
               app_id,
               window.workspace_idx,
               window.workspace_name,
               window.workspace_output,
               window.window_size,
            )?;
         }
      }
   }

   info!("restored session");
   Ok(())
}

#[must_use]
const fn get_styles() -> builder::Styles {
   builder::Styles::styled()
      .usage(
         Style::new()
            .bold()
            .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
      )
      .header(
         Style::new()
            .bold()
            .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
      )
      .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
      .invalid(
         Style::new()
            .bold()
            .fg_color(Some(Color::Ansi(AnsiColor::Red))),
      )
      .error(
         Style::new()
            .bold()
            .fg_color(Some(Color::Ansi(AnsiColor::Red))),
      )
      .valid(
         Style::new()
            .bold()
            .fg_color(Some(Color::Ansi(AnsiColor::Green))),
      )
      .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::White))))
}

fn main() -> eyre::Result<()> {
   logger::init();
   color_eyre::install()?;

   let args = Args::parse();

   if args.debug {
      logger::enable_debug();
   }

   let config = load_config(args.config.as_deref()).unwrap_or_else(|err| {
      warn!("failed to load config, using default values (reason: {err})");
      Config::default()
   });

   let session_path = data_file()?;
   let term = Arc::new(AtomicBool::new(false));

   for sig in TERM_SIGNALS {
      flag::register(*sig, Arc::clone(&term))?;
   }

   info!("starting nirinit-manager");
   restore_session(&config, &session_path)?;

   info!("starting periodic save (interval: {}s)", args.save_interval);
   let mut last_save = Instant::now();

   while !term.load(Ordering::Relaxed) {
      thread::sleep(Duration::from_millis(100));

      if last_save.elapsed() >= Duration::from_secs(args.save_interval) {
         if let Err(report) = save_session(&session_path, &config) {
            error!("failed to save session: {report}");
         }
         last_save = Instant::now();
      }
   }

   info!("shutting down...");
   if let Err(report) = save_session(&session_path, &config) {
      error!("error saving final session: {report}");
   }
   info!("shutdown complete");
   Ok(())
}
