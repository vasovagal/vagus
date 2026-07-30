//! `vagus init` — explicit, fail-closed PARA vault setup.
//!
//! `--icloud` creates/uses the fixed iCloud Drive `Brain` directory and places a friendly symlink at
//! the configured vault path. Every source/target identity and occupancy check happens before mutation.
//! Existing notes are never moved or deleted: an occupied local vault receives conflict-safe manual
//! migration guidance, while only an exactly recognized empty PARA skeleton may be removed (G1/G3/G15).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;

/// The fixed PARA layout (guardrail G15).
pub const PARA_DIRS: [&str; 5] = [
    "00-Inbox",
    "10-Projects",
    "20-Areas",
    "30-Resources",
    "40-Archive",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceState {
    Missing,
    DisposableParaSkeleton,
    Occupied,
}

pub fn run(cfg: &Config, icloud: bool) -> Result<()> {
    if icloud {
        let home = dirs::home_dir().context("cannot resolve home directory")?;
        let target = icloud_brain_dir(&home);
        let cloud_root = target.parent().expect("fixed iCloud target has a parent");
        if !cloud_root.is_dir() {
            bail!(
                "iCloud Drive not found at {} — enable iCloud Drive in System Settings, or run plain `vagus init` for a local vault",
                cloud_root.display()
            )
        }
        link_vault(&cfg.vault, &target)?;
        if crate::path_safety::resolve(&cfg.vault)? == crate::path_safety::resolve(&target)?
            && fs::symlink_metadata(&cfg.vault)
                .map(|m| !m.file_type().is_symlink())
                .unwrap_or(false)
        {
            println!("vault: {} (iCloud Drive)", target.display());
        } else {
            println!("vault: {} → {}", cfg.vault.display(), target.display());
        }
    } else {
        init_local(&cfg.vault)?;
        println!("vault: {}", cfg.vault.display());
        if cfg!(target_os = "macos")
            && !is_icloud_path(&cfg.vault.canonicalize().unwrap_or_default())
        {
            println!(
                "  (local folder only — `vagus init --icloud` puts it in iCloud Drive for device sync)"
            );
        }
    }
    println!("PARA folders ready: {}", PARA_DIRS.join(" · "));
    println!(
        r#"
next:
  vagus tutorial              the capture → search → file workflow
  vagus add-note "My idea"    capture a first note (or drop Markdown into 00-Inbox/)
  vagus index                 first use downloads the embedder (~1.2 GB, one-time)"#
    );
    Ok(())
}

/// Create (or complete) the vault as a plain directory. A valid directory symlink is allowed; a
/// broken link or any non-directory is rejected without mutation.
fn init_local(vault: &Path) -> Result<()> {
    match fs::symlink_metadata(vault) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", vault.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            if !vault.is_dir() {
                bail!(
                    "{} is a broken or non-directory symlink — remove it or restore its target",
                    vault.display()
                )
            }
        }
        Ok(meta) if !meta.is_dir() => {
            bail!("{} exists and is not a directory", vault.display())
        }
        Ok(_) => {}
    }
    ensure_para(vault)
}

/// Point `vault` at `target` without ever moving existing notes. The target is not created until all
/// source/target identity and occupancy checks pass.
fn link_vault(vault: &Path, target: &Path) -> Result<()> {
    // Handle symlinks before generic canonical resolution so an idempotent but temporarily dangling
    // link can be repaired by creating its intended iCloud target.
    if let Ok(meta) = fs::symlink_metadata(vault)
        && meta.file_type().is_symlink()
    {
        let raw =
            fs::read_link(vault).with_context(|| format!("reading symlink {}", vault.display()))?;
        let parent = vault.parent().unwrap_or_else(|| Path::new("/"));
        let linked = crate::path_safety::resolve_from(&raw, parent)?;
        let intended = crate::path_safety::resolve(target)?;
        if linked != intended {
            bail!(
                "{} is already a symlink to {} — refusing to replace it",
                vault.display(),
                raw.display()
            )
        }
        validate_target(target)?;
        ensure_para(target)?;
        return Ok(());
    }

    let resolved_vault = crate::path_safety::resolve(vault)?;
    let resolved_target = crate::path_safety::resolve(target)?;
    if resolved_vault == resolved_target {
        // `VAGUS_VAULT` already names the iCloud Brain directory: initialize in place, never create a
        // self-referential symlink.
        validate_target(target)?;
        ensure_para(target)?;
        return Ok(());
    }
    if crate::path_safety::overlap(vault, target)? {
        bail!(
            "vault {} and iCloud target {} overlap — neither path was changed",
            vault.display(),
            target.display()
        )
    }

    let source_state = inspect_source(vault)?;
    validate_target(target)?;
    if source_state == SourceState::Occupied {
        if target.exists() {
            bail!(
                "{} contains existing entries and {} already exists. Merge the Markdown into the iCloud target manually; neither path was changed, then re-run `vagus init --icloud`",
                vault.display(),
                target.display()
            )
        }
        bail!(
            "{} contains existing entries. Move or rename that whole vault to the not-yet-existing iCloud target {} (do not move it into an existing Brain directory), then re-run `vagus init --icloud`; neither path was changed",
            vault.display(),
            target.display()
        )
    }

    // Mutation begins only after the complete preflight above. Create/complete the target first, so
    // even a later symlink failure leaves the intended empty iCloud skeleton available for retry.
    ensure_para(target)?;
    if source_state == SourceState::DisposableParaSkeleton {
        remove_disposable_skeleton(vault)?;
    }
    if let Some(parent) = vault.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, vault)
        .with_context(|| format!("symlinking {} → {}", vault.display(), target.display()))?;
    #[cfg(not(unix))]
    bail!("--icloud needs symlinks and is only supported on unix");
    Ok(())
}

/// Target may be missing or a real directory. A symlink, file, special entry, or unreadable directory
/// is rejected before mutation. Reading the directory also makes traversal/permission errors fatal.
fn validate_target(target: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspecting {}", target.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "iCloud target {} is a symlink; refusing ambiguous setup",
                target.display()
            )
        }
        Ok(meta) if !meta.is_dir() => {
            bail!(
                "iCloud target {} exists and is not a directory",
                target.display()
            )
        }
        Ok(_) => {
            // Do not flatten errors. Existing contents are preserved; this is a readability preflight.
            let entries =
                fs::read_dir(target).with_context(|| format!("inspecting {}", target.display()))?;
            for entry in entries {
                entry.with_context(|| format!("reading {}", target.display()))?;
            }
            Ok(())
        }
    }
}

/// Missing, an exactly disposable empty PARA skeleton, or occupied. Anything unfamiliar—including a
/// symlink/special entry, an extra directory, or any child inside a PARA directory—is occupied.
fn inspect_source(vault: &Path) -> Result<SourceState> {
    let meta = match fs::symlink_metadata(vault) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SourceState::Missing),
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", vault.display())),
        Ok(meta) => meta,
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Ok(SourceState::Occupied);
    }

    let allowed: HashSet<&str> = PARA_DIRS.into_iter().collect();
    let entries = fs::read_dir(vault).with_context(|| format!("inspecting {}", vault.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", vault.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(SourceState::Occupied);
        };
        let kind = entry
            .file_type()
            .with_context(|| format!("inspecting {}", entry.path().display()))?;
        if !allowed.contains(name) || !kind.is_dir() || kind.is_symlink() {
            return Ok(SourceState::Occupied);
        }
        let mut children = fs::read_dir(entry.path())
            .with_context(|| format!("inspecting {}", entry.path().display()))?;
        if children.next().transpose()?.is_some() {
            return Ok(SourceState::Occupied);
        }
    }
    Ok(SourceState::DisposableParaSkeleton)
}

/// Remove only a skeleton that still passes the strict inspection. Every operation is non-recursive;
/// a concurrent/unexpected entry makes `remove_dir` fail rather than deleting it.
fn remove_disposable_skeleton(vault: &Path) -> Result<()> {
    if inspect_source(vault)? != SourceState::DisposableParaSkeleton {
        bail!(
            "{} changed during setup; refusing to remove it",
            vault.display()
        )
    }
    for name in PARA_DIRS {
        let path = vault.join(name);
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing empty {}", path.display())),
        }
    }
    fs::remove_dir(vault).with_context(|| format!("removing empty {}", vault.display()))
}

/// Create the PARA folders under `root` (idempotent).
fn ensure_para(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => {
            bail!("{} exists and is not a directory", root.display())
        }
        Ok(meta) if meta.file_type().is_symlink() && !root.is_dir() => {
            bail!("{} is a broken or non-directory symlink", root.display())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", root.display())),
        _ => {}
    }
    for name in PARA_DIRS {
        let path = root.join(name);
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(())
}

/// Where the vault lives inside iCloud Drive.
fn icloud_brain_dir(home: &Path) -> PathBuf {
    home.join("Library/Mobile Documents/com~apple~CloudDocs/Brain")
}

pub fn is_icloud_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "com~apple~CloudDocs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    #[test]
    fn ensure_para_is_idempotent() {
        let dir = TempDir::new("init-para");
        ensure_para(dir.path()).unwrap();
        ensure_para(dir.path()).unwrap();
        for name in PARA_DIRS {
            assert!(dir.path().join(name).is_dir());
        }
    }

    #[test]
    fn fresh_link_and_existing_target_are_preserved() {
        let dir = TempDir::new("init-link");
        let vault = dir.path().join("brain");
        let target = dir.path().join("clouddocs/Brain");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("existing.md"), "keep").unwrap();
        link_vault(&vault, &target).unwrap();
        assert!(
            fs::symlink_metadata(&vault)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(target.join("existing.md")).unwrap(),
            "keep"
        );
        assert!(vault.join("00-Inbox").is_dir());
        link_vault(&vault, &target).unwrap();
    }

    #[test]
    fn same_path_initializes_in_place_without_self_symlink() {
        let dir = TempDir::new("init-same-path");
        let target = dir.path().join("cloud/Brain");
        link_vault(&target, &target).unwrap();
        assert!(target.is_dir());
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(target.join("00-Inbox").is_dir());
    }

    #[test]
    fn only_exact_empty_para_skeleton_is_replaceable() {
        let dir = TempDir::new("init-disposable");
        let vault = dir.path().join("brain");
        let target = dir.path().join("cloud/Brain");
        ensure_para(&vault).unwrap();
        link_vault(&vault, &target).unwrap();
        assert!(
            fs::symlink_metadata(&vault)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let dir2 = TempDir::new("init-extra-empty-dir");
        let vault2 = dir2.path().join("brain");
        ensure_para(&vault2).unwrap();
        fs::create_dir(vault2.join("custom-empty-folder")).unwrap();
        let target2 = dir2.path().join("cloud/Brain");
        assert!(link_vault(&vault2, &target2).is_err());
        assert!(vault2.join("custom-empty-folder").is_dir());
        assert!(!target2.exists());
    }

    #[test]
    fn occupied_source_and_target_are_never_mutated() {
        let dir = TempDir::new("init-occupied");
        let vault = dir.path().join("brain");
        let target = dir.path().join("cloud/Brain");
        ensure_para(&vault).unwrap();
        fs::write(vault.join("00-Inbox/keep.md"), "# keep").unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("cloud.md"), "# cloud").unwrap();

        let err = link_vault(&vault, &target).unwrap_err();
        assert!(err.to_string().contains("Merge"), "{err}");
        assert_eq!(
            fs::read_to_string(vault.join("00-Inbox/keep.md")).unwrap(),
            "# keep"
        );
        assert_eq!(
            fs::read_to_string(target.join("cloud.md")).unwrap(),
            "# cloud"
        );
    }

    #[test]
    fn occupied_source_with_missing_target_leaves_both_unchanged() {
        let dir = TempDir::new("init-migration-guidance");
        let vault = dir.path().join("brain");
        let target = dir.path().join("cloud/Brain");
        ensure_para(&vault).unwrap();
        fs::write(vault.join("00-Inbox/keep.md"), "# keep").unwrap();
        let err = link_vault(&vault, &target).unwrap_err();
        assert!(err.to_string().contains("not-yet-existing"), "{err}");
        assert!(vault.join("00-Inbox/keep.md").is_file());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entry_makes_source_occupied_and_is_not_followed_or_removed() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new("init-symlink-entry");
        let vault = dir.path().join("brain");
        fs::create_dir(&vault).unwrap();
        let outside = dir.path().join("outside.md");
        fs::write(&outside, "keep").unwrap();
        symlink(&outside, vault.join("link.md")).unwrap();
        let target = dir.path().join("cloud/Brain");
        assert!(link_vault(&vault, &target).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
        assert!(
            fs::symlink_metadata(vault.join("link.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_source_fails_preflight_without_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("init-unreadable");
        let vault = dir.path().join("brain");
        let target = dir.path().join("cloud/Brain");
        ensure_para(&vault).unwrap();
        let inbox = vault.join("00-Inbox");
        fs::set_permissions(&inbox, fs::Permissions::from_mode(0o000)).unwrap();
        let result = link_vault(&vault, &target);
        fs::set_permissions(&inbox, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert!(vault.is_dir());
        assert!(!target.exists());
    }

    #[test]
    fn ancestor_descendant_paths_fail_before_mutation() {
        let dir = TempDir::new("init-overlap");
        let vault = dir.path().join("brain");
        let target = vault.join("Brain");
        assert!(link_vault(&vault, &target).is_err());
        assert!(!vault.exists());
    }

    #[test]
    fn icloud_paths() {
        let target = icloud_brain_dir(Path::new("/Users/x"));
        assert!(is_icloud_path(&target));
        assert!(!is_icloud_path(Path::new("/Users/x/brain")));
    }
}
