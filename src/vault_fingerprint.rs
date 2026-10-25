//! Lightweight, non-cryptographic fingerprints for packages in external Vault storage.
//! Neither fingerprint proves payload integrity. SHA-1 checks against trusted manifests do.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use twox_hash::XxHash3_128;

pub const SCHEMA: u32 = 1;
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 2_000_000;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fingerprints {
    pub schema: u32,
    pub quick: String,
    pub full_tree: Option<String>,
    pub observed_at: u64,
}
fn field(h: &mut XxHash3_128, bytes: &[u8]) {
    h.write(&(bytes.len() as u64).to_le_bytes());
    h.write(bytes);
}
fn stamp(path: &Path) -> Option<u128> {
    fs::metadata(path)
        .ok()?
        .created()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}
fn initial(package: &Path) -> Result<XxHash3_128, String> {
    let mut h = XxHash3_128::new();
    field(&mut h, b"DoNothingAndWin:VaultFingerprintV1");
    field(
        &mut h,
        package
            .file_name()
            .ok_or("Package has no folder name")?
            .to_string_lossy()
            .as_bytes(),
    );
    let manifest = package.join("manifest");
    let meta = fs::symlink_metadata(&manifest).map_err(|e| format!("Manifest: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > MAX_MANIFEST {
        return Err("Manifest is not a regular file or exceeds 64 MiB".into());
    }
    match stamp(&manifest) {
        Some(t) => {
            field(&mut h, b"created");
            field(&mut h, &t.to_le_bytes());
        }
        None => field(&mut h, b"creation-time-unavailable"),
    }
    let mut f = fs::File::open(&manifest).map_err(|e| e.to_string())?;
    field(&mut h, &meta.len().to_le_bytes());
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.write(&buf[..n]);
    }
    Ok(h)
}
fn children(path: &Path) -> Result<Vec<(String, PathBuf, fs::Metadata)>, String> {
    let mut entries = Vec::new();
    for item in fs::read_dir(path).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        let meta = fs::symlink_metadata(item.path()).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!("Symlink not supported: {name}"));
        }
        if !meta.is_dir() && !meta.is_file() {
            return Err(format!("Unsupported entry: {name}"));
        }
        entries.push((name, item.path(), meta));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}
fn entry(h: &mut XxHash3_128, relative: &str, meta: &fs::Metadata, details: bool) {
    field(h, if meta.is_dir() { b"D" } else { b"F" });
    field(h, relative.as_bytes());
    if details {
        if meta.is_file() {
            field(h, &meta.len().to_le_bytes());
        }
        let nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        match nanos {
            Some(n) => {
                field(h, b"modified");
                field(h, &n.to_le_bytes());
            }
            None => field(h, b"modified-unavailable"),
        }
    }
}
/// Small manifest read plus just the immediate names under data/. Does not read payloads.
pub fn quick(package: &Path) -> Result<String, String> {
    let mut h = initial(package)?;
    let data = package.join("data");
    if !fs::symlink_metadata(&data)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err("Missing data directory".into());
    }
    let entries = children(&data)?;
    if entries.is_empty() {
        return Err("Data directory is empty".into());
    }
    field(&mut h, b"DATA_ROOT");
    for (name, _, meta) in entries {
        entry(&mut h, &name, &meta, false);
    }
    Ok(format!("{:032x}", h.finish_128()))
}
/// Recursive metadata only; paths, sizes and modification times; no asset file bytes.
pub fn full_tree(package: &Path) -> Result<String, String> {
    let mut h = initial(package)?;
    field(&mut h, b"FULL_TREE");
    let data = package.join("data");
    if !fs::symlink_metadata(&data)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("Missing data directory".into());
    }
    let mut stack = vec![(data, String::new())];
    let mut seen = 0;
    while let Some((dir, prefix)) = stack.pop() {
        let items = children(&dir)?;
        let mut subdirs = Vec::new();
        for (name, path, meta) in items {
            seen += 1;
            if seen > MAX_TREE_ENTRIES {
                return Err("Package exceeds fingerprint entry limit".into());
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            entry(&mut h, &relative, &meta, true);
            if meta.is_dir() {
                subdirs.push((path, relative));
            }
        }
        // Deterministic DFS order.
        stack.extend(subdirs.into_iter().rev());
    }
    if seen == 0 {
        return Err("Data directory is empty".into());
    }
    Ok(format!("{:032x}", h.finish_128()))
}
/// Existing downloader receipts and full SHA-1 validation remain authoritative.
pub fn capture(package: &Path, deep: bool) -> Result<Fingerprints, String> {
    Ok(Fingerprints {
        schema: SCHEMA,
        quick: quick(package)?,
        full_tree: if deep {
            Some(full_tree(package)?)
        } else {
            None
        },
        observed_at: crate::epic::now(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_vs_nested_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("manifest"), b"test manifest").unwrap();
        fs::create_dir_all(root.join("data/sub")).unwrap();
        fs::write(root.join("data/sub/a"), b"one").unwrap();
        let q1 = quick(root).unwrap();
        let d1 = full_tree(root).unwrap();
        fs::write(root.join("data/sub/newfile"), b"two").unwrap();
        assert_eq!(q1, quick(root).unwrap());
        assert_ne!(d1, full_tree(root).unwrap());
        fs::write(root.join("manifest"), b"changed").unwrap();
        assert_ne!(q1, quick(root).unwrap());
    }
}
