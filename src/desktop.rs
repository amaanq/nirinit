//! Launch commands from freedesktop desktop entries.
//!
//! - `app_id` is client-controlled, so it is only compared against the index
//!   and never joined into a path.
//! - `TryExec` is not checked, since niri spawns with its own `PATH`, not
//!   nirinit's.

use std::{
   collections::HashSet,
   env,
   ffi::OsString,
   fs,
   path::{
      Path,
      PathBuf,
   },
};

use crate::xdg_dir;

struct DesktopEntry {
   id:       String,
   exec:     String,
   wm_class: Option<String>,
}

impl DesktopEntry {
   fn parse(id: String, contents: &str) -> Option<Self> {
      let mut in_entry = false;
      let mut exec = None;
      let mut wm_class = None;
      let mut is_application = false;

      for line in contents.lines().map(str::trim) {
         if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
         }

         if !in_entry || line.starts_with('#') {
            continue;
         }

         let Some((key, value)) = line.split_once('=') else {
            continue;
         };

         let value = value.trim();
         match key.trim() {
            "Exec" => exec = Some(value),
            "StartupWMClass" => wm_class = Some(value.to_owned()),
            "Type" => is_application = value == "Application",
            "Hidden" if matches!(value, "true" | "1") => return None,
            _ => {},
         }
      }

      let exec = shell_command(exec?);
      (is_application && !exec.is_empty()).then_some(Self { id, exec, wm_class })
   }
}

/// The spec's `Exec` quoting is a subset of POSIX shell double quoting, so only
/// the string-level escapes need undoing before handing it to `sh -c`.
fn shell_command(exec: &str) -> String {
   let mut command = String::with_capacity(exec.len());
   let mut chars = exec.chars();

   while let Some(ch) = chars.next() {
      match (ch, chars.clone().next()) {
         ('\\', Some(escaped @ ('s' | 'n' | 't' | 'r' | '\\'))) => {
            chars.next();
            command.push(match escaped {
               's' => ' ',
               'n' => '\n',
               't' => '\t',
               'r' => '\r',
               _ => '\\',
            });
         },
         ('%', Some('%')) => {
            chars.next();
            command.push('%');
         },
         ('%', Some(code)) if code.is_ascii_alphabetic() => {
            chars.next();
         },
         _ => command.push(ch),
      }
   }

   command.trim_end().to_owned()
}

fn application_dirs() -> Vec<PathBuf> {
   let data_dirs = env::var_os("XDG_DATA_DIRS")
      .filter(|dirs| !dirs.is_empty())
      .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));

   xdg_dir("XDG_DATA_HOME", ".local/share")
      .into_iter()
      .chain(env::split_paths(&data_dirs))
      .filter(|dir| dir.is_absolute())
      .map(|dir| dir.join("applications"))
      .collect()
}

fn desktop_id(root: &Path, path: &Path) -> Option<String> {
   let relative = path.strip_prefix(root).ok()?.to_str()?;
   let stem = relative.strip_suffix(".desktop")?;
   Some(stem.replace('/', "-"))
}

pub struct DesktopEntries(Vec<DesktopEntry>);

impl DesktopEntries {
   pub fn load() -> Self {
      let mut seen_ids = HashSet::new();
      let mut visited_dirs = HashSet::new();
      let mut entries = Vec::new();

      for root in application_dirs() {
         let mut pending = vec![root.clone()];

         while let Some(dir) = pending.pop() {
            if !fs::canonicalize(&dir).is_ok_and(|real| visited_dirs.insert(real)) {
               continue;
            }

            let Ok(read_dir) = fs::read_dir(&dir) else {
               continue;
            };

            let mut paths = read_dir
               .flatten()
               .map(|file| file.path())
               .collect::<Vec<_>>();
            paths.sort();

            for path in paths {
               if path.is_dir() {
                  pending.push(path);
                  continue;
               }

               let Some(id) = desktop_id(&root, &path) else {
                  continue;
               };

               if !seen_ids.insert(id.clone()) {
                  continue;
               }

               if let Some(entry) = fs::read(&path)
                  .ok()
                  .and_then(|contents| DesktopEntry::parse(id, &String::from_utf8_lossy(&contents)))
               {
                  entries.push(entry);
               }
            }
         }
      }

      Self(entries)
   }

   pub fn launch_command(&self, app_id: &str) -> Option<&str> {
      self
         .0
         .iter()
         .find(|entry| entry.id == app_id)
         .or_else(|| {
            self.0.iter().find(|entry| {
               entry
                  .wm_class
                  .as_deref()
                  .is_some_and(|class| class.eq_ignore_ascii_case(app_id))
            })
         })
         .map(|entry| entry.exec.as_str())
   }
}
