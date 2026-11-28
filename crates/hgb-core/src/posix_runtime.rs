//! # POSIX Runtime & Filesystem Infrastructure
//!
//! Provides unified resolution for:
//! 1. Native POSIX runtime data directory: Defaults to `~/.hgb` on native ext4 SSD.
//! 2. UNIX domain socket paths: Defaults to `~/.hgb/hgb.sock`.
//! 3. Vector and relational databases: Defaults to `~/.hgb/vector_db` and `~/.hgb/*.db`.
//! 4. POSIX shared memory segments: Defaults to `/dev/shm` (tmpfs).
//! 5. Filesystem POSIX compliance detection (ext4/btrfs/xfs vs non-POSIX/VFAT).

use std::fs;
use std::path::{Path, PathBuf};

/// Resolves the default Hagibis runtime directory on native Linux ext4 SSD (~/.hgb).
pub fn default_hgb_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("HGB_HOME") {
        let p = PathBuf::from(override_dir);
        let _ = fs::create_dir_all(&p);
        return p;
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".hgb");
        let _ = fs::create_dir_all(&p);
        return p;
    }
    let p = PathBuf::from(".hgb");
    let _ = fs::create_dir_all(&p);
    p
}

/// Resolves the default Hagibis daemon UNIX domain socket path (~/.hgb/hgb.sock).
pub fn default_socket_path() -> PathBuf {
    if let Ok(socket_env) = std::env::var("HGB_SOCKET") {
        return PathBuf::from(socket_env);
    }
    default_hgb_dir().join("hgb.sock")
}

/// Resolves the default runtime vector database directory (~/.hgb/vector_db).
pub fn default_vector_db_dir() -> PathBuf {
    let p = default_hgb_dir().join("vector_db");
    let _ = fs::create_dir_all(&p);
    p
}

/// Resolves a runtime SQLite database path under ~/.hgb
pub fn resolve_runtime_db<P: AsRef<Path>>(db_filename: P) -> PathBuf {
    default_hgb_dir().join(db_filename)
}

/// Resolves a vector database path under ~/.hgb/vector_db
pub fn resolve_vector_db<P: AsRef<Path>>(db_filename: P) -> PathBuf {
    default_vector_db_dir().join(db_filename)
}

/// Resolves the default POSIX shared memory directory (/dev/shm with fallback to temp_dir).
pub fn default_shm_dir() -> PathBuf {
    if let Ok(custom_shm) = std::env::var("HGB_SHM_DIR") {
        let p = PathBuf::from(custom_shm);
        if p.is_dir() {
            return p;
        }
    }
    let shm = Path::new("/dev/shm");
    if shm.is_dir() && is_directory_writable(shm) {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    }
}

/// Checks whether a given path resides on a native POSIX filesystem (ext4, btrfs, xfs, etc.)
pub fn is_posix_filesystem<P: AsRef<Path>>(path: P) -> bool {
    let p = path.as_ref();
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_bytes = p.as_os_str().as_bytes();
        if let Ok(c_path) = CString::new(path_bytes) {
            unsafe {
                let mut stat: libc::statfs = std::mem::zeroed();
                if libc::statfs(c_path.as_ptr(), &mut stat) == 0 {
                    // Magic numbers:
                    // EXT4/EXT3/EXT2: 0xef53
                    // BTRFS: 0x9123683e
                    // XFS: 0x58465342
                    // TMPFS: 0x01021994
                    // MSDOS/VFAT: 0x4d44
                    let f_type = stat.f_type as u64;
                    const MSDOS_MAGIC: u64 = 0x4d44;
                    if f_type == MSDOS_MAGIC {
                        return false;
                    }
                    return true;
                }
            }
        }
    }
    true
}

/// Internal helper to check if a directory is writable
fn is_directory_writable(dir: &Path) -> bool {
    let probe_file = dir.join(format!(
        ".hgb_shm_probe_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if fs::write(&probe_file, b"").is_ok() {
        let _ = fs::remove_file(&probe_file);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_posix_runtime_paths() {
        let hgb_dir = default_hgb_dir();
        assert!(hgb_dir.exists());

        let sock_path = default_socket_path();
        assert!(sock_path.to_string_lossy().contains("hgb.sock"));

        let vec_dir = default_vector_db_dir();
        assert!(vec_dir.exists());

        let shm_dir = default_shm_dir();
        assert!(shm_dir.exists());
    }

    #[test]
    fn test_posix_filesystem_detection() {
        let home_dir = default_hgb_dir();
        assert!(is_posix_filesystem(&home_dir));
    }
}
