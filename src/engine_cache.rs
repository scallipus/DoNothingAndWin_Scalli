//! Optional, exact-build engine payload retention in the standard Vault root.
//! Completed caches are immutable; incomplete caches retain verified chunks for repair.
use crate::buildpatch::{self, Manifest};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct CacheReceipt {
    app: String,
    build: String,
    selected_tags: Vec<String>,
    chunks: usize,
    #[serde(default)]
    required_chunks: Vec<String>,
}

pub fn build_folder(vault_root: &Path, manifest: &Manifest, tags: &[String]) -> PathBuf {
    let mut tags = tags.to_vec();
    tags.sort();
    tags.dedup();
    let key = format!("{}\n{}\n{}", manifest.app, manifest.build, tags.join("\n"));
    let digest = buildpatch::hex(&buildpatch::digest(key.as_bytes()));
    vault_root
        .join(".EngineCache")
        .join(format!("{}-{}", manifest.app, &digest[..24]))
}

fn receipt(manifest: &Manifest, tags: &[String]) -> CacheReceipt {
    let mut selected_tags = tags.to_vec();
    selected_tags.sort();
    selected_tags.dedup();
    CacheReceipt {
        app: manifest.app.clone(),
        build: manifest.build.clone(),
        selected_tags,
        chunks: manifest.chunks.len(),
        required_chunks: required_chunks(manifest, tags),
    }
}

fn required_chunks(manifest: &Manifest, tags: &[String]) -> Vec<String> {
    let selected: std::collections::HashSet<&str> = tags.iter().map(String::as_str).collect();
    let mut ids: Vec<_> = manifest
        .files
        .iter()
        .filter(|f| f.tags.is_empty() || f.tags.iter().any(|tag| selected.contains(tag.as_str())))
        .flat_map(|f| f.parts.iter().map(|part| part.guid.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// A completed cache is usable only if its exact receipt and every required
/// chunk agree with the authenticated manifest. Never trust complete.json alone.
pub fn verify_complete(folder: &Path, manifest: &Manifest, tags: &[String]) -> Result<(), String> {
    crate::installer::no_links(folder)?;
    let expected = receipt(manifest, tags);
    let bytes = fs::read(folder.join("complete.json"))
        .map_err(|e| format!("Cache is not complete: {e}"))?;
    let stored: CacheReceipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if stored.app != expected.app
        || stored.build != expected.build
        || stored.selected_tags != expected.selected_tags
        || stored.required_chunks != expected.required_chunks
    {
        return Err("Cached engine receipt does not match the requested build/components".into());
    }
    let manifest_bytes = fs::read(folder.join("manifest.json")).map_err(|e| e.to_string())?;
    let saved: Manifest = serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
    if saved.app != manifest.app
        || saved.build != manifest.build
        || saved.feature != manifest.feature
    {
        return Err("Cached engine manifest does not match the requested artifact".into());
    }
    for id in &expected.required_chunks {
        let chunk = manifest
            .chunks
            .get(id)
            .ok_or("Missing chunk in requested manifest")?;
        let saved_chunk = saved
            .chunks
            .get(id)
            .ok_or("Missing chunk in cached manifest")?;
        if chunk.guid != saved_chunk.guid
            || chunk.sha != saved_chunk.sha
            || chunk.window != saved_chunk.window
            || chunk.rolling != saved_chunk.rolling
        {
            return Err(format!("Cached manifest chunk differs: {id}"));
        }
        let file = folder.join("chunks").join(id);
        crate::installer::no_links(&file)?;
        if !crate::installer::valid_cached_chunk(&file, chunk) {
            return Err(format!("Cached engine chunk missing or damaged: {id}"));
        }
    }
    Ok(())
}

/// Explicitly remove only one managed, exact build. Caller must confirm the path
/// and ensure no active job uses it. Installed engines are never touched.
pub fn delete_build(folder: &Path, vault_root: &Path) -> Result<(), String> {
    let parent = vault_root.join(".EngineCache");
    if folder.parent() != Some(parent.as_path()) || !folder.is_dir() {
        return Err("Refusing deletion outside the selected engine cache".into());
    }
    crate::installer::no_links(folder)?;
    if !folder.join("complete.json").is_file() || !folder.join("manifest.json").is_file() {
        return Err("Refusing to delete an unrecognized or incomplete cached build".into());
    }
    fs::remove_dir_all(folder).map_err(|e| format!("Cannot remove cached build: {e}"))
}

pub fn check_vacant_or_incomplete(
    folder: &Path,
    manifest: &Manifest,
    tags: &[String],
) -> Result<(), String> {
    if folder.join("complete.json").exists() {
        return Err(format!(
            "Exact engine build already cached at {}",
            folder.display()
        ));
    }
    // Reject links/junctions on cache root and its parents, including partial downloads.
    crate::installer::no_links(folder)?;
    let metadata = folder.join("receipt.json");
    if metadata.exists() {
        let bytes = fs::read(&metadata).map_err(|e| e.to_string())?;
        let old: CacheReceipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let expected = receipt(manifest, tags);
        if old.app != expected.app
            || old.build != expected.build
            || old.selected_tags != expected.selected_tags
        {
            return Err(
                "Existing engine cache belongs to another build or component selection".into(),
            );
        }
    } else {
        fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        crate::storage::write_json(&metadata, &receipt(manifest, tags))?;
    }
    Ok(())
}

pub fn mark_complete(folder: &Path, manifest: &Manifest, tags: &[String]) -> Result<(), String> {
    crate::installer::no_links(folder)?;
    for id in required_chunks(manifest, tags) {
        let chunk = manifest
            .chunks
            .get(&id)
            .ok_or("Missing required engine chunk")?;
        if !crate::installer::valid_cached_chunk(&folder.join("chunks").join(&id), chunk) {
            return Err(format!(
                "Cannot mark engine cache complete: chunk {id} failed verification"
            ));
        }
    }
    // Store the complete manifest for exact artifact identification and future validation.
    crate::storage::write_json(&folder.join("manifest.json"), manifest)?;
    crate::storage::write_json(&folder.join("complete.json"), &receipt(manifest, tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_component_sets_and_blocks_completed_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let m = Manifest {
            app: "UE_5.7".into(),
            build: "5.7.4-test".into(),
            ..Default::default()
        };
        let a = build_folder(tmp.path(), &m, &["editor_symbols".into()]);
        let b = build_folder(tmp.path(), &m, &[]);
        assert_ne!(a, b);
        check_vacant_or_incomplete(&a, &m, &["editor_symbols".into()]).unwrap();
        mark_complete(&a, &m, &["editor_symbols".into()]).unwrap();
        assert!(
            check_vacant_or_incomplete(&a, &m, &["editor_symbols".into()])
                .unwrap_err()
                .contains("already cached")
        );
    }
}
