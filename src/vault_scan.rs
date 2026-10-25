//! Read-only, bounded inspection of an existing Epic VaultCache.
//! Never changes source files, payloads, or package directories.
use crate::buildpatch;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 100_000;
const MAX_PACKAGES: usize = 10_000;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct VaultPackage {
    pub folder: String,
    pub has_data: bool,
    pub manifest_size: Option<u64>,
    pub app: Option<String>,
    pub build: Option<String>,
    pub file_count: Option<usize>,
    pub chunk_count: Option<usize>,
    pub manifest_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VaultScan {
    pub root: PathBuf,
    pub packages: Vec<VaultPackage>,
    pub entries_seen: usize,
    pub truncated: bool,
    pub warnings: Vec<String>,
    pub manifest_found: usize,
    pub manifest_parsed: usize,
    pub data_folders: usize,
}

fn safe_kind(path: &Path) -> Result<Option<fs::Metadata>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(None);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Ok(None);
        }
    }
    Ok(Some(metadata))
}

fn inspect_manifest(path: &Path, package: &mut VaultPackage) {
    let meta = match safe_kind(path) {
        Ok(Some(meta)) if meta.is_file() => meta,
        Ok(Some(_)) | Ok(None) => {
            package.manifest_error = Some("Manifest is not a regular file".into());
            return;
        }
        Err(e) => {
            package.manifest_error = Some(format!("Cannot stat manifest: {e}"));
            return;
        }
    };
    package.manifest_size = Some(meta.len());
    if meta.len() > MAX_MANIFEST_BYTES {
        package.manifest_error = Some(format!(
            "Manifest exceeds {} MiB inspection limit",
            MAX_MANIFEST_BYTES / (1024 * 1024)
        ));
        return;
    }
    let raw = match fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            package.manifest_error = Some(format!("Cannot read manifest: {e}"));
            return;
        }
    };
    match buildpatch::parse(&raw) {
        Ok(manifest) => {
            package.app = Some(manifest.app);
            package.build = Some(manifest.build);
            package.file_count = Some(manifest.files.len());
            package.chunk_count = Some(manifest.chunks.len());
        }
        Err(e) => package.manifest_error = Some(format!("Unsupported or invalid manifest: {e}")),
    }
}

pub fn scan(root: &Path) -> Result<VaultScan, String> {
    if !root.is_dir() {
        return Err("Vault folder does not exist or is unavailable".into());
    }
    let mut result = VaultScan {
        root: root.to_path_buf(),
        packages: Vec::new(),
        entries_seen: 0,
        truncated: false,
        warnings: Vec::new(),
        manifest_found: 0,
        manifest_parsed: 0,
        data_folders: 0,
    };
    let entries = fs::read_dir(root).map_err(|e| format!("Cannot enumerate vault: {e}"))?;
    for entry in entries {
        if result.entries_seen >= MAX_ENTRIES || result.packages.len() >= MAX_PACKAGES {
            result.truncated = true;
            break;
        }
        result.entries_seen += 1;
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                result.warnings.push(format!("Unreadable root entry: {e}"));
                continue;
            }
        };
        let path = entry.path();
        if !matches!(safe_kind(&path), Ok(Some(m)) if m.is_dir()) {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        if folder.starts_with(".engine-workspace-") {
            continue;
        }
        let mut package = VaultPackage {
            folder: folder.clone(),
            has_data: false,
            manifest_size: None,
            app: None,
            build: None,
            file_count: None,
            chunk_count: None,
            manifest_error: None,
        };
        match fs::read_dir(&path) {
            Ok(children) => {
                for child in children {
                    if result.entries_seen >= MAX_ENTRIES {
                        result.truncated = true;
                        break;
                    }
                    result.entries_seen += 1;
                    let child = match child {
                        Ok(c) => c,
                        Err(e) => {
                            result
                                .warnings
                                .push(format!("{folder}: unreadable entry: {e}"));
                            continue;
                        }
                    };
                    let name = child.file_name().to_string_lossy().to_ascii_lowercase();
                    if name == "manifest" {
                        inspect_manifest(&child.path(), &mut package);
                    } else if name == "data" {
                        package.has_data =
                            matches!(safe_kind(&child.path()), Ok(Some(m)) if m.is_dir());
                    }
                }
            }
            Err(e) => result
                .warnings
                .push(format!("{folder}: cannot enumerate: {e}")),
        }
        if package.manifest_size.is_some() {
            result.manifest_found += 1;
        }
        if package.file_count.is_some() {
            result.manifest_parsed += 1;
        }
        if package.has_data {
            result.data_folders += 1;
        }
        result.packages.push(package);
        if result.truncated {
            break;
        }
    }
    result
        .packages
        .sort_by_key(|p| p.folder.to_ascii_lowercase());
    Ok(result)
}

/// Stage 6: read-only structural verification of reconstructed package files.
/// This checks paths and sizes only. It does NOT cryptographically verify payloads.
#[derive(Clone, Debug, Default)]
pub struct VaultVerify {
    pub examined_packages: usize,
    pub complete_structure: usize,
    pub incomplete: usize,
    pub checked_files: usize,
    pub missing_files: usize,
    pub size_mismatches: usize,
    pub unsafe_paths: usize,
    pub errors: Vec<String>,
}

pub fn verify_structure(root: &Path) -> Result<VaultVerify, String> {
    let inventory = scan(root)?;
    let mut report = VaultVerify::default();
    for pkg in &inventory.packages {
        if pkg.manifest_size.is_none() {
            continue;
        }
        report.examined_packages += 1;
        let directory = root.join(&pkg.folder);
        let manifest_path = directory.join("manifest");
        let raw = match safe_kind(&manifest_path) {
            Ok(Some(meta)) if meta.is_file() && meta.len() <= MAX_MANIFEST_BYTES => {
                match fs::read(&manifest_path) {
                    Ok(raw) => raw,
                    Err(_) => {
                        report.incomplete += 1;
                        if report.errors.len() < 100 {
                            report
                                .errors
                                .push(format!("{}: cannot read manifest", pkg.folder));
                        }
                        continue;
                    }
                }
            }
            _ => {
                report.incomplete += 1;
                if report.errors.len() < 100 {
                    report
                        .errors
                        .push(format!("{}: manifest missing or too large", pkg.folder));
                }
                continue;
            }
        };
        let manifest = match buildpatch::parse(&raw) {
            Ok(manifest) => manifest,
            Err(e) => {
                report.incomplete += 1;
                if report.errors.len() < 100 {
                    report.errors.push(format!("{}: {e}", pkg.folder));
                }
                continue;
            }
        };
        let data = directory.join("data");
        if !matches!(safe_kind(&data), Ok(Some(m)) if m.is_dir()) {
            report.incomplete += 1;
            if report.errors.len() < 100 {
                report
                    .errors
                    .push(format!("{}: missing data directory", pkg.folder));
            }
            continue;
        }
        let mut bad = 0usize;
        for file in &manifest.files {
            report.checked_files += 1;
            let relative = match crate::installer::relative(&file.name) {
                Ok(path) => path,
                Err(_) => {
                    report.unsafe_paths += 1;
                    bad += 1;
                    continue;
                }
            };
            // A valid manifest path alone is not sufficient: prevent following
            // junctions/symlinks in *any* intermediate directory component.
            let mut current = data.clone();
            let mut safe = true;
            for component in relative.components() {
                current.push(component.as_os_str());
                match safe_kind(&current) {
                    Ok(Some(_)) => {}
                    _ => {
                        safe = false;
                        break;
                    }
                }
            }
            if !safe {
                bad += 1;
                report.missing_files += 1;
                continue;
            }
            match safe_kind(&current) {
                Ok(Some(meta)) if meta.is_file() && meta.len() == file.size() => {}
                Ok(Some(meta)) if meta.is_file() => {
                    bad += 1;
                    report.size_mismatches += 1;
                }
                _ => {
                    bad += 1;
                    report.missing_files += 1;
                }
            }
        }
        if bad == 0 {
            report.complete_structure += 1;
        } else {
            report.incomplete += 1;
            if report.errors.len() < 100 {
                report.errors.push(format!(
                    "{}: {bad} missing, unsafe, or size-mismatched files",
                    pkg.folder
                ));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_path_is_error() {
        assert!(scan(Path::new("/vault-scan-definitely-missing-012ab")).is_err());
    }
    #[test]
    fn detects_extensionless_manifest_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("ExampleV1");
        fs::create_dir(&pkg).unwrap();
        fs::create_dir(pkg.join("data")).unwrap();
        fs::write(pkg.join("manifest"), b"not a valid manifest").unwrap();
        let result = scan(tmp.path()).unwrap();
        assert_eq!(result.manifest_found, 1);
        assert_eq!(result.data_folders, 1);
        assert_eq!(result.manifest_parsed, 0);
        assert!(result.packages[0].manifest_error.is_some());
        assert_eq!(
            fs::read(pkg.join("manifest")).unwrap(),
            b"not a valid manifest"
        );
    }
}

/// Verify exactly one cached package, using the Stage 6 read-only file inventory checks.
/// This is structural verification (presence, size and safe paths), not SHA-1 hashing.
pub fn verify_package(root: &Path, folder: &str) -> Result<VaultVerify, String> {
    if folder.is_empty()
        || folder == "."
        || folder == ".."
        || (folder.contains('/') || folder.contains('\\'))
    {
        return Err("Invalid Vault package folder".into());
    }
    let target = root.join(folder);
    if !matches!(safe_kind(&target), Ok(Some(m)) if m.is_dir()) {
        return Err("Vault package folder is unavailable".into());
    }
    let mut report = VaultVerify::default();
    let manifest_path = target.join("manifest");
    if !matches!(safe_kind(&manifest_path), Ok(Some(m)) if m.is_file()) {
        return Err("Package manifest is not a regular file".into());
    }
    let raw = fs::read(manifest_path).map_err(|e| format!("Cannot read package manifest: {e}"))?;
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("Manifest exceeds safe limit".into());
    }
    let manifest = buildpatch::parse(&raw).map_err(|e| format!("Invalid package manifest: {e}"))?;
    report.examined_packages = 1;
    let data = target.join("data");
    if !matches!(safe_kind(&data), Ok(Some(m)) if m.is_dir()) {
        return Err("Package data folder missing".into());
    }
    for file in &manifest.files {
        report.checked_files += 1;
        let relative = crate::installer::relative(&file.name)
            .map_err(|e| format!("Unsafe manifest filename: {e}"))?;
        let mut path = data.clone();
        let mut safe = true;
        for component in relative.components() {
            path.push(component.as_os_str());
            if !matches!(safe_kind(&path), Ok(Some(_))) {
                safe = false;
                break;
            }
        }
        if !safe {
            report.missing_files += 1;
            continue;
        }
        match safe_kind(&path) {
            Ok(Some(meta)) if meta.is_file() && meta.len() == file.size() => {}
            Ok(Some(meta)) if meta.is_file() => report.size_mismatches += 1,
            _ => report.missing_files += 1,
        }
    }
    if report.missing_files == 0 && report.size_mismatches == 0 && report.unsafe_paths == 0 {
        report.complete_structure = 1;
    } else {
        report.incomplete = 1;
    }
    Ok(report)
}

/// Full, read-only integrity verification of one cached package. Unlike the
/// Stage 6 scanner, this streams and checks every file's manifest SHA-1 digest.
/// Running this against network storage can take considerable time.
pub fn verify_package_integrity(root: &Path, folder: &str) -> Result<VaultVerify, String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;

    let report = verify_package(root, folder)?;
    if report.complete_structure != 1 {
        return Ok(report);
    }
    let directory = root.join(folder);
    let raw = fs::read(directory.join("manifest"))
        .map_err(|e| format!("Cannot read manifest for hashing: {e}"))?;
    let manifest = buildpatch::parse(&raw)?;
    let data = directory.join("data");
    for file in &manifest.files {
        if file.sha == [0u8; 20] {
            return Err(format!("Manifest lacks a SHA-1 digest for {}", file.name));
        }
        let relative = crate::installer::relative(&file.name)?;
        let mut path = data.clone();
        for component in relative.components() {
            path.push(component.as_os_str());
            if !matches!(safe_kind(&path), Ok(Some(_))) {
                return Err(format!(
                    "File became inaccessible during verification: {}",
                    file.name
                ));
            }
        }
        let mut stream =
            fs::File::open(&path).map_err(|e| format!("Cannot hash {}: {e}", file.name))?;
        let mut sha = Sha1::new();
        let mut buffer = [0u8; 256 * 1024];
        loop {
            let n = stream
                .read(&mut buffer)
                .map_err(|e| format!("Cannot read {} while hashing: {e}", file.name))?;
            if n == 0 {
                break;
            }
            sha.update(&buffer[..n]);
        }
        let actual: [u8; 20] = sha.finalize().into();
        if actual != file.sha {
            return Err(format!("SHA-1 verification failed for {}", file.name));
        }
    }
    Ok(report)
}
