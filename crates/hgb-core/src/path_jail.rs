use crate::error::{HgbError, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// PathJail provides strict workspace boundary enforcement for sandboxed operations.
/// Strictly rejects:
/// 1. Path traversal (`..`) in any path component
/// 2. Absolute / root dir paths (`/` or `\`)
/// 3. Prefix paths (Windows drive letters or UNC shares)
/// 4. Symlinks pointing outside the designated jail root
#[derive(Debug, Clone)]
pub struct PathJail {
    root: PathBuf,
}

impl PathJail {
    /// Create a new PathJail anchored at `root`
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let r = root.as_ref();
        let canonical_root = if r.exists() {
            r.canonicalize().map_err(|e| {
                HgbError::Security(format!(
                    "PathJail failed to canonicalize root '{}': {e}",
                    r.display()
                ))
            })?
        } else {
            // If the root directory doesn't exist yet, normalize without resolving non-existent paths
            r.to_path_buf()
        };

        Ok(Self {
            root: canonical_root,
        })
    }

    /// Access the canonical jail root path
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Strictly validate that `rel_path` is safe and contained entirely within the jail root.
    /// Returns the resolved target `PathBuf` inside the jail if valid.
    pub fn check_boundary(&self, rel_path: impl AsRef<Path>) -> Result<PathBuf> {
        let p = rel_path.as_ref();
        let s = p.to_string_lossy();

        // 1. Strictly reject empty paths
        if s.trim().is_empty() {
            return Err(HgbError::Security(
                "Path boundary violation: empty path rejected".to_string(),
            ));
        }

        // 2. Strictly reject paths starting with root separators
        if s.starts_with('/') || s.starts_with('\\') {
            return Err(HgbError::Security(format!(
                "Path boundary violation: absolute root path '{}' rejected",
                p.display()
            )));
        }

        // 3. Inspect individual components for traversal, root dir, or prefix
        for component in p.components() {
            match component {
                Component::ParentDir => {
                    return Err(HgbError::Security(format!(
                        "Path boundary violation: traversal ('..') detected in '{}'",
                        p.display()
                    )));
                }
                Component::RootDir => {
                    return Err(HgbError::Security(format!(
                        "Path boundary violation: root directory component detected in '{}'",
                        p.display()
                    )));
                }
                Component::Prefix(_) => {
                    return Err(HgbError::Security(format!(
                        "Path boundary violation: path prefix detected in '{}'",
                        p.display()
                    )));
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }

        // 4. Construct candidate target path
        let target = self.root.join(p);

        // 5. Verify symlinks do not escape the jail root
        self.verify_symlink_containment(&target)?;

        Ok(target)
    }

    /// Alias for check_boundary
    pub fn resolve_and_validate(&self, rel_path: impl AsRef<Path>) -> Result<PathBuf> {
        self.check_boundary(rel_path)
    }

    /// Strictly resolve and validate `rel_path` inside the jail root.
    /// Rejects absolute paths, traversals, and symlink escapes with `HgbError::SandboxViolation`.
    pub fn resolve(&self, rel_path: impl AsRef<Path>) -> Result<PathBuf> {
        let p = rel_path.as_ref();
        if p.is_absolute() {
            return Err(HgbError::SandboxViolation(format!(
                "Sandbox path containment violation: absolute path '{}' rejected",
                p.display()
            )));
        }
        self.check_boundary(p).map_err(|e| match e {
            HgbError::Security(msg) => HgbError::SandboxViolation(msg),
            other => HgbError::SandboxViolation(other.to_string()),
        })
    }

    /// Check if a path passes boundary validation without returning error
    pub fn is_safe(&self, rel_path: impl AsRef<Path>) -> bool {
        self.check_boundary(rel_path).is_ok()
    }

    /// Verify that no symlink in the path hierarchy escapes outside `self.root`
    fn verify_symlink_containment(&self, target: &Path) -> Result<()> {
        let canonical_root = if self.root.exists() {
            self.root.canonicalize().unwrap_or_else(|_| self.root.clone())
        } else {
            self.root.clone()
        };

        // If the target already exists, canonicalize directly
        if target.exists() {
            let canon = target.canonicalize().map_err(|e| {
                HgbError::Security(format!(
                    "Path boundary violation: failed to canonicalize '{}': {e}",
                    target.display()
                ))
            })?;
            if !canon.starts_with(&canonical_root) {
                return Err(HgbError::Security(format!(
                    "Path boundary violation: path '{}' resolves to '{}' outside jail root '{}'",
                    target.display(),
                    canon.display(),
                    canonical_root.display()
                )));
            }
            return Ok(());
        }

        // Target does not exist yet. Check if target itself is a broken symlink
        if target.is_symlink() {
            if let Ok(link_dest) = fs::read_link(target) {
                let resolved = if link_dest.is_absolute() {
                    link_dest
                } else {
                    target.parent().unwrap_or(&canonical_root).join(link_dest)
                };
                if let Ok(canon) = resolved.canonicalize() {
                    if !canon.starts_with(&canonical_root) {
                        return Err(HgbError::Security(format!(
                            "Path boundary violation: broken symlink '{}' points outside jail root: '{}'",
                            target.display(),
                            canon.display()
                        )));
                    }
                } else if !resolved.starts_with(&canonical_root) {
                    return Err(HgbError::Security(format!(
                        "Path boundary violation: broken symlink '{}' points outside jail root: '{}'",
                        target.display(),
                        resolved.display()
                    )));
                }
            }
        }

        // Walk up to find existing parent directories and verify no intermediate symlinks escape
        let mut curr = target.to_path_buf();
        while let Some(parent) = curr.parent() {
            if parent.exists() {
                let canon_parent = parent.canonicalize().map_err(|e| {
                    HgbError::Security(format!(
                        "Path boundary violation: failed to canonicalize parent '{}': {e}",
                        parent.display()
                    ))
                })?;
                if !canon_parent.starts_with(&canonical_root) {
                    return Err(HgbError::Security(format!(
                        "Path boundary violation: parent '{}' resolves to '{}' outside jail root '{}'",
                        parent.display(),
                        canon_parent.display(),
                        canonical_root.display()
                    )));
                }
                break;
            }
            curr = parent.to_path_buf();
        }

        // Also check each component from canonical_root down to target for symlinks
        if let Ok(rel) = target.strip_prefix(&canonical_root) {
            let mut accumulated = canonical_root.clone();
            for comp in rel.components() {
                if let Component::Normal(c) = comp {
                    accumulated = accumulated.join(c);
                    if accumulated.is_symlink() {
                        let link_dest = fs::read_link(&accumulated).map_err(|e| {
                            HgbError::Security(format!(
                                "Path boundary violation: failed to read symlink '{}': {e}",
                                accumulated.display()
                            ))
                        })?;
                        let resolved = if link_dest.is_absolute() {
                            link_dest
                        } else {
                            accumulated.parent().unwrap_or(&canonical_root).join(link_dest)
                        };
                        let resolved_canon = resolved.canonicalize().unwrap_or(resolved);
                        if !resolved_canon.starts_with(&canonical_root) {
                            return Err(HgbError::Security(format!(
                                "Path boundary violation: intermediate symlink '{}' points outside jail root: '{}'",
                                accumulated.display(),
                                resolved_canon.display()
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_jail_valid_paths() {
        let temp = std::env::temp_dir().join(format!("path_jail_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp);
        let jail = PathJail::new(&temp).expect("Create jail");

        let valid = jail.check_boundary("src/lib.rs").expect("Valid path");
        assert_eq!(valid, jail.root().join("src/lib.rs"));

        assert!(jail.is_safe("deep/nested/directory/file.txt"));
        assert!(jail.is_safe("Cargo.toml"));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_path_jail_rejects_traversal() {
        let temp = std::env::temp_dir().join(format!("path_jail_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp);
        let jail = PathJail::new(&temp).expect("Create jail");

        assert!(jail.check_boundary("../outside.txt").is_err());
        assert!(jail.check_boundary("src/../../outside.txt").is_err());
        assert!(jail.check_boundary("a/b/c/../../../../etc/passwd").is_err());
        assert!(!jail.is_safe("../evil.sh"));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_path_jail_rejects_root_and_prefix() {
        let temp = std::env::temp_dir().join(format!("path_jail_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp);
        let jail = PathJail::new(&temp).expect("Create jail");

        assert!(jail.check_boundary("/etc/passwd").is_err());
        assert!(jail.check_boundary("/var/log").is_err());
        assert!(jail.check_boundary("\\windows\\system32").is_err());
        assert!(!jail.is_safe("/bin/sh"));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_path_jail_rejects_symlink_escape() {
        let temp = std::env::temp_dir().join(format!("path_jail_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp);
        let jail = PathJail::new(&temp).expect("Create jail");

        // Create symlink pointing outside jail
        let escape_link = temp.join("escape_link");
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink("/etc", &escape_link);
            if escape_link.exists() || escape_link.is_symlink() {
                assert!(jail.check_boundary("escape_link/passwd").is_err());
                assert!(jail.check_boundary("escape_link").is_err());
                assert!(!jail.is_safe("escape_link/passwd"));
            }
        }

        let _ = fs::remove_dir_all(&temp);
    }
}
