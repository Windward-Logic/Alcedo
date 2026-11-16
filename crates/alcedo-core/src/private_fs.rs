//! Owner-only filesystem helpers: app data directories are created `0700`
//! and app-written files `0600`, so other local users cannot read mail,
//! drafts, attachments or the database.
//!
//! Only use these on directories Alcedo owns; [`create_private_dir_all`]
//! tightens the mode of the leaf directory it is given.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Create `path` (and missing parents) and make the leaf directory `0700`.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Restrict an existing file to `0600`. Missing files are not an error.
pub fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(path) {
            Ok(meta) if meta.permissions().mode() & 0o077 != 0 => {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            }
            Ok(_) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Create a new `0600` file; fails with `AlreadyExists` instead of
/// overwriting, so callers can pick another name without a race.
pub fn create_new_private_file(path: &Path) -> io::Result<File> {
    private_open_options().create_new(true).open(path)
}

/// Create or truncate a `0600` file.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let file = private_open_options()
        .create(true)
        .truncate(true)
        .open(path)?;
    restrict_file_permissions(path)?;
    Ok(file)
}

/// Write `bytes` to `path` atomically (temp file + fsync + rename), `0600`.
pub fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    let _ = fs::remove_file(&temp);
    let result = (|| {
        let mut file = create_new_private_file(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "alcedo-private-fs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn dirs_are_0700_and_existing_dirs_are_tightened() {
        let dir = scratch("dir");
        create_private_dir_all(&dir.join("a")).unwrap();
        assert_eq!(mode(&dir.join("a")), 0o700);
        fs::set_permissions(dir.join("a"), fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir_all(&dir.join("a")).unwrap();
        assert_eq!(mode(&dir.join("a")), 0o700);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn atomic_write_is_0600_and_replaces_content() {
        let dir = scratch("file");
        create_private_dir_all(&dir).unwrap();
        let path = dir.join("x.json");
        write_private_file_atomic(&path, b"one").unwrap();
        write_private_file_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(mode(&path), 0o600);
        assert!(!dir.join("x.json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_new_refuses_existing_and_restrict_tightens() {
        let dir = scratch("new");
        create_private_dir_all(&dir).unwrap();
        let path = dir.join("f");
        create_new_private_file(&path).unwrap();
        assert_eq!(
            create_new_private_file(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        restrict_file_permissions(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        restrict_file_permissions(&dir.join("missing")).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
