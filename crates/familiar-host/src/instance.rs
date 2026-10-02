//! One host per machine: an exclusive OS lock on `~/.familiar/host.lock`, held for the host's lifetime. Two hosts would
//! mean two daemons (and two API servers) on one database, e.g. while the Tauri and native apps coexist. The OS drops
//! the lock when the process exits, so a crash never leaves it stale.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// Holds the lock until dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// `Ok(None)` when another process (or another handle in this one) already holds the lock.
    pub fn acquire(path: &Path) -> io::Result<Option<Self>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("familiar-host-test-{}-{nanos}", std::process::id())).join("host.lock")
    }

    #[test]
    fn second_acquire_fails_until_the_first_is_dropped() {
        let path = temp_lock();
        let first = InstanceLock::acquire(&path).unwrap().expect("first acquire");
        assert!(InstanceLock::acquire(&path).unwrap().is_none(), "lock must be exclusive");
        drop(first);
        let again = InstanceLock::acquire(&path).unwrap();
        assert!(again.is_some(), "lock must be released on drop");
        drop(again);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
