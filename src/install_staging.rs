//! Launcher-owned *local* scratch space for operations sourced from network Vaults.
//!
//! Do not use this for the downloader's resumable Vault package staging.
//! This is a building block for verified Add-to-Project/Create-Project/Install-to-Engine
//! publication; the workflows must explicitly opt into it.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// Check whether a path is on a local Windows fixed drive. No directories are created.
/// Mapped remote drives and UNC paths are rejected. Unknown drive types are rejected.
#[cfg(windows)]
pub fn ensure_local_path(path: &Path) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    // Win32 DRIVE_FIXED: local fixed disk (GetDriveTypeW return value).
    const DRIVE_FIXED: u32 = 3;
    if !path.is_absolute() {
        return Err("Staging must use an absolute local drive path".into());
    }
    let mut components = path.components();
    let root = match components.next() {
        Some(std::path::Component::Prefix(prefix)) => {
            use std::path::Prefix;
            match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    format!("{}:{}", char::from(drive), std::path::MAIN_SEPARATOR)
                }
                _ => return Err("Staging must use a local drive path".into()),
            }
        }
        _ => return Err("Staging requires an absolute local drive path".into()),
    };
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
    if kind != DRIVE_FIXED {
        return Err("Staging drive must be local fixed storage (network/removable/unknown drives are rejected)".into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn ensure_local_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Staging path must be absolute".into());
    }
    // The application currently targets Windows. Don't guess locality for
    // arbitrary Linux NFS/CIFS mountpoints: fail closed until mount discovery
    // is implemented for that platform.
    Err("Local-versus-network staging detection is currently supported on Windows only".into())
}

pub struct InstallStage {
    path: PathBuf,
    /// A stage is disposable only after rollback/recovery has succeeded.
    cleanup_on_drop: bool,
}
impl InstallStage {
    /// Create an empty, unique application-owned directory under a validated
    /// user-selected local staging root. Never remove the parent/root itself.
    pub fn create(root: &Path) -> Result<Self, String> {
        ensure_local_path(root)?;
        fs::create_dir_all(root).map_err(|e| format!("Cannot create staging root: {e}"))?;
        crate::installer::no_links(root)?;
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        ensure_local_path(&root)?;
        for _ in 0..16 {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            let next = NONCE.fetch_add(1, Ordering::Relaxed);
            let candidate = root.join(format!("dnw-install-{}-{tick}-{next}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    return Ok(Self {
                        path: candidate,
                        cleanup_on_drop: true,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Cannot reserve staging folder: {e}")),
            }
        }
        Err("Could not reserve a unique staging folder".into())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Reserve room for payload data and a small housekeeping margin.
    /// Must be called before any bulk transfer starts.
    pub fn require_space(&self, payload_bytes: u64) -> Result<(), String> {
        let free = fs2::available_space(&self.path).map_err(|e| e.to_string())?;
        if free < payload_bytes.saturating_add(64 * 1024 * 1024) {
            return Err(format!(
                "Not enough local staging space: need at least {} bytes, available {}",
                payload_bytes.saturating_add(64 * 1024 * 1024),
                free
            ));
        }
        Ok(())
    }
    /// Retain data for explicit recovery handling after an incomplete publish.
    pub fn retain_for_recovery(&mut self) {
        self.cleanup_on_drop = false;
    }
    /// Explicit cleanup reports errors. `Drop` is only best-effort.
    pub fn cleanup(mut self) -> Result<(), String> {
        self.cleanup_on_drop = false;
        crate::installer::no_links(&self.path)?;
        fs::remove_dir_all(&self.path).map_err(|e| format!("Staging cleanup failed: {e}"))
    }
}
impl Drop for InstallStage {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = crate::installer::no_links(&self.path)
                .and_then(|_| fs::remove_dir_all(&self.path).map_err(|e| e.to_string()));
        }
    }
}

/// Exact artifact lookup against the existing Vault inventory. A healthy badge
/// alone does not authorize use; the authenticated Fab manifest remains the
/// source of expected file hashes and build identity.
pub struct CachedPackage {
    root: PathBuf,
    _stage: Option<InstallStage>,
}
impl CachedPackage {
    pub fn find(manifest: &crate::buildpatch::Manifest) -> Result<Option<Self>, String> {
        use crate::vault_health::Health;
        let matches: Vec<_> = crate::vault_health::load()
            .records
            .into_values()
            .filter(|r| {
                r.path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&manifest.app))
                    && r.identities.iter().any(|id| id == &manifest.app)
                    && r.health != Health::Redownload
                    && r.path.is_dir()
            })
            .collect();
        if matches.is_empty() {
            return Ok(None);
        }
        if matches.len() != 1 {
            return Err("Multiple Vault locations contain this artifact; select one explicitly before installing".into());
        }
        let record = &matches[0];
        crate::installer::no_links(&record.path)?;
        // Reject a cache whose own embedded manifest disagrees with the
        // authenticated current Fab manifest, even if its folder name matches.
        if !record.path.join("manifest").is_file() {
            // Older Vault caches without an authoritative manifest are not
            // eligible for local reuse. Preserve the existing CDN fallback.
            return Ok(None);
        }
        let raw = fs::read(record.path.join("manifest"))
            .map_err(|e| format!("Cached artifact manifest missing: {e}"))?;
        if raw.len() > 128 * 1024 * 1024 {
            return Err("Oversized cached artifact manifest".into());
        }
        let saved = crate::buildpatch::parse(&raw)?;
        if saved.app != manifest.app || saved.build != manifest.build {
            return Err("Vault artifact build differs from the selected Fab release; refusing cached installation".into());
        }
        let expected: std::collections::HashMap<_, _> = manifest
            .files
            .iter()
            .map(|f| (f.name.as_str(), (f.sha, f.size())))
            .collect();
        if saved.files.len() != manifest.files.len()
            || saved.files.iter().any(|f| {
                expected
                    .get(f.name.as_str())
                    .is_none_or(|(hash, size)| *hash != f.sha || *size != f.size())
            })
        {
            return Err(
                "Vault manifest file inventory disagrees with the current Fab manifest".into(),
            );
        }
        let root = record.path.join("data");
        crate::installer::no_links(&root)?;
        // Inspect the entire manifest inventory before returning anything usable.
        for file in &manifest.files {
            let relative = crate::installer::relative(&file.name)?;
            let source = root.join(relative);
            crate::installer::no_links(&source)?;
            if !fs::metadata(&source).is_ok_and(|m| m.is_file() && m.len() == file.size()) {
                return Err(format!("Vault artifact is incomplete: {}", file.name));
            }
        }
        Ok(Some(Self { root, _stage: None }))
    }
    /// On Windows a network Vault is first copied to the configured local
    /// staging drive. Each staged file is SHA-1 checked as it is copied.
    pub fn prepare(
        &mut self,
        files: &[&crate::buildpatch::File],
        control: &crate::installer::Control,
        progress: &impl Fn(String),
    ) -> Result<(), String> {
        if ensure_local_path(&self.root).is_ok() {
            return Ok(());
        }
        let prefs = crate::storage::load_preferences();
        let stage = InstallStage::create(Path::new(&prefs.local_install_staging_root))?;
        let total = files.iter().try_fold(0u64, |sum, f| {
            sum.checked_add(f.size()).ok_or("Stage size overflow")
        })?;
        stage.require_space(total)?;
        for (i, f) in files.iter().enumerate() {
            control.check()?;
            progress(format!(
                "Staging verified Vault file {} / {}",
                i + 1,
                files.len()
            ));
            let relative = crate::installer::relative(&f.name)?;
            let target = stage.path().join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            copy_verified(&self.root.join(&f.name), &target, f, control)?;
        }
        self.root = stage.path().to_owned();
        self._stage = Some(stage);
        Ok(())
    }
    pub fn copy(
        &self,
        file: &crate::buildpatch::File,
        target: &Path,
        control: &crate::installer::Control,
    ) -> Result<(), String> {
        let relative = crate::installer::relative(&file.name)?;
        copy_verified(&self.root.join(relative), target, file, control)
    }
}

fn copy_verified(
    source: &Path,
    target: &Path,
    file: &crate::buildpatch::File,
    control: &crate::installer::Control,
) -> Result<(), String> {
    use sha1::{Digest, Sha1};
    use std::io::{Read, Write};
    control.check()?;
    crate::installer::no_links(source)?;
    crate::installer::no_links(target)?;
    let mut src = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut dest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| e.to_string())?;
    let mut hash = Sha1::new();
    let mut written = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    let result = (|| {
        loop {
            control.check()?;
            let n = src.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            written = written.checked_add(n as u64).ok_or("Copy size overflow")?;
            if written > file.size() {
                return Err("Vault file exceeds manifest size".into());
            }
            hash.update(&buffer[..n]);
            dest.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        if written != file.size() || hash.finalize().as_slice() != file.sha.as_slice() {
            return Err(format!("Vault SHA-1 verification failed: {}", file.name));
        }
        dest.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    })();
    drop(dest);
    if result.is_err() {
        let _ = fs::remove_file(target);
    }
    result
}
