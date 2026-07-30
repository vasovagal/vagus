//! Fail-closed path identity and containment helpers.
//!
//! Paths used for the vault, derived data, caches, and explicit exports may be relative, missing, or
//! spelled through symlink aliases. Canonicalizing only an existing prefix loses the unresolved suffix;
//! comparing only lexical paths misses aliases. These helpers do both without creating anything.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

/// Resolve `path` from the process current directory, canonicalizing its nearest existing ancestor
/// and retaining every unresolved suffix component.
pub fn resolve(path: &Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    resolve_from(path, &cwd)
}

/// Testable form of [`resolve`] with an explicit current directory for relative paths.
pub fn resolve_from(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let absolute = absolute_from(path, cwd)?;
    resolve_existing_ancestor(&absolute)
}

/// Lexically normalize an absolute or cwd-relative path without requiring it to exist.
pub fn absolute_from(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut clean = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => clean.push(prefix.as_os_str()),
            Component::RootDir => clean.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    bail!("path {} escapes the filesystem root", path.display())
                }
            }
            Component::Normal(part) => clean.push(part),
        }
    }
    if !clean.is_absolute() {
        bail!("could not resolve {} to an absolute path", path.display())
    }
    Ok(clean)
}

/// Whether either resolved path is equal to or nested below the other.
pub fn overlap(a: &Path, b: &Path) -> Result<bool> {
    let a = resolve(a)?;
    let b = resolve(b)?;
    Ok(a.starts_with(&b) || b.starts_with(&a))
}

/// Whether `child` resolves to `parent` or a descendant of it.
pub fn at_or_within(child: &Path, parent: &Path) -> Result<bool> {
    Ok(resolve(child)?.starts_with(resolve(parent)?))
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut probe = path.to_path_buf();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&probe) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = probe.file_name().ok_or_else(|| {
                    anyhow!("cannot find an existing ancestor for {}", path.display())
                })?;
                suffix.push(name.to_os_string());
                if !probe.pop() {
                    bail!("cannot find an existing ancestor for {}", path.display())
                }
            }
            Err(e) => return Err(e).with_context(|| format!("inspecting {}", probe.display())),
        }
    }
    let mut resolved = probe
        .canonicalize()
        .with_context(|| format!("resolving {}", probe.display()))?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    absolute_from(&resolved, Path::new(std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    #[test]
    fn missing_suffix_and_dotdot_survive_resolution() {
        let dir = TempDir::new("path-safety-missing");
        let p = resolve_from(Path::new("a/new/../../vault/export"), dir.path()).unwrap();
        assert_eq!(p, dir.path().canonicalize().unwrap().join("vault/export"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_compare_by_target() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new("path-safety-alias");
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let alias = dir.path().join("alias");
        symlink(&real, &alias).unwrap();
        assert!(at_or_within(&alias.join("missing"), &real).unwrap());
        assert!(overlap(&alias, &real.join("child")).unwrap());
    }
}
