//! Single-instance guard: an `app.lock` file holding the owner's PID,
//! kept in the application's own folder, pure `std` + one `kernel32` call.
//!
//! A second copy would double the QPDF load and race the first one for the same
//! output names, so it shows "QpdfDesk is already running" and exits. A stale
//! lock (its PID no longer runs) is silently taken over.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::platform::pid_is_this_app;

/// Holds the lock for the lifetime of the app; dropping it removes the file.
pub struct InstanceLock {
    path: PathBuf,
}

/// Why the lock could not be acquired.
#[derive(Debug, PartialEq, Eq)]
pub enum LockError {
    /// Another live instance owns the lock.
    AlreadyRunning,
    /// The lock directory could not be created/written — treat as "no lock",
    /// the app still runs (a missing guard must not block the user).
    Unavailable,
}

impl InstanceLock {
    /// Try to become the single instance, using `dir` for the lock file.
    pub fn acquire(dir: &Path) -> Result<InstanceLock, LockError> {
        if std::fs::create_dir_all(dir).is_err() {
            return Err(LockError::Unavailable);
        }
        let path = dir.join("app.lock");

        match Self::try_create(&path) {
            Some(lock) => Ok(lock),
            None => {
                // Lock exists: live owner → refuse; stale/garbage → take over.
                let owner = crate::settings::read_text(&path, 64).and_then(|text| text.trim().parse::<u32>().ok());
                match owner {
                    Some(pid) if pid != std::process::id() && pid_is_this_app(pid) => {
                        Err(LockError::AlreadyRunning)
                    }
                    _ => {
                        let _ = std::fs::remove_file(&path);
                        Self::try_create(&path).ok_or(LockError::Unavailable)
                    }
                }
            }
        }
    }

    /// Create the lock file exclusively (`create_new`) with our PID inside.
    fn try_create(path: &Path) -> Option<InstanceLock> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .ok()?;
        let _ = write!(file, "{}", std::process::id());
        Some(InstanceLock { path: path.to_path_buf() })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qd_lock_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn acquires_and_releases() {
        let dir = temp_dir("basic");
        let lock = InstanceLock::acquire(&dir).expect("first acquire");
        assert!(dir.join("app.lock").exists());
        drop(lock);
        assert!(!dir.join("app.lock").exists(), "lock removed on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_owner_blocks_second_instance() {
        let dir = temp_dir("live");
        std::fs::create_dir_all(&dir).unwrap();
        // A live process that cannot be inspected counts as a running instance
        // (the safe side): PID 4 is the protected Windows System process.
        std::fs::write(dir.join("app.lock"), "4").unwrap();
        assert!(matches!(
            InstanceLock::acquire(&dir),
            Err(LockError::AlreadyRunning)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reused_pid_of_another_program_does_not_block() {
        // Regression: after a crash or power cut `app.lock` survives, and after
        // a restart its PID often belongs to some other program — which kept
        // the app refusing to start as "already running".
        let dir = temp_dir("reused");
        std::fs::create_dir_all(&dir).unwrap();
        let mut other = std::process::Command::new(crate::platform::system32("cmd.exe"))
            .args(["/C", "ping -n 6 127.0.0.1 >nul"])
            .spawn()
            .expect("start a stand-in process");
        std::fs::write(dir.join("app.lock"), other.id().to_string()).unwrap();
        let acquired = InstanceLock::acquire(&dir);
        let _ = other.kill();
        let _ = other.wait();
        assert!(acquired.is_ok(), "a live process of another program is a stale lock");
        drop(acquired);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_lock_is_taken_over() {
        let dir = temp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        // A PID that cannot exist keeps the lock stale.
        std::fs::write(dir.join("app.lock"), "4294967294").unwrap();
        let lock = InstanceLock::acquire(&dir).expect("stale lock taken over");
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_lock_is_taken_over() {
        let dir = temp_dir("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.lock"), "not a pid").unwrap();
        let lock = InstanceLock::acquire(&dir).expect("garbage lock taken over");
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
