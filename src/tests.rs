use std::{
   env,
   os::unix::fs::PermissionsExt as _,
};

use super::*;

struct TestDirectory(PathBuf);

impl TestDirectory {
   #[expect(
      clippy::create_dir,
      reason = "Exclusive creation prevents sharing a directory with another test process"
   )]
   fn new() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      loop {
         let path = env::temp_dir().join(format!(
            "nirinit-test-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
         ));
         match fs::create_dir(&path) {
            Ok(()) => return Self(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {},
            Err(err) => panic!("cannot create test directory: {err}"),
         }
      }
   }
}

impl Drop for TestDirectory {
   fn drop(&mut self) {
      fs::remove_dir_all(&self.0).unwrap();
   }
}

#[test]
fn saves_create_and_replace_complete_sessions() {
   let directory = TestDirectory::new();
   let path = directory.0.join("session.json");
   write_session_file(&path, b"[1,2,3]").unwrap();
   assert_eq!(fs::read(&path).unwrap(), b"[1,2,3]");
   assert_eq!(
      fs::metadata(&path).unwrap().permissions().mode() & 0o777,
      0o600
   );
   fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
   write_session_file(&path, b"[]").unwrap();
   assert_eq!(fs::read(&path).unwrap(), b"[]");
   assert_eq!(
      fs::metadata(&path).unwrap().permissions().mode() & 0o777,
      0o600
   );
   assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn failed_partial_write_preserves_previous_session_and_cleans_staging_file() {
   let directory = TestDirectory::new();
   let path = directory.0.join("session.json");
   write_session_file(&path, b"[1,2,3]").unwrap();
   let error = stage_session_file(&path, |file| {
      assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
      file.write_all(b"[")?;
      Err(io::Error::other("simulated full disk"))
   })
   .unwrap_err();
   assert_eq!(error.to_string(), "simulated full disk");
   assert_eq!(fs::read(&path).unwrap(), b"[1,2,3]");
   assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn failed_rename_cleans_staging_file() {
   let directory = TestDirectory::new();
   let path = directory.0.join("session.json");
   fs::create_dir_all(&path).unwrap();
   fs::write(path.join("marker"), b"preserved").unwrap();
   assert!(write_session_file(&path, b"[]").is_err());
   assert_eq!(fs::read(path.join("marker")).unwrap(), b"preserved");
   assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

