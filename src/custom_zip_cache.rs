//! Read-only, persistent catalogue for user-owned ZIP archives.
//! Original archives are never modified or removed by catalogue operations.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};
use twox_hash::XxHash3_128;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Verification {
    #[default]
    Unchecked,
    Checking,
    Valid,
    Invalid(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: PathBuf,
    pub present: bool,
    #[serde(default)]
    pub hidden: bool,
    pub bytes: u64,
    pub modified_unix_secs: Option<u64>,
    pub category: String,
    #[serde(default)]
    pub category_override: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub verification: Verification,
    #[serde(default, alias = "fingerprint")]
    pub full_fingerprint: Option<String>,
    #[serde(default)]
    pub quick_fingerprint: Option<String>,
    #[serde(default)]
    pub verified_entries: u64,
    #[serde(default)]
    pub uncompressed_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Catalogue {
    #[serde(default)]
    pub organization: crate::library_organization::Organization,
    #[serde(default = "fingerprint_spec")]
    pub fingerprint_algorithm: String,
    pub entries: BTreeMap<String, Entry>,
}
impl Default for Catalogue {
    fn default() -> Self {
        Self {
            organization: Default::default(),
            fingerprint_algorithm: fingerprint_spec(),
            entries: BTreeMap::new(),
        }
    }
}
fn fingerprint_spec() -> String {
    "XXH3-128/zip-quick-v1/full-file-v1".into()
}
fn id_for(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    format!("{:032x}", XxHash3_128::oneshot(normalized.as_bytes()))
}
fn metadata_time(m: &fs::Metadata) -> Option<u64> {
    m.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
impl Catalogue {
    fn read_valid(path: &Path) -> Result<Self, String> {
        let raw = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut catalogue: Self = serde_json::from_slice(&raw)
            .map_err(|e| format!("{}: invalid catalogue JSON: {e}", path.display()))?;
        // An interrupted scan must not remain indefinitely in a transient state.
        for entry in catalogue.entries.values_mut() {
            if entry.verification == Verification::Checking {
                entry.verification = Verification::Unchecked;
                entry.full_fingerprint = None;
                entry.quick_fingerprint = None;
                entry.verified_entries = 0;
            }
        }
        if catalogue.fingerprint_algorithm != fingerprint_spec() {
            for entry in catalogue.entries.values_mut() {
                entry.quick_fingerprint = None;
                entry.full_fingerprint = None;
            }
            catalogue.fingerprint_algorithm = fingerprint_spec();
        }
        Ok(catalogue)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let backup = path.with_extension("json.bak");
        match (path.exists(), backup.exists()) {
            (false, false) => Ok(Self::default()),
            (true, _) => match Self::read_valid(path) {
                Ok(c) => Ok(c),
                Err(primary_error) if backup.exists() => Self::read_valid(&backup)
                    .map_err(|backup_error| format!("ZIP catalogue and recovery copy both failed: {primary_error}; {backup_error}")),
                Err(error) => Err(error),
            },
            (false, true) => Self::read_valid(&backup),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = path.with_extension("json.tmp");
        let backup = path.with_extension("json.bak");
        // Refuse to rotate an invalid primary into the known-good backup.
        // If recovery succeeds from backup, replace the corrupt primary only after
        // a complete new temporary file has been written and synced.
        let primary_valid = if path.exists() {
            Self::read_valid(path).is_ok()
        } else {
            false
        };
        let backup_valid = backup.exists() && Self::read_valid(&backup).is_ok();
        if path.exists() && !primary_valid && !backup_valid {
            return Err(
                "Refusing to overwrite corrupt ZIP catalogue without a valid recovery copy".into(),
            );
        }
        {
            let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        // Keep a known-good recovery copy. Do not delete an existing valid backup
        // before publication: the replacement operation can fail on Windows.
        if primary_valid && !backup_valid {
            let backup_temp = path.with_extension("json.bak.tmp");
            fs::copy(path, &backup_temp).map_err(|e| format!("Backup copy failed: {e}"))?;
            // Windows FlushFileBuffers requires a handle opened with write access.
            // File::open is read-only and can fail with Access denied (os error 5),
            // preventing catalogue updates and virtual folders from persisting.
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&backup_temp)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("Backup sync failed: {e}"))?;
            if backup.exists() {
                fs::remove_file(&backup)
                    .map_err(|e| format!("Old corrupt backup removal failed: {e}"))?;
            }
            fs::rename(&backup_temp, &backup)
                .map_err(|e| format!("Backup publication failed: {e}"))?;
        }
        // On Windows, ordinary std::fs::rename cannot atomically replace a file.
        // ReplaceFileW substitutes the complete, synced temporary file while keeping
        // the previous target readable on failure. Never delete the primary first.
        #[cfg(windows)]
        {
            if path.exists() {
                use std::os::windows::ffi::OsStrExt;
                #[link(name = "kernel32")]
                unsafe extern "system" {
                    fn ReplaceFileW(
                        target: *const u16,
                        replacement: *const u16,
                        backup: *const u16,
                        flags: u32,
                        exclude: *mut std::ffi::c_void,
                        reserved: *mut std::ffi::c_void,
                    ) -> i32;
                }
                let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let replacement: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
                let result = unsafe {
                    ReplaceFileW(
                        target.as_ptr(),
                        replacement.as_ptr(),
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if result == 0 {
                    return Err(format!(
                        "Windows catalogue replacement failed: {} (existing catalogue and recovery backup preserved)",
                        io::Error::last_os_error()
                    ));
                }
            } else {
                fs::rename(&temp, path)
                    .map_err(|e| format!("New catalogue publication failed: {e}"))?;
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temp, path).map_err(|e| format!("Catalogue publication failed: {e}"))?;
        Ok(())
    }
    /// Existing records survive a missing folder or disconnected network share.
    pub fn scan(&mut self, roots: &[String]) -> Result<usize, String> {
        let mut discovered = 0;
        let mut seen = HashSet::new();
        for root in roots {
            let folder = Path::new(root);
            if !folder.is_dir() {
                continue;
            }
            let mut pending = vec![folder.to_path_buf()];
            while let Some(dir) = pending.pop() {
                for item in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
                    let item = item.map_err(|e| e.to_string())?;
                    let ty = item.file_type().map_err(|e| e.to_string())?;
                    if ty.is_symlink() {
                        continue;
                    }
                    let p = item.path();
                    if ty.is_dir() {
                        pending.push(p);
                        continue;
                    }
                    if !ty.is_file()
                        || !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                    {
                        continue;
                    }
                    let id = id_for(&p);
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let meta = item.metadata().map_err(|e| e.to_string())?;
                    let time = metadata_time(&meta);
                    let prior = self.entries.get_mut(&id);
                    if let Some(record) = prior {
                        if record.bytes != meta.len() || record.modified_unix_secs != time {
                            record.verification = Verification::Unchecked;
                            // Preserve the last trusted fingerprint as a comparison baseline.
                            // The badge is Unchecked until a fresh whole-file hash matches.
                        }
                        record.present = true;
                        record.bytes = meta.len();
                        record.modified_unix_secs = time;
                    } else {
                        self.entries.insert(
                            id.clone(),
                            Entry {
                                id,
                                path: p,
                                present: true,
                                hidden: false,
                                bytes: meta.len(),
                                modified_unix_secs: time,
                                category: "Unclassified".into(),
                                category_override: None,
                                tags: vec![],
                                verification: Verification::Unchecked,
                                full_fingerprint: None,
                                quick_fingerprint: None,
                                verified_entries: 0,
                                uncompressed_bytes: 0,
                            },
                        );
                    }
                    discovered += 1;
                }
            }
        }
        for entry in self.entries.values_mut() {
            if !seen.contains(&entry.id) {
                entry.present = entry.path.is_file();
            }
        }
        Ok(discovered)
    }
    pub fn remove_record(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }
    pub fn set_tags(&mut self, id: &str, tags: Vec<String>) -> Result<(), String> {
        let entry = self.entries.get_mut(id).ok_or("Unknown ZIP archive")?;
        let mut dedup = HashSet::new();
        entry.tags = tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && dedup.insert(t.to_lowercase()))
            .collect();
        Ok(())
    }
    /// Legacy synchronous catalogue API; callers should prefer the read-only
    /// `check_archive` worker and apply its result in the Library manager.
    pub fn verify(&mut self, id: &str) -> Result<(), String> {
        let entry = self.entries.get_mut(id).ok_or("Unknown ZIP archive")?;
        let checked = check_archive(&entry.path).map_err(|e| e.to_string())?;
        entry.present = true;
        entry.full_fingerprint = Some(checked.fingerprint);
        entry.quick_fingerprint =
            Some(structural_fingerprint(&entry.path).map_err(|e| e.to_string())?);
        entry.verified_entries = checked.entries;
        entry.uncompressed_bytes = checked.expanded_bytes;
        entry.category = checked.category;
        entry.verification = Verification::Valid;
        Ok(())
    }
}
/// Worker outcome: Valid on full inspection; Invalid only for proven ZIP corruption.
/// Filesystem failures are execution errors, not archive validity results.
#[derive(Debug)]
pub enum VerifyError {
    Invalid(String),
    Io(String),
}
impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "{reason}"),
            Self::Io(reason) => write!(f, "{reason}"),
        }
    }
}
#[derive(Debug)]
pub struct ArchiveCheck {
    pub fingerprint: String,
    pub entries: u64,
    pub expanded_bytes: u64,
    pub category: String,
    pub source_bytes: u64,
    pub source_modified_unix_secs: Option<u64>,
}
/// Pure read-only verification, safe to invoke off the UI thread.
pub fn check_archive(path: &Path) -> Result<ArchiveCheck, VerifyError> {
    let meta =
        fs::metadata(path).map_err(|e| VerifyError::Io(format!("{}: {e}", path.display())))?;
    let (fingerprint, entries, expanded_bytes, category) = verify_archive(path)?;
    Ok(ArchiveCheck {
        fingerprint,
        entries,
        expanded_bytes,
        category,
        source_bytes: meta.len(),
        source_modified_unix_secs: metadata_time(&meta),
    })
}

/// Recheck a previously full-CRC-validated archive without decompression.
pub fn full_file_fingerprint(path: &Path) -> Result<String, VerifyError> {
    let mut file = fs::File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut hasher = XxHash3_128::default();
    let mut buf = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| VerifyError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.finish_128()))
}
/// Near-instant metadata fingerprint: path, size and modification time.
/// No archive bytes are read. A mismatch is not proof of corruption.
pub fn structural_fingerprint(path: &Path) -> Result<String, VerifyError> {
    let metadata = fs::metadata(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let mut hasher = XxHash3_128::default();
    for field in [normalized.as_bytes(), &metadata.len().to_le_bytes()[..]] {
        hasher.write(&(field.len() as u64).to_le_bytes());
        hasher.write(field);
    }
    let modified = metadata
        .modified()
        .map_err(|e| VerifyError::Io(e.to_string()))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| VerifyError::Io(e.to_string()))?;
    hasher.write(&modified.as_secs().to_le_bytes());
    hasher.write(&modified.subsec_nanos().to_le_bytes());
    Ok(format!("{:032x}", hasher.finish_128()))
}
fn zip_error(error: zip::result::ZipError) -> VerifyError {
    match error {
        zip::result::ZipError::Io(io) => VerifyError::Io(io.to_string()),
        other => VerifyError::Invalid(other.to_string()),
    }
}
fn verify_archive(path: &Path) -> Result<(String, u64, u64, String), VerifyError> {
    let f = fs::File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut z = zip::ZipArchive::new(f).map_err(zip_error)?;
    if z.len() > 100_000 {
        return Err(VerifyError::Invalid("ZIP entry limit exceeded".into()));
    }
    let mut sum = 0_u64;
    let mut has_project = false;
    let mut has_plugin = false;
    let mut has_asset = false;
    let mut buffer = [0u8; 65536];
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(zip_error)?;
        let name = file.name().replace('\\', "/").to_lowercase();
        if name.ends_with(".uproject") {
            has_project = true;
        }
        if name.ends_with(".uplugin") {
            has_plugin = true;
        }
        if name.ends_with(".uasset") || name.ends_with(".umap") {
            has_asset = true;
        }
        if file.is_symlink() {
            continue;
        }
        if !file.is_file() {
            continue;
        }
        safe_relative_zip_path(file.name()).map_err(VerifyError::Invalid)?;
        sum = sum
            .checked_add(file.size())
            .ok_or_else(|| VerifyError::Invalid("ZIP size overflow".into()))?;
        if sum > 512 * 1024 * 1024 * 1024 {
            return Err(VerifyError::Invalid(
                "ZIP expanded-size limit exceeded (512 GiB)".into(),
            ));
        }
        // zip validates CRC on full entry reads; a truncated read is an error.
        io::copy(&mut file, &mut io::sink()).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                VerifyError::Invalid(format!("CRC/decompression failure in {name}: {e}"))
            } else {
                VerifyError::Io(format!("Reading {name}: {e}"))
            }
        })?;
    }
    let mut reader = fs::File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut hasher = XxHash3_128::default();
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| VerifyError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.write(&buffer[..n]);
    }
    let category = if has_project {
        "Unreal Project"
    } else if has_plugin {
        "Unreal Plugin"
    } else if has_asset {
        "Asset Pack"
    } else {
        "Unclassified"
    };
    // XxHash3_128 exposes a 128-bit finish through finish128.
    Ok((
        format!("{:032x}", hasher.finish_128()),
        z.len() as u64,
        sum,
        category.to_string(),
    ))
}

/// Strictly validate Windows destination semantics even when the scan runs on Unix.
/// No path rewriting: malformed names cause a hard failure.
fn safe_relative_zip_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.contains('\0')
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.contains('\\')
    {
        return Err(format!("Unsafe ZIP entry name: {raw:?}"));
    }
    let mut out = PathBuf::new();
    for part in raw.trim_end_matches('/').split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(':')
            || part.ends_with('.')
            || part.ends_with(' ')
        {
            return Err(format!("Unsafe ZIP path component: {raw:?}"));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0')
        {
            return Err(format!("Windows reserved ZIP entry name: {raw:?}"));
        }
        out.push(part);
    }
    if out.components().any(|c| !matches!(c, Component::Normal(_))) || out.as_os_str().is_empty() {
        return Err(format!("Unsafe ZIP entry path: {raw:?}"));
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkPolicy {
    Stop,
    SkipAll,
}
#[derive(Clone, Debug, Default)]
pub struct ExtractionReport {
    pub regular_files: u64,
    pub directories: u64,
    pub extracted_bytes: u64,
    pub skipped_links: Vec<String>,
}

/// Preflight only, before writing ANY data. Caller presents a link decision if
/// skipped_links is non-empty. Unsupported special entries always fail closed.
pub fn inspect_extraction(path: &Path) -> Result<ExtractionReport, String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if archive.len() > 100_000 {
        return Err("ZIP entry limit exceeded".into());
    }
    let mut report = ExtractionReport::default();
    let mut paths = HashSet::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| e.to_string())?;
        let relative = safe_relative_zip_path(file.name())?;
        let key = relative.to_string_lossy().to_lowercase();
        if !paths.insert(key) {
            return Err(format!(
                "Duplicate case-insensitive ZIP path: {}",
                file.name()
            ));
        }
        if file.is_symlink() {
            report.skipped_links.push(file.name().to_string());
        } else if file.is_dir() {
            report.directories += 1;
        } else if file.is_file() {
            report.regular_files += 1;
            report.extracted_bytes = report
                .extracted_bytes
                .checked_add(file.size())
                .ok_or("Expanded size overflow")?;
            if report.extracted_bytes > 512 * 1024 * 1024 * 1024 {
                return Err("Expanded size exceeds 512 GiB safety limit".into());
            }
        } else {
            return Err(format!("Unsupported special ZIP entry: {}", file.name()));
        }
    }
    Ok(report)
}

/// Only normal files/directories are written. Output must be a new, exclusively
/// owned staging folder, never an existing project or a shared staging root.
/// A failed extraction remains in staging for the owner to inspect/clean; no
/// target project is modified by this function.
pub fn extract_into_new_staging(
    archive_path: &Path,
    staging_dir: &Path,
    policy: LinkPolicy,
) -> Result<ExtractionReport, String> {
    let expected = inspect_extraction(archive_path)?;
    if !expected.skipped_links.is_empty() && policy == LinkPolicy::Stop {
        return Err(format!(
            "Archive contains {} link entries; user decision required",
            expected.skipped_links.len()
        ));
    }
    if staging_dir.exists() {
        return Err("Staging destination already exists; refusing to overwrite".into());
    }
    let parent = staging_dir.parent().ok_or("Staging parent missing")?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err("Unsafe staging parent".into());
    }
    fs::create_dir(staging_dir).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(fs::File::open(archive_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut report = ExtractionReport {
        skipped_links: expected.skipped_links,
        ..ExtractionReport::default()
    };
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let relative = safe_relative_zip_path(entry.name())?;
        if entry.is_symlink() {
            continue;
        }
        let target = staging_dir.join(relative);
        let directory = if entry.is_dir() {
            target.as_path()
        } else {
            target.parent().ok_or("Missing output parent")?
        };
        // Never traverse existing reparse points/symlinks within the staging tree.
        let mut cursor = staging_dir.to_path_buf();
        for component in directory
            .strip_prefix(staging_dir)
            .map_err(|e| e.to_string())?
            .components()
        {
            cursor.push(component);
            if cursor.exists() {
                let meta = fs::symlink_metadata(&cursor).map_err(|e| e.to_string())?;
                if meta.file_type().is_symlink()
                    || !meta.is_dir()
                    || (cfg!(windows) && windows_reparse_point(&meta))
                {
                    return Err(format!("Unsafe staging directory: {}", cursor.display()));
                }
            } else {
                fs::create_dir(&cursor).map_err(|e| e.to_string())?;
            }
        }
        if entry.is_dir() {
            report.directories += 1;
            continue;
        }
        if !entry.is_file() {
            return Err("Non-file entry rejected".into());
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| e.to_string())?;
        let written = io::copy(&mut entry, &mut out)
            .map_err(|e| format!("ZIP decompression/CRC failure: {e}"))?;
        out.flush().map_err(|e| e.to_string())?;
        if written != entry.size() {
            return Err("ZIP decompressed size mismatch".into());
        }
        report.regular_files += 1;
        report.extracted_bytes = report
            .extracted_bytes
            .checked_add(written)
            .ok_or("Expanded size overflow")?;
    }
    if report.regular_files != expected.regular_files
        || report.extracted_bytes != expected.extracted_bytes
    {
        return Err("Extracted file inventory mismatch".into());
    }
    Ok(report)
}
#[cfg(windows)]
fn windows_reparse_point(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn windows_reparse_point(_: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unsafe_windows_paths() {
        for name in [
            "../escape",
            "C:/root",
            "CON.txt",
            "folder/COM1.bin",
            "a\\b",
            "/etc/passwd",
            "a/../b",
        ] {
            assert!(safe_relative_zip_path(name).is_err(), "{name}");
        }
        assert!(safe_relative_zip_path("Plugins/Test/Source/a.cpp").is_ok());
    }
    #[test]
    fn missing_entries_survive_scan_and_tags() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("Test.zip");
        fs::write(&path, b"test").unwrap();
        let mut c = Catalogue::default();
        assert_eq!(c.scan(&[t.path().display().to_string()]).unwrap(), 1);
        let id = c.entries.keys().next().unwrap().clone();
        c.set_tags(&id, vec!["Survival".into(), "survival".into()])
            .unwrap();
        fs::remove_file(path).unwrap();
        c.scan(&[t.path().display().to_string()]).unwrap();
        assert!(!c.entries[&id].present);
        assert_eq!(c.entries[&id].tags, ["Survival"]);
    }
}

/// An existing user-selected staging root must not itself be a link/reparse point.
pub fn ensure_safe_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() || windows_reparse_point(&meta) {
        return Err(format!("Unsafe staging root: {}", root.display()));
    }
    Ok(())
}

#[derive(Debug)]
pub enum PublishError {
    NoSpace { required: u64, available: u64 },
    Invalid(String),
}
impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSpace {
                required,
                available,
            } => write!(
                f,
                "Insufficient destination space: need {required} bytes, have {available}"
            ),
            Self::Invalid(e) => f.write_str(e),
        }
    }
}
fn publish_problem(e: impl std::fmt::Display) -> PublishError {
    PublishError::Invalid(e.to_string())
}

/// A project detected in verified staging. Nothing in the descriptor or C++ source is rewritten.
#[derive(Clone, Debug)]
pub struct StagedProject {
    pub root: PathBuf,
    pub descriptor_name: String,
    pub original_name: String,
    pub cpp_project: bool,
}

/// Inspect only staged regular files. Detect ambiguous multi-project archives rather
/// than silently publishing the wrong subtree.
pub fn detect_staged_project(staged: &Path) -> Result<Option<StagedProject>, String> {
    fn visit(dir: &Path, found: &mut Vec<PathBuf>, depth: usize) -> Result<(), String> {
        if depth > 16 {
            return Err("Staging tree is too deep".into());
        }
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() || windows_reparse_point(&meta) {
                return Err("Link or reparse point found in staging".into());
            }
            if meta.is_dir() {
                visit(&path, found, depth + 1)?;
            } else if meta.is_file()
                && path
                    .extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("uproject"))
            {
                found.push(path);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    visit(staged, &mut found, 0)?;
    if found.is_empty() {
        return Ok(None);
    }
    if found.len() != 1 {
        return Err(format!(
            "Archive contains {} .uproject files; selection is required",
            found.len()
        ));
    }
    let desc = &found[0];
    let root = desc
        .parent()
        .ok_or("Project descriptor has no parent")?
        .to_path_buf();
    let descriptor_name = desc
        .file_name()
        .ok_or("Missing descriptor filename")?
        .to_string_lossy()
        .into_owned();
    let original_name = desc
        .file_stem()
        .ok_or("Missing project name")?
        .to_string_lossy()
        .into_owned();
    let source = root.join("Source");
    let mut target = false;
    let mut build = false;
    fn scan_build(
        dir: &Path,
        target: &mut bool,
        build: &mut bool,
        depth: u8,
    ) -> Result<(), String> {
        if depth > 12 {
            return Ok(());
        }
        for child in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?;
            let p = child.path();
            let meta = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() || windows_reparse_point(&meta) {
                return Err("Unsafe link in Source directory".into());
            }
            if meta.is_dir() {
                scan_build(&p, target, build, depth + 1)?;
            } else if meta.is_file() {
                let name = child.file_name().to_string_lossy().to_ascii_lowercase();
                if name.ends_with(".target.cs") {
                    *target = true;
                }
                if name.ends_with(".build.cs") {
                    *build = true;
                }
            }
        }
        Ok(())
    }
    // Conservative: any Source tree is potentially a native code project.
    if source.is_dir() {
        scan_build(&source, &mut target, &mut build, 0)?;
    }
    Ok(Some(StagedProject {
        root,
        descriptor_name,
        original_name,
        cpp_project: source.is_dir() || target || build,
    }))
}

/// Only the .uproject filename changes for Blueprint-only projects. Its bytes remain unchanged.
/// The operation runs in private staging and rejects filename collisions.
pub fn rename_blueprint_descriptor(project: &StagedProject, new_name: &str) -> Result<(), String> {
    validate_install_folder_name(new_name)?;
    if project.cpp_project {
        return Ok(());
    }
    let old = project.root.join(&project.descriptor_name);
    let new = project.root.join(format!("{new_name}.uproject"));
    if old == new || (!old.exists() && new.is_file()) {
        return Ok(());
    }
    if new.exists() {
        return Err("Destination .uproject already exists in staging".into());
    }
    fs::rename(old, new).map_err(|e| e.to_string())
}

/// Folder names must be single safe Windows components, not nested paths.
pub fn validate_install_folder_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.ends_with('.')
        || name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err("Invalid project folder name".into());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("Control character in project name".into());
    }
    safe_relative_zip_path(name)?;
    Ok(())
}

pub fn suggested_install_name(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join("_");
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if validate_install_folder_name(&clean).is_ok() {
        clean
    } else {
        "Unreal_Asset".into()
    }
}

/// Copy verified staging files to a NEW directory on the destination volume,
/// then atomically rename the private incoming directory into place.
/// Never modify an existing Unreal project or installation implicitly.
pub fn publish_staged_into_new_directory(
    staged: &Path,
    destination_parent: &Path,
    folder_name: &str,
    bytes: u64,
) -> Result<PathBuf, PublishError> {
    if !staged.is_dir() {
        return Err(publish_problem("Staging directory is missing"));
    }
    let src = fs::symlink_metadata(staged).map_err(publish_problem)?;
    if src.file_type().is_symlink() || windows_reparse_point(&src) {
        return Err(publish_problem("Unsafe staging location"));
    }
    let parent = fs::canonicalize(destination_parent).map_err(publish_problem)?;
    let meta = fs::symlink_metadata(&parent).map_err(publish_problem)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || windows_reparse_point(&meta) {
        return Err(publish_problem("Unsafe destination directory"));
    }
    safe_relative_zip_path(folder_name).map_err(publish_problem)?;
    if folder_name.contains('/') {
        return Err(publish_problem(
            "Destination folder name must be one component",
        ));
    }
    let destination = parent.join(folder_name);
    if destination.exists() {
        return Err(publish_problem(
            "Destination already exists; refusing overwrite",
        ));
    }
    let reserve = 64_u64 * 1024 * 1024;
    let required = bytes
        .checked_add(reserve)
        .ok_or_else(|| publish_problem("Required space overflow"))?;
    let free = fs2::available_space(&parent).map_err(publish_problem)?;
    if free < required {
        return Err(PublishError::NoSpace {
            required,
            available: free,
        });
    }
    let incoming = parent.join(format!(".{folder_name}.incoming"));
    if incoming.exists() {
        return Err(publish_problem(
            "Previous incomplete installation present; refusing overwrite",
        ));
    }
    fs::create_dir(&incoming).map_err(publish_problem)?;
    fn copy_tree(from: &Path, to: &Path) -> Result<(), PublishError> {
        for item in fs::read_dir(from).map_err(publish_problem)? {
            let item = item.map_err(publish_problem)?;
            let meta = fs::symlink_metadata(item.path()).map_err(publish_problem)?;
            if meta.file_type().is_symlink() || windows_reparse_point(&meta) {
                return Err(publish_problem("Link/reparse point encountered in staging"));
            }
            let target = to.join(item.file_name());
            if meta.is_dir() {
                fs::create_dir(&target).map_err(publish_problem)?;
                copy_tree(&item.path(), &target)?;
            } else if meta.is_file() {
                let mut src = fs::File::open(item.path()).map_err(publish_problem)?;
                let mut dst = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target)
                    .map_err(publish_problem)?;
                let transferred = io::copy(&mut src, &mut dst).map_err(publish_problem)?;
                if transferred != meta.len() {
                    return Err(publish_problem("Copied byte count mismatch"));
                }
                dst.sync_all().map_err(publish_problem)?;
            } else {
                return Err(publish_problem("Special file in staging"));
            }
        }
        Ok(())
    }
    // On errors the private .incoming folder is deliberately preserved for diagnosis.
    copy_tree(staged, &incoming)?;
    fs::rename(&incoming, &destination).map_err(publish_problem)?;
    Ok(destination)
}

/// Read a small candidate preview image from an archive without unpacking
/// arbitrary contents. The caller may save the returned PNG/JPEG bytes in its
/// own thumbnail cache. Archive content is never modified.
pub fn preview_image(archive_path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut archive =
        zip::ZipArchive::new(fs::File::open(archive_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    // Prefer product-style artwork. Unreal project archives commonly include many
    // internal texture/material images; those are not suitable card thumbnails.
    let mut preferred: Vec<(u8, usize)> = Vec::new();
    for i in 0..archive.len().min(100_000) {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if !entry.is_file() || entry.is_symlink() || entry.size() > 8 * 1024 * 1024 {
            continue;
        }
        if safe_relative_zip_path(entry.name()).is_err() {
            continue;
        }
        let name = entry.name().replace('\\', "/").to_lowercase();
        if !(name.ends_with(".png")
            || name.ends_with(".jpg")
            || name.ends_with(".jpeg")
            || name.ends_with(".bmp"))
        {
            continue;
        }
        let segments: Vec<&str> = name.split('/').collect();
        let basename = segments.last().copied().unwrap_or("");
        // Never present material maps as storefront artwork. Avoid generic
        // project icons, too: the badge already conveys archive type.
        if segments.iter().any(|part| {
            matches!(
                *part,
                "materials"
                    | "material"
                    | "textures"
                    | "texture"
                    | "maps"
                    | "normalmaps"
                    | "surfaces"
                    | "sprites"
                    | "icons"
            )
        }) || [
            "normal",
            "roughness",
            "metallic",
            "albedo",
            "diffuse",
            "_orm",
            "_rma",
            "_ao",
            "_mask",
        ]
        .iter()
        .any(|word| basename.contains(word))
        {
            continue;
        }
        let at_root = segments.len() <= 2;
        let has = |word: &str| basename.contains(word);
        let rank = if matches!(basename, "splash.png" | "splash.bmp")
            && segments
                .iter()
                .any(|s| *s == "splash" || *s == "windows" || *s == "content")
        {
            0
        } else if has("cover") || has("thumbnail") || has("keyart") || has("key_art") {
            if at_root { 0 } else { 1 }
        } else if has("screenshot") || has("showcase") || has("hero") || has("promo") {
            if at_root { 1 } else { 2 }
        } else if has("preview") && at_root {
            3
        } else if segments
            .iter()
            .any(|s| matches!(*s, "screenshots" | "marketing" | "promotional"))
        {
            4
        } else {
            // Unknown images are more likely to be in-game textures than
            // intentional asset artwork. Show the fallback instead.
            continue;
        };
        preferred.push((rank, i));
    }
    preferred.sort_unstable();
    for (_, i) in preferred.into_iter().take(16) {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("ZIP preview CRC failed: {e}"))?;
        // Prevent storing arbitrary non-image content under an image extension.
        if image::load_from_memory(&bytes).is_ok() {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod catalogue_persistence_tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dnw-zip-catalogue-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn entry(id: &str, verification: Verification) -> Entry {
        Entry {
            id: id.into(),
            path: PathBuf::from(format!("{id}.zip")),
            present: true,
            hidden: false,
            bytes: 100,
            modified_unix_secs: None,
            category: "Asset Pack".into(),
            category_override: Some("Unreal Project".into()),
            tags: vec!["Environment".into()],
            verification,
            full_fingerprint: None,
            quick_fingerprint: None,
            verified_entries: 0,
            uncompressed_bytes: 0,
        }
    }

    #[test]
    fn hidden_flag_defaults_for_old_catalogues_and_roundtrips() {
        let entry = entry("older", Verification::Unchecked);
        let mut json = serde_json::to_value(&entry).expect("serialize entry");
        json.as_object_mut().unwrap().remove("hidden");
        let old: Entry = serde_json::from_value(json).expect("read older catalogue entry");
        assert!(
            !old.hidden,
            "older catalogues must display entries by default"
        );
        let mut hidden = old;
        hidden.hidden = true;
        let restored: Entry =
            serde_json::from_str(&serde_json::to_string(&hidden).unwrap()).unwrap();
        assert!(
            restored.hidden,
            "hide preference must persist across serialization"
        );
    }

    #[test]
    fn interrupted_verification_is_not_reported_as_verified() {
        let path = test_path("interrupted");
        let mut c = Catalogue::default();
        c.entries
            .insert("one".into(), entry("one", Verification::Checking));
        c.save(&path).unwrap();
        let loaded = Catalogue::load(&path).unwrap();
        assert_eq!(loaded.entries["one"].verification, Verification::Unchecked);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn recovers_from_damaged_primary_without_losing_tags() {
        let path = test_path("recovery");
        let mut c = Catalogue::default();
        c.entries
            .insert("one".into(), entry("one", Verification::Valid));
        c.save(&path).unwrap();
        c.save(&path).unwrap(); // ensure last-good backup exists
        fs::write(&path, b"{ truncated").unwrap();
        let recovered = Catalogue::load(&path).unwrap();
        assert_eq!(recovered.entries["one"].tags, vec!["Environment"]);
        assert_eq!(
            recovered.entries["one"].category_override.as_deref(),
            Some("Unreal Project")
        );
        recovered.save(&path).unwrap();
        assert!(Catalogue::read_valid(&path).is_ok());
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.bak"));
    }

    #[test]
    fn refuses_to_replace_unrecoverably_corrupt_catalogue() {
        let path = test_path("fail-closed");
        fs::write(&path, b"not valid json").unwrap();
        let c = Catalogue::default();
        assert!(c.save(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not valid json");
        let _ = fs::remove_file(path);
    }
}
