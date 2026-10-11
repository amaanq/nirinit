use std::{
   collections::{
      HashMap,
      HashSet,
   },
   env,
   fs,
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   thread,
   time::Duration,
};

use log::{
   debug,
   error,
   info,
   warn,
};
use misstep::{
   OptionExt as _,
   Result,
   ResultExt as _,
   bail,
   report,
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
use pound::Parse;
use serde::{
   Deserialize,
   Serialize,
};

use crate::desktop::DesktopEntries;

mod desktop;
mod logger;

const APP_NAME: &str = env!("CARGO_PKG_NAME");

const WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Window data for session persistence (excludes title field).
#[derive(Serialize, Deserialize)]
struct SessionWindow<'niri> {
   id:               u64,
   /// The application id of the window, see <https://wayland-book.com/xdg-shell-basics/xdg-toplevel.html>.
   app_id:           Option<String>,
   /// The launch command to spawn this window (mapped from `app_id` via config,
   /// otherwise `app_id` if no mapping exists).
   launch_command:   Option<String>,
   /// Index of the workspace on the corresponding monitor.
   workspace_idx:    Option<u8>,
   /// Name of the workspace, in case of a named workspace.
   workspace_name:   Option<&'niri str>,
   /// Output the workspace is on.
   workspace_output: Option<&'niri str>,
   /// Whether the window is focused or not.
   is_focused:       bool,
   /// Window size (width, height) in logical pixels
   /// TODO: Remove [`Option`] in a month.
   #[serde(default)]
   window_size:      Option<(i32, i32)>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
   #[serde(default)]
   skip:    Skip,
   /// Map `app_id` to actual launch command (e.g.,
   /// "chrome-discord.com__app-Default" -> "discord-web-app").
   #[serde(default)]
   launch:  HashMap<String, String>,
   /// Prepended to every launch command (e.g., "uwsm app --").
   wrapper: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Skip {
   #[serde(default)]
   apps: Vec<String>,
}

/// A session manager for Niri, a scrollable-tiling Wayland compositor.
#[derive(Parse)]
struct Args {
   /// Save interval in seconds.
   #[pound(long, default = "300")]
   save_interval: u64,

   /// Path to config file.
   #[pound(short, long)]
   config: Option<PathBuf>,

   /// Enable debug output.
   #[pound(short, long)]
   debug: bool,
}

fn load_config(config_override: Option<&Path>) -> Result<Config> {
   let config_path = config_file(config_override)?;

   let config = fs::read_to_string(&config_path)
      .with_context(|| format!("The config file doesn't exist at {}", config_path.display()))?;

   Ok(toml::from_str(&config)?)
}

fn niri_request(request: Request) -> Result<Response> {
   let mut socket = Socket::connect().context("Failed to connect to Niri's IPC socket")?;
   let reply = socket
      .send(request)
      .context("Failed to send data to Niri's IPC socket")?;
   reply.map_err(|err| report!("Niri rejected the request: {err}"))
}

fn niri_windows() -> Result<Vec<Window>> {
   match niri_request(Request::Windows)? {
      Response::Windows(windows) => Ok(windows),
      _ => bail!("Unexpected response from Niri"),
   }
}

fn niri_workspaces() -> Result<Vec<Workspace>> {
   match niri_request(Request::Workspaces)? {
      Response::Workspaces(workspaces) => Ok(workspaces),
      _ => bail!("Unexpected response from Niri"),
   }
}

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
   env::var_os(var)
      .map(PathBuf::from)
      .filter(|dir| dir.is_absolute())
      .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
}

fn data_file() -> Result<PathBuf> {
   let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share")
      .context("Failed to locate the data directory ($XDG_DATA_HOME)")?
      .join(APP_NAME);
   fs::create_dir_all(&data_dir)
      .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;
   Ok(data_dir.join("session.json"))
}

/// Resolves the config file path by checking the `--config` flag, then the
/// `NIRINIT_CONFIG` environment variable, and finally falling back to
/// `$XDG_CONFIG_HOME/nirinit/config.toml`.
fn config_file(config_override: Option<&Path>) -> Result<PathBuf> {
   if let Some(path) = config_override {
      return Ok(path.to_path_buf());
   }

   if let Ok(config_path) = env::var("NIRINIT_CONFIG") {
      return Ok(PathBuf::from(config_path));
   }

   let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config")
      .context("Failed to locate the config directory ($XDG_CONFIG_HOME)")?
      .join(APP_NAME);
   fs::create_dir_all(&config_dir).with_context(|| {
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

/// Save the session.
fn save_session(file_path: &Path, config: &Config) -> Result<()> {
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

   let json_data =
      serde_json::to_string_pretty(&session_windows).context("Failed to serialize session data")?;

   let write = || -> io::Result<()> {
      let target = if fs::symlink_metadata(file_path).is_ok_and(|meta| meta.is_symlink()) {
         file_path.with_file_name(fs::read_link(file_path)?)
      } else {
         file_path.to_owned()
      };
      let permissions = match fs::metadata(&target) {
         Ok(metadata) => Some(metadata.permissions()),
         Err(err) if err.kind() == io::ErrorKind::NotFound => None,
         Err(err) => return Err(err),
      };

      let tmp_path = target.with_extension("json.tmp");
      let mut file = fs::File::create(&tmp_path)?;
      if let Some(permissions) = permissions {
         file.set_permissions(permissions)?;
      }
      file.write_all(json_data.as_bytes())?;
      file.sync_all()?;

      match fs::rename(&tmp_path, &target) {
         // A bind-mounted file (e.g. impermanence) can't be renamed over, only
         // rewritten in place.
         Err(err) if err.kind() == io::ErrorKind::ResourceBusy => {
            fs::remove_file(&tmp_path)?;
            fs::write(&target, &json_data)
         },
         result => result,
      }
   };
   write().with_context(|| format!("Failed to write to session file: {}", file_path.display()))?;
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
) -> Result<()> {
   let mut socket = Socket::connect().context("Failed to connect to Niri's IPC socket")?;

   // Earlier restores and windows already open must never be repositioned.
   let existing_ids = niri_windows()?
      .iter()
      .map(|window| window.id)
      .collect::<HashSet<u64>>();

   let reply = socket
      .send(Request::Action(Action::SpawnSh {
         command: launch_command.to_owned(),
      }))
      .context("Failed to send data to Niri's IPC socket")?;

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

      let Some(new_window) = windows.iter().find(|window| {
         window.app_id.as_deref() == Some(app_id) && !existing_ids.contains(&window.id)
      }) else {
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
         .context("Failed to send data to Niri's IPC socket")?
         .map_err(|err| report!("Niri rejected moving the window: {err}"))?;

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

fn restore_session(config: &Config, session_path: &Path) -> Result<()> {
   if !session_path.exists() {
      save_session(session_path, config)?;
      return Ok(());
   }

   info!("restoring previous session");

   let session_data = fs::read_to_string(session_path).context("Failed to read session file")?;
   if session_data.is_empty() {
      info!("session file at {} is empty", session_path.display());
      return Ok(());
   }

   let windows = match serde_json::from_str::<Vec<SessionWindow>>(&session_data) {
      Ok(windows) => windows,
      Err(err) => {
         let corrupt_path = session_path.with_extension("json.corrupt");
         warn!(
            "session file is corrupt, moving it to {} ({err})",
            corrupt_path.display()
         );
         fs::rename(session_path, &corrupt_path)
            .context("Failed to move the corrupt session file aside")?;
         return Ok(());
      },
   };

   // Sort windows by workspace index to ensure lower-indexed workspaces get
   // created first
   let mut sorted_windows = windows;
   sorted_windows.sort_by_key(|w| (w.workspace_output, w.workspace_idx));

   let desktop_entries = DesktopEntries::load();

   for window in sorted_windows {
      // Check if the launch command should be skipped
      if let Some(ref launch_command) = window.launch_command {
         if config.skip.apps.contains(launch_command) {
            info!("skipping command: {launch_command}");
            continue;
         }

         if let Some(ref app_id) = window.app_id {
            let discovered = if config.launch.contains_key(app_id) {
               None
            } else {
               desktop_entries.launch_command(app_id)
            };
            if let Some(discovered) = discovered {
               debug!("using desktop entry for {app_id}: {discovered}");
            }

            let base = discovered.unwrap_or(launch_command);
            let command = config
               .wrapper
               .as_ref()
               .map_or_else(|| base.to_owned(), |wrapper| format!("{wrapper} {base}"));
            spawn_and_move_window(
               &command,
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

#[expect(
   clippy::infinite_loop,
   reason = "saves until the service manager kills the process"
)]
fn main() -> Result<()> {
   logger::init();

   let args = Args::parse();

   if args.debug {
      logger::enable_debug();
   }

   let config = load_config(args.config.as_deref()).unwrap_or_else(|err| {
      warn!("failed to load config, using default values (reason: {err})");
      Config::default()
   });

   let session_path = data_file()?;
   info!("starting nirinit-manager");
   restore_session(&config, &session_path)?;

   info!("starting periodic save (interval: {}s)", args.save_interval);
   loop {
      thread::sleep(Duration::from_secs(args.save_interval));

      if let Err(report) = save_session(&session_path, &config) {
         error!("failed to save session: {report}");
      }
   }
}
