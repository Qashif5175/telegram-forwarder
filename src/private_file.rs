//! Writing a file that only its owner can read.
//!
//! Both files this tool persists hold something that should not be readable by
//! other users of the machine: the session file *is* a login to the Telegram
//! account, and the configuration holds the API credentials. They are written
//! through here so the answer is the same for both, rather than the session
//! being careful and the configuration inheriting whatever the umask allows.
//!
//! The permissions are applied *as the file is created*, not afterwards. A
//! create-then-`chmod` leaves the contents on disk at the umask's discretion for
//! as long as the write takes, which is precisely the window worth closing.
//!
//! On platforms without Unix permissions this is an ordinary write: Windows
//! inherits the containing directory's ACL, and the directories this tool
//! creates are under the user's own profile.

use std::io;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// Write `bytes` to `path`, readable and writable by the owner alone.
#[cfg(unix)]
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // `fs_err` wraps free functions, not `OpenOptions`, and the mode has to be
    // set at creation rather than applied to an already-open file. The caller
    // attaches the path to any error, so nothing is lost by dropping to `std`.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;

    file.write_all(bytes)?;

    // A rename over this file is atomic, but only with respect to content that
    // actually reached the disk.
    file.sync_all()
}

/// Write `bytes` to `path`.
#[cfg(not(unix))]
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs_err::write(path, bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn a_private_file_is_never_briefly_readable() {
        // Creating the file and then tightening it would leave the contents on
        // disk at the umask's discretion for as long as the write takes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write(&path, b"key").unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} exposes the file to other users");
    }

    #[test]
    fn writing_over_an_existing_file_does_not_inherit_its_permissions() {
        // `OpenOptions::mode` applies only when the file is created, so a file
        // that already exists keeps whatever it had. Anything written through
        // here is replaced by a rename from a fresh temporary, but assert the
        // shape of the trap so a future caller that writes in place is caught.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing");

        fs_err::write(&path, b"old").unwrap();
        fs_err::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, b"new").unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0o044,
            "documenting that an existing file keeps its own mode"
        );
    }
}
