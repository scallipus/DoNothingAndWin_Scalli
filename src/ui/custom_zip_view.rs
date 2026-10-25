//! Custom ZIP Library tab. All archive work is done off the UI thread.
use super::*;
use engine_workspace::custom_zip_cache::{self as zipcache, Catalogue, LinkPolicy, Verification};
use engine_workspace::library_organization::FolderFilter;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::mpsc,
    thread,
};

enum Done {
    Scan(Result<Catalogue, String>),
    Verify(
        String,
        Result<zipcache::ArchiveCheck, zipcache::VerifyError>,
    ),
    Extract(
        String,
        Result<(PathBuf, zipcache::ExtractionReport), String>,
    ),
    Install(Result<PathBuf, zipcache::PublishError>),
}
#[derive(Default)]
pub(super) struct ZipAssets {
    active: bool,
    loaded: bool,
    catalogue: Catalogue,
    rx: Option<mpsc::Receiver<Done>>,
    busy: bool,
    verify_queue: VecDeque<(String, bool)>,
    active_verification: Option<String>,
    active_verification_prior: Option<Verification>,
    search: String,
    tag_filter: String,
    notice: String,
    remove: Option<String>,
    classify: Option<String>,
    classification_draft: Option<String>,
    decision: Option<(String, Vec<String>)>,
    pending: Option<(String, PathBuf, zipcache::ExtractionReport)>,
    destination: String,
    setup_id: Option<String>,
    install_as_asset: bool,
    target_project: String,
    known_projects: Vec<(String, String)>,
    project_folder_name: String,
    staged_project: Option<zipcache::StagedProject>,
    space_warning: Option<(u64, u64)>,
    installing: bool,
    install_rx: Option<mpsc::Receiver<Done>>,
    thumbnails: HashMap<String, egui::TextureHandle>,
    list_view: bool,
    show_hidden: bool,
    folder_filter: FolderFilter,
    manage_folders: bool,
    new_folder_name: String,
    folder_error: String,
    session_log: Option<PathBuf>,
    show_validation_log: bool,
    cache_roots: Vec<String>,
    thumbnail_update: Option<String>,
}
impl ZipAssets {
    pub(super) fn has_active_activity(&self) -> bool {
        self.busy
            || self.installing
            || self.active_verification.is_some()
            || !self.verify_queue.is_empty()
    }
    pub(super) fn activity_description(&self) -> String {
        if self.installing {
            "Publishing staged asset files".to_string()
        } else if let Some(id) = &self.active_verification {
            let name = self
                .catalogue
                .entries
                .get(id)
                .and_then(|e| e.path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.clone());
            format!("Verifying {name} · +{} in queue", self.verify_queue.len())
        } else if self.busy {
            "Scanning, verifying or extracting a ZIP archive".to_string()
        } else if let Some((_, path, _)) = &self.pending {
            format!(
                "Staged extraction awaiting installation: {}",
                path.display()
            )
        } else if self.notice.is_empty() {
            "Idle".to_string()
        } else {
            self.notice.clone()
        }
    }
    fn catalogue_path() -> PathBuf {
        storage::data_dir().join("custom-zip-library.json")
    }
    fn thumbnail_path(id: &str) -> PathBuf {
        storage::data_dir()
            .join("zip-thumbnails")
            .join(format!("{id}.img"))
    }
    fn manual_thumbnail_path(id: &str) -> PathBuf {
        storage::data_dir()
            .join("zip-thumbnails")
            .join(format!("{id}.user.img"))
    }
    fn thumb(&mut self, ui: &egui::Ui, id: &str) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.thumbnails.get(id) {
            return Some(texture.clone());
        }
        let bytes = std::fs::read(Self::manual_thumbnail_path(id))
            .or_else(|_| std::fs::read(Self::thumbnail_path(id)))
            .ok()?;
        let decoded = image::load_from_memory(&bytes)
            .ok()?
            .thumbnail(192, 128)
            .to_rgba8();
        let (w, h) = decoded.dimensions();
        let pixels = decoded.into_raw();
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        let texture = ui.ctx().load_texture(
            format!("zip-thumb-{id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.thumbnails.insert(id.to_owned(), texture.clone());
        Some(texture)
    }
    fn save(&mut self) {
        if let Err(e) = self.catalogue.save(&Self::catalogue_path()) {
            self.notice = format!("Failed to save ZIP Library: {e}");
        }
    }
    pub(super) fn toolbar(&mut self, ui: &mut Ui, c: &Canvas, _roots: &[String]) -> bool {
        // Dedicated Library source selector; the Fab filter row is placed below it.
        let area = c.rect([271., 104., 280., 36.]);
        ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active, false, "Fab Assets");
                ui.selectable_value(&mut self.active, true, "ZIP Assets");
            });
        });
        self.active
    }
    fn scan(&mut self, roots: &[String]) {
        if self.busy {
            return;
        }
        self.busy = true;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let roots = roots.to_vec();
        let path = Self::catalogue_path();
        thread::spawn(move || {
            let result = (|| {
                let mut c = Catalogue::load(&path)?;
                c.scan(&roots)?;
                Ok(c)
            })();
            let _ = tx.send(Done::Scan(result));
        });
    }
    fn verify(&mut self, id: String, force_full: bool) {
        if !self
            .catalogue
            .entries
            .get(&id)
            .is_some_and(|entry| entry.present)
        {
            return;
        }
        if self.active_verification.as_deref() == Some(id.as_str())
            || self.verify_queue.iter().any(|queued| queued.0 == id)
        {
            return;
        }
        self.verify_queue.push_back((id, force_full));
        self.start_next_verification();
    }
    fn start_next_verification(&mut self) {
        if self.busy || self.installing {
            return;
        }
        let Some((id, force_full)) = self.verify_queue.pop_front() else {
            return;
        };
        self.busy = true;
        self.active_verification = Some(id.clone());
        if let Some(e) = self.catalogue.entries.get_mut(&id) {
            self.active_verification_prior = Some(e.verification.clone());
            e.verification = Verification::Checking;
        }
        let Some(archive) = self.catalogue.entries.get(&id).map(|e| e.path.clone()) else {
            self.busy = false;
            self.active_verification = None;
            return;
        };
        let known = self.catalogue.entries.get(&id).and_then(|entry| {
            if !force_full
                && (entry.verification == Verification::Valid
                    || entry.verification == Verification::Unchecked)
            {
                entry.full_fingerprint.clone().map(|hash| {
                    (
                        hash,
                        entry.quick_fingerprint.clone(),
                        entry.category.clone(),
                        entry.verified_entries,
                        entry.uncompressed_bytes,
                    )
                })
            } else {
                None
            }
        });
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            // Work is strictly read-only; only the Library manager persists results.
            let result = (|| {
                if let Some((previous, quick_prior, category, entries, expanded_bytes)) = known {
                    let structural = zipcache::structural_fingerprint(&archive)?;
                    let fresh = if quick_prior.as_deref() == Some(structural.as_str()) {
                        previous.clone()
                    } else {
                        zipcache::full_file_fingerprint(&archive)?
                    };
                    if fresh == previous {
                        let meta = std::fs::metadata(&archive)
                            .map_err(|e| zipcache::VerifyError::Io(e.to_string()))?;
                        let time = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs());
                        return Ok(zipcache::ArchiveCheck {
                            fingerprint: fresh,
                            entries,
                            expanded_bytes,
                            category,
                            source_bytes: meta.len(),
                            source_modified_unix_secs: time,
                        });
                    }
                }
                zipcache::check_archive(&archive)
            })();
            let _ = tx.send(Done::Verify(id, result));
        });
    }
    fn write_manual_validation_log(&mut self, id: &str, outcome: &str, detail: &str) {
        let Some(entry) = self.catalogue.entries.get(id) else {
            return;
        };
        let root = self
            .cache_roots
            .iter()
            .filter(|root| entry.path.starts_with(PathBuf::from(root)))
            .max_by_key(|root| root.len())
            .map(PathBuf::from)
            .unwrap_or_else(|| entry.path.parent().unwrap_or(&entry.path).to_path_buf());
        match validation_session::append(
            "Custom ZIP Library",
            &root,
            &entry.path,
            "CRC or whole-file XXH3-128",
            outcome,
            detail,
        ) {
            Ok(path) => self.session_log = Some(path),
            Err(err) => self.notice = format!("Manual validation log write failed: {err}"),
        }
    }
    fn extract(&mut self, id: String, staging_root: &str) {
        if let Some(entry) = self.catalogue.entries.get(&id) {
            if let Some(override_kind) = &entry.category_override {
                if (override_kind == "Unreal Project" || override_kind == "Unreal Plugin")
                    && override_kind != &entry.category
                {
                    self.notice = format!(
                        "Cannot install as {override_kind}: archive detection reports {}. Override only changes display until a compatible descriptor is found.",
                        entry.category
                    );
                    return;
                }
            }
        }
        if self.busy {
            return;
        }
        let Some(entry) = self.catalogue.entries.get(&id) else {
            return;
        };
        let archive = entry.path.clone();
        let preflight = match zipcache::inspect_extraction(&archive) {
            Ok(v) => v,
            Err(e) => {
                self.notice = e;
                return;
            }
        };
        if !preflight.skipped_links.is_empty() {
            self.decision = Some((id, preflight.skipped_links));
            return;
        }
        self.start_extraction(id, archive, staging_root.to_owned(), LinkPolicy::Stop);
    }
    fn start_extraction(
        &mut self,
        id: String,
        archive: PathBuf,
        staging_root: String,
        policy: LinkPolicy,
    ) {
        self.busy = true;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            let result = (|| {
                let root = PathBuf::from(staging_root);
                zipcache::ensure_safe_root(&root)?;
                let directory = root.join(format!("zip-{}", &id));
                if directory.exists() {
                    return Err(format!(
                        "Staging exists; inspect/remove manually: {}",
                        directory.display()
                    ));
                }
                let report = zipcache::extract_into_new_staging(&archive, &directory, policy)?;
                Ok((directory, report))
            })();
            let _ = tx.send(Done::Extract(id, result));
        });
    }
    fn poll(&mut self) {
        let event = self.rx.as_ref().and_then(|r| r.try_recv().ok());
        if let Some(event) = event {
            self.busy = false;
            self.rx = None;
            self.active_verification = None;
            let verification_prior = self.active_verification_prior.take();
            match event {
                Done::Scan(Ok(c)) => {
                    self.catalogue = c;
                    self.notice = "ZIP Library scan completed".into();
                    self.save();
                }
                Done::Scan(Err(e)) => self.notice = e,
                Done::Verify(id, Ok(check)) => {
                    // The UI Library manager is the sole owner of catalogue state.
                    let mut accepted = false;
                    if let Some(record) = self.catalogue.entries.get_mut(&id) {
                        let still_same = std::fs::metadata(&record.path).ok().is_some_and(|meta| {
                            let modified = meta
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs());
                            meta.len() == check.source_bytes
                                && modified == check.source_modified_unix_secs
                        });
                        if still_same {
                            accepted = true;
                            record.verification = Verification::Valid;
                            record.full_fingerprint = Some(check.fingerprint);
                            record.quick_fingerprint =
                                zipcache::structural_fingerprint(&record.path).ok();
                            record.verified_entries = check.entries;
                            record.uncompressed_bytes = check.expanded_bytes;
                            record.category = check.category;
                            self.notice.clear();
                        } else {
                            record.verification = Verification::Unchecked;
                            self.notice =
                                "ZIP changed during verification; result discarded".into();
                        }
                    }
                    if accepted {
                        self.write_manual_validation_log(
                            &id,
                            "VALID",
                            "ZIP CRC / stored XXH3-128 fingerprint verified",
                        );
                    } else {
                        self.write_manual_validation_log(
                            &id,
                            "STALE / NOT VERIFIED",
                            "File changed while being inspected; stale result discarded",
                        );
                    }
                    self.save();
                }
                Done::Verify(id, Err(e)) => {
                    if let Some(record) = self.catalogue.entries.get_mut(&id) {
                        record.verification = match &e {
                            zipcache::VerifyError::Invalid(reason) => {
                                Verification::Invalid(reason.clone())
                            }
                            zipcache::VerifyError::Io(_) => verification_prior
                                .clone()
                                .filter(|prior| *prior != Verification::Checking)
                                .unwrap_or(Verification::Unchecked),
                        };
                        if matches!(e, zipcache::VerifyError::Invalid(_)) {
                            record.full_fingerprint = None;
                            record.quick_fingerprint = None;
                        }
                    }
                    self.notice = format!(
                        "ZIP verification {}: {e}",
                        if matches!(e, zipcache::VerifyError::Invalid(_)) {
                            "invalid"
                        } else {
                            "filesystem error"
                        }
                    );
                    self.show_validation_log = true;
                    self.write_manual_validation_log(
                        &id,
                        if matches!(e, zipcache::VerifyError::Invalid(_)) {
                            "INVALID"
                        } else {
                            "FILESYSTEM ERROR"
                        },
                        &e.to_string(),
                    );
                    self.save();
                }
                Done::Extract(id, Ok((folder, report))) => {
                    Self::record_history(
                        "extracted",
                        &format!(
                            "{} regular files; {} links skipped; staging {}",
                            report.regular_files,
                            report.skipped_links.len(),
                            folder.display()
                        ),
                    );
                    let detected = match zipcache::detect_staged_project(&folder) {
                        Ok(project) => project,
                        Err(err) => {
                            self.notice = format!("Staged project inspection failed: {err}");
                            return;
                        }
                    };
                    let suggestion = detected
                        .as_ref()
                        .map(|p| p.original_name.as_str())
                        .or_else(|| {
                            self.catalogue
                                .entries
                                .get(&id)
                                .and_then(|e| e.path.file_stem().and_then(|n| n.to_str()))
                        })
                        .unwrap_or("Unreal_Asset");
                    if self.project_folder_name.trim().is_empty() {
                        self.project_folder_name = zipcache::suggested_install_name(suggestion);
                    }
                    self.staged_project = detected;
                    self.pending = Some((id, folder, report.clone()));
                    if report.skipped_links.is_empty() {
                        self.try_install();
                    } else {
                        self.notice =
                            "ZIP extracted. Review links or choose an installation destination"
                                .into();
                    }
                }
                Done::Extract(_, Err(e)) => {
                    self.notice = format!("Extraction failed. Target unchanged. {e}");
                    Self::record_history("extraction_failed", &self.notice);
                }
                Done::Install(_) => unreachable!("Install results use separate receiver"),
            }
        }
        self.start_next_verification();
    }
    pub(super) fn page(
        &mut self,
        ui: &mut Ui,
        c: &Canvas,
        roots: &[String],
        staging_root: &str,
        tags_registry: &mut asset_tags::Registry,
        chosen_size: u16,
        known_projects: &[(String, String)],
    ) {
        self.known_projects = known_projects.to_vec();
        self.cache_roots = roots.to_vec();
        if !self.loaded {
            self.loaded = true;
            match Catalogue::load(&Self::catalogue_path()) {
                Ok(c) => self.catalogue = c,
                Err(e) => self.notice = e,
            }
            self.scan(roots);
        }
        self.poll();
        self.poll_install();
        // Import the legacy ZIP-only assignments once; do not overwrite shared tags.
        for (id, entry) in &self.catalogue.entries {
            tags_registry.import_zip(id, &entry.tags);
        }
        let rect = c.rect([271., 150., c.width - 297., c.height - 205.]);
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading("Custom ZIP Assets");
                    if ui.button("Validation Logs").clicked() {
                        let folder = validation_session::directory();
                        let _ = std::fs::create_dir_all(&folder);
                        #[cfg(windows)] { let _ = std::process::Command::new("explorer.exe").arg(folder).spawn(); }
                        #[cfg(not(windows))] { self.notice = format!("Validation logs: {}", folder.display()); }
                    }
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Scan folders"))
                        .clicked()
                    {
                        self.scan(roots);
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
                ui.label(format!(
                    "{} configured folders · {} indexed ZIPs",
                    roots.len(),
                    self.catalogue.entries.len()
                ));
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.text_edit_singleline(&mut self.search);
                    ui.label("Tag filter");
                    let tags = tags_registry.all_tags();
                    egui::ComboBox::from_id_salt("zip-tag-filter")
                        .selected_text(if self.tag_filter.is_empty() {
                            "All tags"
                        } else {
                            &self.tag_filter
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tag_filter, String::new(), "All tags");
                            for tag in tags {
                                ui.selectable_value(&mut self.tag_filter, tag.clone(), tag);
                            }
                        });
                });
                if !self.notice.is_empty() {
                    ui.label(&self.notice);
                }
                ui.horizontal(|ui| {
                    ui.label("Install destination parent");
                    ui.text_edit_singleline(&mut self.destination);
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.destination = path.to_string_lossy().into_owned();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Folder");
                    let selected = match &self.folder_filter {
                        FolderFilter::All => "All items".to_owned(),
                        FolderFilter::Unfiled => "Unfiled".to_owned(),
                        FolderFilter::Folder(name) => name.clone(),
                    };
                    egui::ComboBox::from_id_salt("zip-virtual-folder-filter")
                        // All + Unfiled + roughly five folders before scrolling.
                        .height(260.0_f32)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.folder_filter, FolderFilter::All, "All items");
                            ui.selectable_value(&mut self.folder_filter, FolderFilter::Unfiled, "Unfiled");
                            for folder in self.catalogue.organization.folders() {
                                ui.selectable_value(&mut self.folder_filter, FolderFilter::Folder(folder.clone()), folder);
                            }
                        });
                    if ui.button("Manage folders").clicked() {
                        self.manage_folders = true;
                        self.folder_error.clear();
                    }
                });
                ui.horizontal(|ui| {
                    let count = self.catalogue.entries.values().filter(|e| e.hidden).count();
                    ui.checkbox(&mut self.show_hidden, format!("Show hidden ({count})"));
                    // Same vector Grid/List buttons as the Fab library, on the far right.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        library_view_toolbar::draw(ui, c, &mut self.list_view);
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("zip-assets-list")
                    .show(ui, |ui| {
                        let ids: Vec<_> = self
                            .catalogue
                            .entries
                            .iter()
                            .filter(|(id, e)| {
                                (self.show_hidden || !e.hidden)
                                    && match &self.folder_filter {
                                        FolderFilter::All => true,
                                        FolderFilter::Unfiled => self.catalogue.organization.folder(id).is_none(),
                                        FolderFilter::Folder(folder) => self.catalogue.organization.folder(id) == Some(folder.as_str()),
                                    }
                                    && (self.search.is_empty()
                                    || e.path
                                        .to_string_lossy()
                                        .to_lowercase()
                                        .contains(&self.search.to_lowercase()))
                                    && (self.tag_filter.is_empty()
                                        || tags_registry
                                            .get(&format!("zip:{id}"))
                                            .iter()
                                            .any(|t| t.eq_ignore_ascii_case(&self.tag_filter)))
                            })
                            .map(|(id, _)| id.clone())
                            .collect();
                        let grid = !self.list_view;
                        // Reuse the setting used by normal Fab Library cards.
                        // 0 = original proportional card, otherwise the user selects
                        // square thumbnail dimensions (64/128/192/256).
                        let desired_width = library_card::preferred_card_width(chosen_size, LIBRARY_CARD_WIDTH);
                        let (columns, card_width) = if grid {
                            card_grid_metrics(ui.available_width() - scrollbar::GRID_ALLOCATED_WIDTH
                                - 2.0 * scrollbar::GRID_MARGIN - PROJECT_CARD_GAP,
                                desired_width, PROJECT_CARD_GAP)
                        } else { (1, ui.available_width()) };
                        for row in ids.chunks(columns) {
                          ui.horizontal(|ui| {
                           for id in row {
                            let id = id.clone();
                            let Some(e) = self.catalogue.entries.get(&id).cloned() else {
                                continue;
                            };
                            let name = e
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            let status = if !e.present {
                                "Missing"
                            } else {
                                match &e.verification {
                                    Verification::Unchecked => "Present",
                                    Verification::Checking => "Checking",
                                    Verification::Valid => "CRC validated",
                                    Verification::Invalid(_) => "CRC failed",
                                }
                            };
                            let badge_state = if !e.present {
                                asset_badges::BadgeState::Missing
                            } else {
                                match &e.verification {
                                    Verification::Unchecked => asset_badges::BadgeState::Present,
                                    Verification::Checking => asset_badges::BadgeState::Checking,
                                    Verification::Valid => asset_badges::BadgeState::Verified,
                                    Verification::Invalid(_) => asset_badges::BadgeState::Failed,
                                }
                            };
                            let category = e.category_override.as_deref().unwrap_or(&e.category).to_string();
                            let mut verify = false;
                            let mut force_full = false;
                            let mut extract = false;
                            let mut remove = false;
                            let mut change_type = false;
                            let mut update_thumbnail = false;

                            let mut toggle_hidden = false;
                            let mut assign_folder: Option<Option<String>> = None;
                            let thumbnail = self.thumb(ui, &id);
                            ui.group(|ui| {
                                let width = if grid { card_width } else { ui.available_width() };
                                ui.set_min_width(width);
                                ui.set_max_width(width);
                                ui.vertical(|ui| {
                                    // All grid cards reserve the same thumbnail slot, sized by
                                    // the shared Fab Library setting. The actual image fits
                                    // inside that slot without stretching or changing the footer
                                    // position (including landscape/portrait artwork).
                                    let slot_size = if grid {
                                        library_card::grid_thumbnail_viewport(
                                            chosen_size, card_width, 16.0_f32, LIBRARY_THUMBNAIL_ASPECT,
                                        )
                                    } else {
                                        egui::vec2(86.0, 58.0)
                                    };
                                    let draw_thumbnail = |ui: &mut egui::Ui| {
                                        let (rect, response) = ui.allocate_exact_size(slot_size, egui::Sense::click());
                                        if let Some(texture) = thumbnail.as_ref() {
                                            library_card::paint_contained_thumbnail(
                                                ui, texture, rect, egui::CornerRadius::same(5),
                                                ui.visuals().faint_bg_color,
                                            );
                                        } else {
                                            ui.painter().rect_filled(
                                                rect, egui::CornerRadius::same(5),
                                                ui.visuals().faint_bg_color,
                                            );
                                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                                "ZIP archive", egui::FontId::proportional(13.0_f32),
                                                ui.visuals().weak_text_color());
                                        }
                                        response
                                    };
                                    let mut menu = |ui: &mut egui::Ui,
                                                verify: &mut bool,
                                                change_type: &mut bool,
                                                update_thumbnail: &mut bool,
                                                toggle_hidden: &mut bool,
                                                remove: &mut bool| {
                                        if let Some(action) = super::library_asset_menu::common_actions(ui, e.present, e.hidden) {
                                            match action {
                                                super::library_asset_menu::CommonAction::VerifyIntegrity => *verify = true,
                                                super::library_asset_menu::CommonAction::EditTags => tags_registry.edit(format!("zip:{id}")),
                                                super::library_asset_menu::CommonAction::ToggleHidden => *toggle_hidden = true,
                                            }
                                        }
                                        if e.present && ui.button("Force Full CRC Verification").clicked() {
                                            *verify = true;
                                            force_full = true;
                                            ui.close();
                                        }
                                        if ui.button("Change Asset Type…").clicked() {
                                            *change_type = true;
                                            ui.close();
                                        }
                                        if ui.button("Update Thumbnail…").clicked() {
                                            *update_thumbnail = true;
                                            ui.close();
                                        }
                                        ui.separator();
                                        if !e.present && ui.button("Remove from Library…").clicked() {
                                            *remove = true;
                                            ui.close();
                                        }
                                    };
                                    if grid {
                                        ui.horizontal(|ui| {
                                            ui.add_space(((card_width - 16.0 - slot_size.x) / 2.0_f32).max(0.0_f32));
                                            let response = draw_thumbnail(ui);
                                            let image_rect = response.rect;
                                            response.context_menu(|ui| menu(ui, &mut verify, &mut change_type, &mut update_thumbnail, &mut toggle_hidden, &mut remove));
                                            // Match the Fab eye vector and behaviour. Hide changes
                                            // catalogue visibility only; it never modifies archives.
                                            if ui.rect_contains_pointer(image_rect)
                                                || egui::Popup::is_id_open(ui.ctx(), ui.id().with((&id, "zip-folder-grid-popup"))) {
                                                let center = egui::pos2(image_rect.right() - 20.0_f32, image_rect.top() + 20.0_f32);
                                                let eye_rect = egui::Rect::from_center_size(center, egui::Vec2::splat(28.0_f32));
                                                if library_folders::visibility_button(ui, eye_rect,
                                                    ui.id().with((&id, "zip-visibility")), e.hidden, &c.theme).clicked() {
                                                    toggle_hidden = true;
                                                }
                                                let folder_rect = egui::Rect::from_center_size(
                                                    egui::pos2(image_rect.right() - 20.0_f32, image_rect.top() + 54.0_f32),
                                                    egui::Vec2::splat(28.0_f32),
                                                );
                                                let folder_button = library_folders::folder_button(
                                                    ui, folder_rect, ui.id().with((&id, "zip-folder-grid")), &c.theme,
                                                );
                                                egui::Popup::menu(&folder_button)
                                                    .id(ui.id().with((&id, "zip-folder-grid-popup")))
                                                    .show(|ui| {
                                                        ui.label("Move to folder");
                                                        ui.separator();
                                                        if ui.selectable_label(self.catalogue.organization.folder(&id).is_none(), "Unfiled").clicked() {
                                                            assign_folder = Some(None);
                                                            ui.close();
                                                        }
                                                        for folder in self.catalogue.organization.folders() {
                                                            if ui.selectable_label(self.catalogue.organization.folder(&id) == Some(folder.as_str()), folder).clicked() {
                                                                assign_folder = Some(Some(folder.clone()));
                                                                ui.close();
                                                            }
                                                        }
                                                        ui.separator();
                                                        if ui.button("Manage folders…").clicked() {
                                                            self.manage_folders = true;
                                                            ui.close();
                                                        }
                                                    });
                                            }
                                        });
                                        // A fixed-width, multi-line title beneath the thumbnail,
                                        // rather than a clipped single-line label.
                                        ui.add_sized(
                                            [card_width - 16.0_f32, library_card::zip_grid_title_height()],
                                            egui::Label::new(egui::RichText::new(&name).size(14.0).strong().color(ui.visuals().text_color()))
                                                .truncate(),
                                        ).on_hover_text(&name);
                                        // Tags are catalogue/filter metadata, not card content.
                                        // The fixed title height keeps the footer aligned.
                                    }
                                    ui.horizontal(|ui| {
                                        if !grid {
                                            let response = draw_thumbnail(ui);
                                            let image_rect = response.rect;
                                            response.context_menu(|ui| menu(ui, &mut verify, &mut change_type, &mut update_thumbnail, &mut toggle_hidden, &mut remove));
                                            if ui.rect_contains_pointer(image_rect)
                                                || egui::Popup::is_id_open(ui.ctx(), ui.id().with((&id, "zip-folder-list-popup"))) {
                                                let center = egui::pos2(image_rect.right() - 20.0_f32, image_rect.top() + 20.0_f32);
                                                let eye_rect = egui::Rect::from_center_size(center, egui::Vec2::splat(28.0_f32));
                                                if library_folders::visibility_button(ui, eye_rect,
                                                    ui.id().with((&id, "zip-visibility-list")), e.hidden, &c.theme).clicked() {
                                                    toggle_hidden = true;
                                                }
                                                let folder_rect = egui::Rect::from_center_size(
                                                    egui::pos2(image_rect.right() - 20.0_f32, image_rect.top() + 54.0_f32),
                                                    egui::Vec2::splat(28.0_f32),
                                                );
                                                let folder_button = library_folders::folder_button(
                                                    ui, folder_rect, ui.id().with((&id, "zip-folder-list")), &c.theme,
                                                );
                                                egui::Popup::menu(&folder_button)
                                                    .id(ui.id().with((&id, "zip-folder-list-popup")))
                                                    .show(|ui| {
                                                        ui.label("Move to folder");
                                                        ui.separator();
                                                        if ui.selectable_label(self.catalogue.organization.folder(&id).is_none(), "Unfiled").clicked() {
                                                            assign_folder = Some(None);
                                                            ui.close();
                                                        }
                                                        for folder in self.catalogue.organization.folders() {
                                                            if ui.selectable_label(self.catalogue.organization.folder(&id) == Some(folder.as_str()), folder).clicked() {
                                                                assign_folder = Some(Some(folder.clone()));
                                                                ui.close();
                                                            }
                                                        }
                                                        ui.separator();
                                                        if ui.button("Manage folders…").clicked() {
                                                            self.manage_folders = true;
                                                            ui.close();
                                                        }
                                                    });
                                            }
                                        }
                                        let (badge_rect, response) = ui.allocate_exact_size(
                                            library_card::labelled_badge_size(), egui::Sense::click());
                                        let response = response.on_hover_text(format!("{category} · {status}"));
                                        let kind = if category == "Unreal Plugin" { "Plugin" }
                                            else if category == "Unreal Project" { "Project" }
                                            else { "Asset" };
                                        asset_badges::paint_labelled(
                                            ui, badge_rect, kind, true, badge_state,
                                            badge_state.color(), ui.visuals().widgets.noninteractive.bg_stroke.color,
                                            ui.visuals().text_color(),
                                        );
                                        response.context_menu(|ui| menu(ui, &mut verify, &mut change_type, &mut update_thumbnail, &mut toggle_hidden, &mut remove));
                                        if !grid {
                                            ui.vertical(|ui| {
                                                ui.label(&name);
                                                ui.small(format!("{category} · {status}"));
                                            });
                                        } else {
                                            ui.add_enabled(
                                                e.present && !self.busy && !self.installing,
                                                egui::Button::new(match category.as_str() {
                                                    "Unreal Project" => "Create Project",
                                                    "Unreal Plugin" => "Install to Engine",
                                                    _ => "Add to Project",
                                                }),
                                            ).clicked().then(|| extract = true);
                                        }
                                    });
                                    if !grid {
                                        if ui.add_enabled(e.present && !self.busy && !self.installing,
                                            egui::Button::new(match category.as_str() {
                                                "Unreal Project" => "Create Project",
                                                "Unreal Plugin" => "Install to Engine",
                                                _ => "Add to Project",
                                            })).clicked() { extract = true; }
                                    }
                                });
                            });
                            if let Some(destination) = assign_folder {
                                match self.catalogue.organization.move_item(&id, destination.as_deref()) {
                                    Ok(()) => self.save(),
                                    Err(error) => self.notice = error,
                                }
                            }
                            if update_thumbnail {
                                self.thumbnail_update = Some(id.clone());
                            }
                            if toggle_hidden {
                                if let Some(entry) = self.catalogue.entries.get_mut(&id) {
                                    entry.hidden = !entry.hidden;
                                }
                                self.save();
                            }
                            if verify {
                                self.verify(id.clone(), force_full);
                            }
                            if extract {
                                self.install_as_asset = category != "Unreal Project";
                                self.setup_id = Some(id.clone());
                                self.project_folder_name = zipcache::suggested_install_name(
                                    e.path.file_stem().and_then(|n| n.to_str()).unwrap_or("Unreal_Asset")
                                );
                                if self.install_as_asset && self.target_project.is_empty() {
                                    if let Some((_, path)) = self.known_projects.first() {
                                        self.target_project = path.clone();
                                    }
                                }
                            }
                            if remove {
                                self.remove = Some(id.clone());
                            }
                            if change_type {
                                self.classification_draft = e.category_override.clone();
                                self.classify = Some(id.clone());
                            }
                           }
                          });
                        }
                    });
            });
        });
        self.dialogs(ui.ctx(), staging_root);
    }
    /// Publish off the UI thread. The destination is captured when the user
    /// clicks Install/Retry, so editing a control cannot redirect an active copy.
    fn try_install(&mut self) {
        if self.installing {
            return;
        }
        let Some((_id, folder, report)) = self.pending.clone() else {
            return;
        };
        if self.destination.trim().is_empty() {
            self.notice = "Choose a destination folder before installation".into();
            return;
        }
        let destination = if self.install_as_asset {
            let project = PathBuf::from(&self.target_project);
            if !project.is_file()
                || project
                    .extension()
                    .is_none_or(|e| !e.eq_ignore_ascii_case("uproject"))
            {
                self.notice = "Select an existing .uproject file before adding assets".into();
                return;
            }
            let Some(root) = project.parent() else {
                self.notice = "Project folder is missing".into();
                return;
            };
            let content = root.join("Content");
            if !content.is_dir() {
                self.notice = "Selected Unreal project does not have a Content folder".into();
                return;
            }
            content
        } else {
            PathBuf::from(&self.destination)
        };
        let name = self.project_folder_name.clone();
        if let Err(error) = zipcache::validate_install_folder_name(&name) {
            self.notice = format!("Invalid destination name: {error}");
            return;
        }
        let staged_project = self.staged_project.clone();
        if self.install_as_asset && staged_project.is_some() {
            self.notice = "This archive contains a complete Unreal project; use Create Project instead of Add to Project".into();
            return;
        }
        if !self.install_as_asset && staged_project.is_none() {
            self.notice =
                "No .uproject descriptor detected in ZIP; cannot create Unreal Project".into();
            return;
        }
        let source = if self.install_as_asset {
            // Asset archives with a Content directory should publish its contents,
            // not a nested Content/Content tree. Never touch existing target files.
            let staged_root = staged_project
                .as_ref()
                .map(|p| p.root.as_path())
                .unwrap_or(&folder);
            let content = staged_root.join("Content");
            if content.is_dir() {
                content
            } else {
                staged_root.to_path_buf()
            }
        } else {
            staged_project
                .as_ref()
                .map(|p| p.root.clone())
                .unwrap_or_else(|| folder.clone())
        };
        if !self.install_as_asset && staged_project.as_ref().is_some_and(|p| !p.cpp_project) {
            if let Some(project) = staged_project.as_ref() {
                if let Err(error) = zipcache::rename_blueprint_descriptor(project, &name) {
                    self.notice = format!("Could not rename staged project descriptor: {error}");
                    return;
                }
            }
        }
        let (tx, rx) = mpsc::channel();
        self.install_rx = Some(rx);
        self.installing = true;
        self.notice = "Publishing verified staged files…".into();
        thread::spawn(move || {
            let outcome = zipcache::publish_staged_into_new_directory(
                &source,
                &destination,
                &name,
                report.extracted_bytes,
            );
            let _ = tx.send(Done::Install(outcome));
        });
    }
    fn record_history(event: &str, detail: &str) {
        use std::io::Write;
        let file = storage::data_dir().join("custom-zip-history.jsonl");
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut out) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let entry = serde_json::json!({"event": event, "detail": detail,
                "unix_secs": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_secs()).unwrap_or_default()});
            let _ = writeln!(out, "{entry}");
        }
    }
    fn poll_install(&mut self) {
        let event = self.install_rx.as_ref().and_then(|r| r.try_recv().ok());
        let Some(Done::Install(outcome)) = event else {
            return;
        };
        self.install_rx = None;
        self.installing = false;
        match outcome {
            Ok(dest) => {
                if let Some((_, folder, report)) = self.pending.take() {
                    self.staged_project = None;
                    self.notice = format!(
                        "Installed {} files into {} ({} links skipped)",
                        report.regular_files,
                        dest.display(),
                        report.skipped_links.len()
                    );
                    if let Err(e) = std::fs::remove_dir_all(&folder) {
                        self.notice
                            .push_str(&format!("; staging cleanup needs attention: {e}"));
                    }
                }
                Self::record_history("installed", &self.notice);
                self.space_warning = None;
            }
            Err(zipcache::PublishError::NoSpace {
                required,
                available,
            }) => {
                self.space_warning = Some((required, available));
                self.notice = "Installation waiting for disk space. Select Retry, Stop or Change Destination.".into();
            }
            Err(e) => {
                self.notice = format!("Installation failed; staging retained for recovery: {e}");
                Self::record_history("install_failed", &self.notice);
            }
        }
    }
    fn dialogs(&mut self, ctx: &Context, staging_root: &str) {
        if self.manage_folders {
            let mut open = true;
            egui::Window::new("ZIP Library folders")
                .id(egui::Id::new("zip-library-folders-manager"))
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Virtual folders only — original ZIP archives are never moved.");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.new_folder_name);
                        if ui.button("Create folder").clicked() {
                            match self.catalogue.organization.create_folder(&self.new_folder_name) {
                                Ok(_) => { self.new_folder_name.clear(); self.folder_error.clear(); self.save(); },
                                Err(error) => self.folder_error = error,
                            }
                        }
                    });
                    if !self.folder_error.is_empty() { ui.colored_label(egui::Color32::LIGHT_RED, &self.folder_error); }
                    for name in self.catalogue.organization.folders().to_vec() {
                        ui.horizontal(|ui| {
                            ui.label(&name);
                            if ui.button(format!("Delete##zip-folder-{name}")).clicked() {
                                self.catalogue.organization.delete_folder(&name);
                                if self.folder_filter == FolderFilter::Folder(name.clone()) {
                                    self.folder_filter = FolderFilter::All;
                                }
                                self.save();
                            }
                        });
                    }
                });
            self.manage_folders = open;
        }

        if self.show_validation_log {
            let mut open = self.show_validation_log;
            egui::Window::new("Manual Validation Report")
                .open(&mut open)
                .resizable(true)
                .default_width(750.0)
                .show(ctx, |ui| {
                    if let Some(path) = self.session_log.as_ref() {
                        ui.label(path.display().to_string());
                        if let Ok(contents) = std::fs::read_to_string(path) {
                            egui::ScrollArea::vertical()
                                .max_height(460.0)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(contents).monospace())
                                            .selectable(true),
                                    );
                                });
                        } else {
                            ui.label("Validation log could not be read");
                        }
                    }
                });
            self.show_validation_log = open;
        }
        if let Some(id) = self.thumbnail_update.clone() {
            let mut open = true;
            egui::Window::new("Update ZIP Asset Thumbnail")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Choose a local image to override this ZIP's automatic preview.");
                    ui.label("The chosen image is retained through subsequent CRC checks.");
                    if ui.button("Browse Computer…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
                            .pick_file()
                        {
                            match std::fs::read(&path) {
                                Ok(bytes) if image::load_from_memory(&bytes).is_ok() => {
                                    let dest = Self::manual_thumbnail_path(&id);
                                    let write_result = dest
                                        .parent()
                                        .ok_or_else(|| "Invalid thumbnail path".to_string())
                                        .and_then(|parent| {
                                            std::fs::create_dir_all(parent)
                                                .map_err(|e| e.to_string())
                                        })
                                        .and_then(|_| {
                                            std::fs::write(&dest, bytes).map_err(|e| e.to_string())
                                        });
                                    match write_result {
                                        Ok(()) => {
                                            self.thumbnails.remove(&id);
                                            self.notice = "Updated ZIP thumbnail".into();
                                            self.thumbnail_update = None;
                                        }
                                        Err(err) => {
                                            self.notice = format!("Thumbnail update failed: {err}")
                                        }
                                    }
                                }
                                Ok(_) => {
                                    self.notice = "Selected file is not a supported image".into()
                                }
                                Err(err) => self.notice = format!("Cannot read image: {err}"),
                            }
                        }
                    }
                    if Self::manual_thumbnail_path(&id).exists()
                        && ui.button("Restore Automatic Thumbnail").clicked()
                    {
                        if let Err(err) = std::fs::remove_file(Self::manual_thumbnail_path(&id)) {
                            self.notice = format!("Cannot restore automatic thumbnail: {err}");
                        } else {
                            self.thumbnails.remove(&id);
                            self.thumbnail_update = None;
                        }
                    }
                });
            if !open {
                self.thumbnail_update = None;
            }
        }

        if let Some(id) = self.classify.clone() {
            let mut close = false;
            let mut save = false;
            if let Some(entry) = self.catalogue.entries.get(&id) {
                egui::Window::new("Change ZIP Asset Type")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(format!("Detected automatically: {}", entry.category));
                        ui.label("The override changes the Library display and preferred action; descriptor and CRC checks remain mandatory.");
                        for (label, value) in [
                            ("Use automatic detection", None),
                            ("Unreal Project", Some("Unreal Project")),
                            ("Unreal Plugin", Some("Unreal Plugin")),
                            ("Asset Pack", Some("Asset Pack")),
                            ("Unclassified", Some("Unclassified")),
                        ] {
                            let target = value.map(str::to_string);
                            ui.radio_value(&mut self.classification_draft, target, label);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() { save = true; close = true; }
                            if ui.button("Cancel").clicked() { close = true; }
                        });
                    });
            } else {
                close = true;
            }
            if save {
                if let Some(entry) = self.catalogue.entries.get_mut(&id) {
                    entry.category_override = self.classification_draft.clone();
                }
                self.save();
            }
            if close {
                self.classify = None;
            }
        }
        if let Some(id) = self.remove.clone() {
            let mut close = false;
            egui::Window::new("Remove ZIP from Library?")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        "Only the Library record is removed. The ZIP on disk is never deleted.",
                    );
                    if ui.button("Remove from Library").clicked() {
                        self.catalogue.remove_record(&id);
                        self.save();
                        close = true;
                    }
                    if ui.button("Keep entry").clicked() {
                        close = true;
                    }
                });
            if close {
                self.remove = None;
            }
        }
        if let Some((id, links)) = self.decision.clone() {
            let mut cancel = false;
            let mut skip = false;
            egui::Window::new("Links detected in ZIP")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{} symbolic links detected. Links are never extracted.",
                        links.len()
                    ));
                    for link in links.iter().take(12) {
                        ui.label(link);
                    }
                    if ui.button("Stop extraction").clicked() {
                        cancel = true;
                    }
                    if ui.button("Skip all links for this archive").clicked() {
                        skip = true;
                    }
                });
            if cancel {
                self.decision = None;
            }
            if skip {
                self.decision = None;
                if let Some(e) = self.catalogue.entries.get(&id) {
                    self.start_extraction(
                        id.clone(),
                        e.path.clone(),
                        staging_root.into(),
                        LinkPolicy::SkipAll,
                    );
                }
            }
        }
        if let Some(id) = self.setup_id.clone() {
            let mut begin = false;
            let mut cancel_setup = false;
            let mut browse_parent = false;
            let mut browse_project = false;
            egui::Window::new(if self.install_as_asset { "Add ZIP assets to project" } else { "Create Unreal Project" })
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label("Choose the destination before extracting or staging the ZIP.");
                    ui.horizontal(|ui| {
                        ui.label(if self.install_as_asset { "Asset folder name" } else { "Project name" });
                        ui.text_edit_singleline(&mut self.project_folder_name);
                    });
                    if self.install_as_asset {
                        ui.label("Known Unreal projects:");
                        egui::ComboBox::from_id_salt("zip-known-projects")
                            .selected_text(self.known_projects.iter().find(|(_, path)| path == &self.target_project)
                                .map(|(name, _)| name.as_str()).unwrap_or("Other project (browse below)"))
                            .show_ui(ui, |ui| {
                                for (name, path) in &self.known_projects {
                                    ui.selectable_value(&mut self.target_project, path.clone(), name);
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label("Project (.uproject)");
                            ui.text_edit_singleline(&mut self.target_project);
                            if ui.button("Browse to Project…").clicked() { browse_project = true; }
                        });
                        ui.small("Assets are installed into a new named folder under the selected project's Content directory. Existing files will not be overwritten.");
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Destination parent folder");
                            ui.text_edit_singleline(&mut self.destination);
                            if ui.button("Browse…").clicked() { browse_parent = true; }
                        });
                        ui.label(format!("New project folder: {}", PathBuf::from(&self.destination).join(&self.project_folder_name).display()));
                    }
                    let name_ok = zipcache::validate_install_folder_name(&self.project_folder_name).is_ok();
                    let destination_ok = if self.install_as_asset { !self.target_project.trim().is_empty() } else { !self.destination.trim().is_empty() };
                    ui.horizontal(|ui| {
                        if ui.add_enabled(name_ok && destination_ok, egui::Button::new(if self.install_as_asset { "Add to Project" } else { "Create Project" })).clicked() { begin = true; }
                        if ui.button("Cancel").clicked() { cancel_setup = true; }
                    });
                });
            if browse_parent {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose new project parent folder")
                    .pick_folder()
                {
                    self.destination = path.to_string_lossy().into_owned();
                }
            }
            if browse_project {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose an Unreal .uproject file")
                    .add_filter("Unreal projects", &["uproject"])
                    .pick_file()
                {
                    self.target_project = path.to_string_lossy().into_owned();
                }
            }
            if cancel_setup {
                self.setup_id = None;
            }
            if begin {
                if self.install_as_asset {
                    let selected = PathBuf::from(&self.target_project);
                    if !selected.is_file()
                        || selected
                            .extension()
                            .is_none_or(|e| !e.eq_ignore_ascii_case("uproject"))
                    {
                        self.notice =
                            "Choose a valid .uproject file (from known projects or Browse)".into();
                        return;
                    }
                } else if !PathBuf::from(&self.destination).is_dir() {
                    self.notice = "Choose an existing destination parent directory".into();
                    return;
                }
                self.setup_id = None;
                self.extract(id, staging_root);
            }
        }
        if let Some((_id, folder, report)) = self.pending.clone() {
            let mut stop = false;
            let mut retry = false;
            let mut choose = false;
            let mut inspect = false;
            egui::Window::new(if self.space_warning.is_some() {
                "Insufficient disk space"
            } else {
                "ZIP installation"
            })
            .collapsible(false)
            .show(ctx, |ui| {
                if self.installing {
                    ui.spinner();
                    ui.label("Installing in background…");
                }
                ui.label(format!(
                    "Extracted {} regular files ({} bytes)",
                    report.regular_files, report.extracted_bytes
                ));
                if !report.skipped_links.is_empty() {
                    ui.colored_label(
                        Color32::YELLOW,
                        format!(
                            "{} links skipped; inspect files before installation",
                            report.skipped_links.len()
                        ),
                    );
                }
                if let Some((need, free)) = self.space_warning {
                    ui.colored_label(
                        Color32::RED,
                        format!(
                            "Required: {} bytes · Available: {} bytes · Shortfall: {} bytes",
                            need,
                            free,
                            need.saturating_sub(free)
                        ),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label("Project / Asset Folder Name");
                    ui.text_edit_singleline(&mut self.project_folder_name);
                });
                if let Some(project) = &self.staged_project {
                    ui.label(if project.cpp_project {
                        "C++ source detected: original .uproject filename will be retained"
                    } else {
                        "Blueprint project: .uproject filename will match chosen folder name"
                    });
                    ui.small(format!("Original descriptor: {}", project.descriptor_name));
                }
                ui.horizontal(|ui| {
                    ui.label("Destination parent");
                    ui.text_edit_singleline(&mut self.destination);
                });
                ui.label(format!(
                    "Final destination: {}",
                    PathBuf::from(&self.destination)
                        .join(&self.project_folder_name)
                        .display()
                ));
                if ui
                    .add_enabled(!self.installing, egui::Button::new("Change Destination"))
                    .clicked()
                {
                    choose = true;
                }
                if !report.skipped_links.is_empty() && ui.button("Check Staging Folder").clicked() {
                    inspect = true;
                }
                if ui
                    .add_enabled(
                        !self.installing,
                        egui::Button::new(if self.space_warning.is_some() {
                            "Retry"
                        } else {
                            "Continue Installation"
                        }),
                    )
                    .clicked()
                {
                    retry = true;
                }
                if ui
                    .add_enabled(!self.installing, egui::Button::new("Stop"))
                    .clicked()
                {
                    stop = true;
                }
            });
            if choose {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.destination = path.to_string_lossy().into_owned();
                    self.space_warning = None;
                }
            }
            if inspect {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(&folder)
                    .spawn();
            }
            if stop && !self.installing {
                self.pending = None;
                self.staged_project = None;
                self.space_warning = None;
                self.notice = format!(
                    "Installation skipped. Staging preserved for review: {}",
                    folder.display()
                );
            }
            if retry && !self.installing {
                self.try_install();
            }
        }
    }
}
