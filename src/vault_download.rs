//! Cache-only Fab package retrieval. Never opens or changes an Unreal project.
use crate::{
    epic::Session,
    fab::{self, Item, Version},
    installer::{self, Control},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum VaultMode {
    New,
    Replace {
        folder: String,
        backup_directory: Option<PathBuf>,
    },
}

pub fn download_with_control(
    session: &Session,
    item: &Item,
    version: &Version,
    vault_root: &Path,
    control: &Control,
    progress: impl Fn(String),
) -> Result<String, String> {
    download_with_mode(
        session,
        item,
        version,
        vault_root,
        control,
        VaultMode::New,
        progress,
    )
}

pub fn download_with_mode(
    session: &Session,
    item: &Item,
    version: &Version,
    vault_root: &Path,
    control: &Control,
    mode: VaultMode,
    progress: impl Fn(String),
) -> Result<String, String> {
    download_with_mode_staged(
        session, item, version, vault_root, control, mode, None, progress,
    )
}

/// An opaque staging folder name is persisted with the queue job. Only names
/// generated for the same artifact are accepted on resume or cancellation.
pub fn stage_name(artifact: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_nanos())
        .unwrap_or_default();
    format!(
        ".engine-workspace-vault-{artifact}-{}-{nonce}",
        std::process::id()
    )
}

fn stage_path(root: &Path, artifact: &str, name: &str) -> Result<PathBuf, String> {
    let prefix = format!(".engine-workspace-vault-{artifact}-");
    if !name.starts_with(&prefix)
        || name.len() <= prefix.len()
        || !name[prefix.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid saved Vault staging identity".into());
    }
    let path = root.join(name);
    installer::no_links(&path)?;
    Ok(path)
}

pub fn cleanup_staged_job(vault_root: &Path, artifact: &str, name: &str) -> Result<(), String> {
    if !vault_root.is_dir() {
        return Ok(());
    }
    let root = fs::canonicalize(vault_root).map_err(|e| e.to_string())?;
    let path = stage_path(&root, artifact, name)?;
    if path.exists() {
        fs::remove_dir_all(&path)
            .map_err(|e| format!("Cannot remove cancelled staging data: {e}"))?;
    }
    Ok(())
}

pub fn download_with_mode_staged(
    session: &Session,
    item: &Item,
    version: &Version,
    vault_root: &Path,
    control: &Control,
    mode: VaultMode,
    saved_stage: Option<&str>,
    progress: impl Fn(String),
) -> Result<String, String> {
    control.check()?;
    if !vault_root.is_dir() {
        return Err(format!(
            "Vault Cache folder does not exist: {}",
            vault_root.display()
        ));
    }
    installer::no_links(vault_root)?;
    let root = fs::canonicalize(vault_root).map_err(|e| e.to_string())?;
    if !version
        .artifact_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || version.artifact_id.is_empty()
    {
        return Err("Unsafe Fab artifact identifier".into());
    }
    let target_folder = match &mode {
        VaultMode::New => version.artifact_id.as_str(),
        VaultMode::Replace { folder, .. } => folder.as_str(),
    };
    if target_folder.is_empty()
        || target_folder == "."
        || target_folder == ".."
        || !target_folder
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("Unsafe Vault replacement folder".into());
    }
    let destination = root.join(target_folder);
    installer::no_links(&destination)?;
    if destination.exists() && matches!(&mode, VaultMode::New) {
        return Err(format!(
            "Vault package already exists: {}. Use Re-download Asset after a failed verification.",
            destination.display()
        ));
    }
    if matches!(&mode, VaultMode::Replace { .. }) && !destination.is_dir() {
        return Err("Existing package is missing or is not a directory".into());
    }
    progress("Reading verified Fab manifest...".into());
    let (manifest, bases, raw_manifest) = fab::package_with_raw(session, item, version)?;
    control.check()?;
    if raw_manifest.is_empty() {
        return Err("Missing original package manifest".into());
    }
    let mut paths = HashSet::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    for file in &manifest.files {
        let relative = installer::relative(&file.name)?;
        let key = relative.to_string_lossy().to_lowercase();
        if !paths.insert(key) {
            return Err("Duplicate filename in Fab manifest".into());
        }
        total = total
            .checked_add(file.size())
            .ok_or("Package size overflow")?;
        files.push((file, relative));
    }
    if files.is_empty() {
        return Err("Empty Fab package".into());
    }
    if fs2::available_space(&root).map_err(|e| e.to_string())?
        < total.saturating_add(64 * 1024 * 1024)
    {
        return Err("Not enough free space in selected Vault Cache".into());
    }
    let generated = stage_name(&version.artifact_id);
    let stage = stage_path(
        &root,
        &version.artifact_id,
        saved_stage.unwrap_or(&generated),
    )?;
    if stage.exists() {
        if !stage.is_dir() {
            return Err("Saved Vault staging location is not a directory".into());
        }
    } else {
        fs::create_dir(&stage).map_err(|e| format!("Cannot create Vault staging folder: {e}"))?;
    }
    let result = (|| {
        fs::create_dir_all(stage.join("data")).map_err(|e| e.to_string())?;
        let mut chunks = HashMap::new();
        let mut written = 0u64;
        for (index, (file, relative)) in files.iter().enumerate() {
            control.check()?;
            let path = stage.join("data").join(relative);
            installer::no_links(&path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            if path.exists() {
                // Check SHA-1 before trusting a file from an earlier run.
                use sha1::{Digest, Sha1};
                let mut valid = false;
                if fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == file.size()) {
                    let mut input = fs::File::open(&path).map_err(|e| e.to_string())?;
                    let mut hash = Sha1::new();
                    use std::io::Read;
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
                        if count == 0 {
                            break;
                        }
                        hash.update(&buffer[..count]);
                    }
                    valid = hash.finalize().as_slice() == file.sha.as_slice();
                }
                if valid {
                    written = written.saturating_add(file.size());
                    progress(format!(
                        "Vault Cache: file {} / {} · {} / {} bytes",
                        index + 1,
                        files.len(),
                        written,
                        total
                    ));
                    continue;
                }
                // Partial or invalid file; rebuild safely from validated chunks.
                fs::remove_file(&path)
                    .map_err(|e| format!("Cannot reset unfinished staged file: {e}"))?;
            }
            let mut last_report = std::time::Instant::now();
            crate::plugins::payload_to_file_controlled(
                &manifest,
                &bases,
                file,
                &mut chunks,
                &path,
                control,
                &mut |current, _received| {
                    if last_report.elapsed().as_millis() >= 200 || current == file.size() {
                        progress(format!(
                            "Vault Cache: file {} / {} · {} / {} bytes",
                            index + 1,
                            files.len(),
                            written.saturating_add(current),
                            total
                        ));
                        last_report = std::time::Instant::now();
                    }
                },
            )?;
            written = written.saturating_add(file.size());
        }
        control.check()?;
        // Each file was SHA-1 checked by payload_to_file_controlled. Before
        // publishing, also verify the complete staged file inventory by size.
        progress("Validating staged Vault package files...".into());
        for (file, relative) in &files {
            control.check()?;
            let path = stage.join("data").join(relative);
            installer::no_links(&path)?;
            let metadata =
                fs::metadata(&path).map_err(|e| format!("Cannot validate {}: {e}", file.name))?;
            if !metadata.is_file() || metadata.len() != file.size() {
                return Err(format!("Vault file size validation failed: {}", file.name));
            }
        }
        fs::write(stage.join("manifest"), &raw_manifest).map_err(|e| e.to_string())?;
        // Persist trusted *expected* SHA-1 values beside the payload. Each
        // reconstructed payload has already been checked by the downloader.
        // This receipt is written while still in staging, before publication.
        crate::vault_health::write_download_index(
            &stage,
            &manifest,
            Path::new(""),
            Path::new("data"),
        )?;
        // Download and verify before changing the original package. The original
        // remains recoverable until the new directory is successfully published.
        match &mode {
            VaultMode::New => {
                if destination.exists() {
                    return Err(
                        "Vault package appeared during download; refusing to overwrite".into(),
                    );
                }
                fs::rename(&stage, &destination)
                    .map_err(|e| format!("Could not publish downloaded Vault package: {e}"))?;
            }
            VaultMode::Replace {
                backup_directory, ..
            } => {
                publish_replacement(
                    &stage,
                    &destination,
                    &root,
                    backup_directory.as_deref(),
                    &progress,
                )?;
            }
        }
        // Fingerprint paths and metadata after the final package name is known.
        // This requires directory enumeration but never re-reads payload bytes.
        // A receipt persistence error must not suggest the successful publish
        // failed and encourage an unsafe re-download over the intact package.
        if let Err(error) = crate::vault_health::record_verified_vault_download(
            &root,
            &destination,
            &item.title,
            &manifest,
        ) {
            progress(format!(
                "Vault download completed, but verification history could not be saved: {error}"
            ));
        }
        Ok(format!(
            "Cached {} in {}",
            item.title,
            destination.display()
        ))
    })();
    // Failed and paused attempts preserve their staged data for inspection/retry.
    // Cancellation cleanup is performed by the caller after the worker stops.
    result
}

/// Reject junctions and symlinks at every level before reading/copying a backup.
fn copy_tree_verified(source: &Path, destination: &Path) -> Result<(), String> {
    installer::no_links(source)?;
    if destination.exists() {
        return Err(format!("Backup already exists: {}", destination.display()));
    }
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        installer::no_links(&from)?;
        let to = destination.join(entry.file_name());
        let meta = fs::symlink_metadata(&from).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            copy_tree_verified(&from, &to)?;
        } else if meta.is_file() {
            fs::copy(&from, &to).map_err(|e| format!("Backup copy {}: {e}", from.display()))?;
            let copied = fs::metadata(&to).map_err(|e| e.to_string())?;
            if copied.len() != meta.len() {
                return Err(format!("Backup copy size mismatch: {}", to.display()));
            }
        } else {
            return Err(format!("Unsupported package entry: {}", from.display()));
        }
    }
    Ok(())
}

fn publish_replacement(
    stage: &Path,
    destination: &Path,
    vault_root: &Path,
    backup_directory: Option<&Path>,
    progress: &impl Fn(String),
) -> Result<(), String> {
    installer::no_links(destination)?;
    if !destination.is_dir() {
        return Err("Original package disappeared during download".into());
    }
    let name = destination
        .file_name()
        .ok_or("Invalid package destination")?
        .to_string_lossy();
    let now = time::OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let mut rollback = vault_root.join(format!(".engine-workspace-rollback-{name}-{stamp}"));
    let mut ordinal = 0;
    while rollback.exists() {
        ordinal += 1;
        rollback = vault_root.join(format!(
            ".engine-workspace-rollback-{name}-{stamp}-{ordinal}"
        ));
    }
    installer::no_links(&rollback)?;

    // A backup is copied into a private temp directory first, leaving the
    // original untouched. It is published only after the replacement succeeds.
    let backup = if let Some(dir) = backup_directory {
        let parent = dir.parent().ok_or("Invalid backup location")?;
        if !parent.is_dir() {
            return Err("Selected backup parent folder is not available".into());
        }
        installer::no_links(parent)?;
        let root_real = fs::canonicalize(vault_root).map_err(|e| e.to_string())?;
        let backup_real = fs::canonicalize(parent).map_err(|e| e.to_string())?;
        if backup_real.starts_with(&root_real) || root_real.starts_with(&backup_real) {
            return Err("Backup destination cannot contain or be inside the Vault Cache".into());
        }
        // The UI captures the minute-specific parent when the job is queued.
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut target = dir.join(name.as_ref());
        let mut suffix = 2;
        while target.exists() {
            target = dir.join(format!("{name}_{suffix}"));
            suffix += 1;
        }
        let pending = dir.join(format!(
            ".engine-workspace-copying-{name}-{}",
            std::process::id()
        ));
        if pending.exists() {
            return Err("A backup copy is already in progress for this asset".into());
        }
        progress("Copying existing package to selected backup location...".into());
        let copied = copy_tree_verified(destination, &pending);
        if let Err(e) = copied {
            let _ = fs::remove_dir_all(&pending);
            return Err(e);
        }
        // Structural checks on the backup before any original package is moved.
        // A failed original is expected here. Copy sizes have already been
        // checked; do not demand that its original manifest be complete.
        Some((pending, target))
    } else {
        None
    };

    fs::rename(destination, &rollback)
        .map_err(|e| format!("Cannot reserve original for rollback: {e}"))?;
    if let Err(e) = fs::rename(stage, destination) {
        let restore = fs::rename(&rollback, destination);
        if let Some((pending, _)) = &backup {
            let _ = fs::remove_dir_all(pending);
        }
        return Err(format!(
            "Cannot publish replacement: {e}. Rollback restore: {restore:?}"
        ));
    }
    // Check the newly published manifest and reconstructed payload sizes.
    let report = crate::vault_scan::verify_package(vault_root, name.as_ref());
    if !matches!(report, Ok(ref v) if v.complete_structure == 1) {
        let failed = vault_root.join(format!(".engine-workspace-failed-{name}-{stamp}"));
        let _ = fs::rename(destination, &failed);
        let restore = fs::rename(&rollback, destination);
        if let Some((pending, _)) = &backup {
            let _ = fs::remove_dir_all(pending);
        }
        return Err(format!(
            "Published replacement failed structural validation. Restore: {restore:?}; retained failed payload: {}",
            failed.display()
        ));
    }
    if let Some((pending, target)) = backup {
        if let Err(e) = fs::rename(&pending, &target) {
            // Preserve the original rollback directory as well as the new
            // published package rather than deleting unbacked data.
            return Err(format!(
                "Replacement installed, but backup finalisation failed: {e}. Original retained at {}",
                rollback.display()
            ));
        }
    }
    if let Err(e) = fs::remove_dir_all(&rollback) {
        progress(format!(
            "Replacement installed. Original rollback retained at {} ({e})",
            rollback.display()
        ));
    }
    Ok(())
}
