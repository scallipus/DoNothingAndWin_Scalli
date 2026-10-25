use super::*;
use engine_workspace::discovery;
#[derive(Default)]
pub(super) struct EngineSpace {
    key: Vec<String>,
    result: Option<(u64, bool)>,
    rx: Option<Receiver<(u64, bool)>>,
    checked: Option<Instant>,
}

#[derive(Default)]
pub(super) struct VaultDiscovery {
    pub path: String,
    pub rx: Option<Receiver<Result<engine_workspace::vault_scan::VaultScan, String>>>,
}
#[derive(Default)]
pub(super) struct CacheSpace {
    path: String,
    last_checked: Option<Instant>,
    rx: Option<Receiver<(Option<u64>, Option<u64>)>>,
    result: Option<(Option<u64>, Option<u64>)>,
}
fn cache_folder_bytes(root: &Path, exclude_workspace: bool) -> Option<u64> {
    if !root.is_dir() {
        return None;
    }
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let name = entry.file_name();
            if exclude_workspace && dir == root && name == "EngineWorkspace" {
                continue;
            }
            let ty = entry.file_type().ok()?;
            if ty.is_symlink() {
                continue;
            }
            if ty.is_dir() {
                stack.push(entry.path());
            } else if ty.is_file() {
                total = total.saturating_add(entry.metadata().ok()?.len());
            }
        }
    }
    Some(total)
}
fn cache_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut index = 0;
    while value >= 1024. && index + 1 < UNITS.len() {
        value /= 1024.;
        index += 1;
    }
    format!("{value:.2} {}", UNITS[index])
}
impl CacheSpace {
    fn show(&mut self, ui: &mut Ui, path: &str, chunk_only: bool) {
        if self.rx.is_none()
            && (self.path != path
                || self
                    .last_checked
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(300)))
        {
            self.path = path.to_string();
            self.result = None;
            self.last_checked = Some(Instant::now());
            let path = std::path::PathBuf::from(path);
            let (tx, rx) = mpsc::channel();
            self.rx = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                // Report volume free space immediately, before the potentially
                // lengthy recursive cache-size scan (especially network shares).
                let free = fs2::available_space(&path).ok();
                let _ = tx.send((None, free));
                ctx.request_repaint();
                let scanned = if chunk_only {
                    cache_folder_bytes(&path.join("EngineWorkspace/Chunks"), false)
                } else {
                    cache_folder_bytes(&path, true)
                };
                let _ = tx.send((scanned, free));
                ctx.request_repaint();
            });
        }
        if let Some(result) = self.rx.as_ref().map(|rx| rx.try_recv()) {
            match result {
                Ok(result) => {
                    let has_scanned = result.0.is_some();
                    self.result = Some(result);
                    if has_scanned {
                        self.rx = None;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.horizontal_wrapped(|ui| {
            let used = self
                .result
                .as_ref()
                .and_then(|x| x.0)
                .map(cache_size)
                .unwrap_or_else(|| {
                    if self.rx.is_some() {
                        "Calculating…".into()
                    } else {
                        "Unavailable".into()
                    }
                });
            let free = self
                .result
                .as_ref()
                .and_then(|x| x.1)
                .map(cache_size)
                .unwrap_or_else(|| "Unavailable".into());
            ui.label(format!("Cache Used: {used}"));
            ui.label(format!("Storage Free: {free}"));
        });
    }
}

impl Workspace {
    pub(super) fn add_project_root(&mut self, path: std::path::PathBuf, ctx: &Context) {
        self.add_project_roots(vec![path], ctx);
    }
    pub(super) fn add_project_roots(&mut self, paths: Vec<std::path::PathBuf>, ctx: &Context) {
        let mut added = 0;
        for path in paths {
            if !path.is_dir() {
                continue;
            }
            if discovery::project_roots(&self.prefs.project_roots)
                .iter()
                .any(|root| discovery::inside(&path, Path::new(root)))
            {
                continue;
            }
            self.prefs
                .project_roots
                .push(dialog::display_path(&path.to_string_lossy()));
            added += 1;
        }
        if added == 0 {
            self.notify("These folders are already tracked or are no longer accessible.");
            return;
        }
        self.reconcile_project_roots(ctx);
        self.notify(format!(
            "Added {added} project folders to Settings. Finding their projects…"
        ));
    }
    fn reconcile_project_roots(&mut self, ctx: &Context) {
        self.scan
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.scan = transfers::Scan::default();
        discovery::retain_project_roots(
            &mut self.projects,
            &discovery::project_roots(&self.prefs.project_roots),
            &self.prefs.manual_projects,
        );
        self.persist();
        self.start_scan(ctx.clone(), None);
    }
    pub(super) fn engine_storage_badge(&mut self, ui: &mut Ui, c: &Canvas) {
        let mut key: Vec<_> = self
            .engines
            .iter()
            .filter(|e| self.workspace_cache.engines.get(&e.path) == Some(&true))
            .map(|e| e.path.clone())
            .collect();
        key.sort();
        key.dedup();
        if self.engine_space.rx.is_none()
            && (self.engine_space.key != key
                || self
                    .engine_space
                    .checked
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(300)))
        {
            self.engine_space.key = key.clone();
            self.engine_space.result = None;
            self.engine_space.checked = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            self.engine_space.rx = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let paths = key.iter().map(std::path::PathBuf::from).collect::<Vec<_>>();
                let _ = tx.send(discovery::installed_bytes(&paths));
                ctx.request_repaint();
            });
        }
        if let Some(result) = self
            .engine_space
            .rx
            .as_ref()
            .and_then(|r| r.try_recv().ok())
        {
            self.engine_space.result = Some(result);
            self.engine_space.rx = None;
        }
        let r = c.rect([510., 130., 180., 28.]);
        let p = ui.painter();
        let tint = color(self.theme.muted);
        let s = Stroke::new(1.5_f32, tint);
        let o = r.min + vec2(2., 5.);
        p.add(Shape::line(
            vec![
                o + vec2(3., 15.),
                o + vec2(0., 12.),
                o + vec2(0., 8.),
                o + vec2(3., 5.),
                o + vec2(7., 5.),
                o + vec2(9., 1.),
                o + vec2(14., 0.),
                o + vec2(19., 3.),
                o + vec2(20., 7.),
                o + vec2(24., 8.),
                o + vec2(26., 11.),
                o + vec2(24., 15.),
                o + vec2(3., 15.),
            ],
            s,
        ));
        let label = self
            .engine_space
            .result
            .map(|(n, complete)| {
                format!(
                    "{}{:.1} GB",
                    if complete { "" } else { "≥ " },
                    n as f64 / 1_000_000_000.
                )
            })
            .unwrap_or_else(|| "Calculating…".into());
        p.text(
            r.min + vec2(36., 14.),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.),
            tint,
        );
        ui.interact(r, Id::new("installed-engine-size"), Sense::hover())
            .on_hover_text("Total size of all installed engines");
    }
    pub(super) fn settings_flow(&mut self, ui: &mut Ui, c: &Canvas, ctx: &Context) {
        let top = PageHeaderLayout::new().divider_y + 14.;
        let area = c.rect([271., top, c.width - 297., c.height - top - 62.]);
        let theme = self.theme.clone();
        let mut remove = None;
        ui.scope_builder(UiBuilder::new().max_rect(area),|ui|{
            ui.style_mut().text_styles.insert(TextStyle::Body,FontId::proportional(16.));
            ui.style_mut().text_styles.insert(TextStyle::Button,FontId::proportional(COMPACT_BUTTON_FONT_SIZE));
            ui.style_mut().text_styles.insert(TextStyle::Heading,FontId::proportional(23.));
            AppScrollArea::vertical(&theme).inner_margin(16.).id_salt("settings-flow").auto_shrink([false,true]).show(ui,|ui|{
                ui.set_width(ui.available_width());ui.spacing_mut().item_spacing=vec2(14.,14.);ui.spacing_mut().button_padding=vec2(18.,10.);
                self.account_community_row(ui,&theme);
                settings_section(ui,&theme,|ui| { self.startup_preferences(ui, &theme); });
                settings_section(ui,&theme,|ui|{
                ui.heading("Project folders");
                if downloads::toggle(ui, &theme, &mut self.prefs.scan_projects_on_startup, "Scan project folders on startup", "Search configured project folders when the app starts. Turn off to use Rescan folders or the project refresh button manually.") { self.persist(); }
                if downloads::toggle(ui, &theme, &mut self.prefs.notify_project_scan, "Show scan-completion notifications", "Show the scan result popup. Results and skipped folder paths remain available in Downloads when this is off.") { self.persist(); }
                if !self.scan.summary.is_empty() && ui.add(buttons::secondary_button("View latest scan results")).clicked() {
                    self.page = Page::Downloads;
                    self.downloads.settings = false;
                    self.downloads.show_overview();
                }
                ui.label("Add a single project's folder or a parent folder containing many projects. Projects in all subfolders appear automatically. Removing a folder only removes its projects from this launcher; files stay on disk.");
                ui.separator();
                ui.label(RichText::new("Epic default project folder (fixed)").strong().color(ui.visuals().text_color()));
                ui.label(discovery::default_project_root().to_string_lossy().as_ref());
                for(i,path)in self.prefs.project_roots.iter().enumerate(){
                    if discovery::path_key(Path::new(path))==discovery::path_key(&discovery::default_project_root()){continue;}
                    ui.separator();
                    let (row,_) = ui.allocate_exact_size(vec2(ui.available_width(),42.),Sense::hover());
                    let button_rect=Rect::from_center_size(pos2(row.right()-50.,row.center().y),vec2(100.,COMPACT_BUTTON_HEIGHT));
                    let path_rect=Rect::from_min_max(row.min,pos2(button_rect.left()-14.,row.bottom()));
                    ui.scope_builder(UiBuilder::new().max_rect(path_rect).layout(Layout::left_to_right(Align::Center)),|ui|{
                        ui.add(Label::new(path).truncate()).on_hover_text(path);
                    });
                    if ui.put(button_rect,buttons::secondary_button("Remove")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){remove=Some(i);}
                }
                ui.horizontal_wrapped(|ui|{
                    if ui.add(buttons::secondary_button("Add project folder")).on_hover_text("Choose a folder containing one or many projects, including projects in subfolders.").on_hover_cursor(CursorIcon::PointingHand).clicked(){if let Some(p)=rfd::FileDialog::new().set_title("Choose a folder containing one or more Unreal projects").pick_folder(){self.add_project_root(p,ctx);}}
                    if ui.add(buttons::secondary_button("Find projects on this PC")).clicked(){self.open_project_finder();}
                    if ui.add_enabled(self.scan.receiver.is_none(),buttons::secondary_button("Rescan folders")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.start_scan(ctx.clone(),None);}
                    if self.scan.receiver.is_some(){ui.add(loading::spinner(&theme));ui.label("Scanning project folders…");}
                });
                });
                settings_section(ui, &theme, |ui| {
                    ui.heading("Library appearance");
                    ui.label("Asset card thumbnail size");
                    let previous = self.prefs.library_thumbnail_size;
                    egui::ComboBox::from_id_salt("library-thumbnail-size")
                        .selected_text(match previous {
                            64 => "64 × 64 (Small)",
                            128 => "128 × 128 (Compact)",
                            192 => "192 × 192 (Large)",
                            256 => "256 × 256 (Extra large)",
                            _ => "Original (recommended)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.prefs.library_thumbnail_size, 0, "Original (recommended)");
                            ui.selectable_value(&mut self.prefs.library_thumbnail_size, 64, "64 × 64 (Small)");
                            ui.selectable_value(&mut self.prefs.library_thumbnail_size, 128, "128 × 128 (Compact)");
                            ui.selectable_value(&mut self.prefs.library_thumbnail_size, 192, "192 × 192 (Large)");
                            ui.selectable_value(&mut self.prefs.library_thumbnail_size, 256, "256 × 256 (Extra large)");
                        });
                    if self.prefs.library_thumbnail_size != previous { self.persist(); }
                    ui.label("Changing the size reflows Library cards. Alt + mouse wheel is no longer used.");
                });
                settings_section(ui,&theme,|ui|{

                ui.horizontal(|ui| {
                    cache_location_icon(ui, true);
                    ui.heading("Cache Location Settings");
                });
                ui.horizontal(|ui| {
                    ui.label("Vault locations are managed in Download Settings.");
                    if ui.add(buttons::secondary_button("Manage Vault locations")).clicked() {
                        self.page = Page::Downloads;
                        self.downloads.settings = true;
                    }
                });
                let previous = self.prefs.chunk_cache_uses_vault_cache;
                ui.horizontal(|ui| {
                    cache_location_icon(ui, false);
                    ui.checkbox(&mut self.prefs.chunk_cache_uses_vault_cache, "Chunk Cache Uses Vault Cache");
                });
                if self.prefs.chunk_cache_uses_vault_cache != previous { self.persist(); }
                ui.label("When enabled, chunks use the selected default Vault location. Disable to select a separate chunk cache (such as an SSD).");
                if !self.prefs.chunk_cache_uses_vault_cache {
                    ui.label("Fab Download Chunk Cache location");
                    let old = self.prefs.fab_cache.clone();
                    dialog::folder(ui, &mut self.prefs.fab_cache);
                    if self.prefs.fab_cache != old { self.persist(); }
                }
                self.fab_cache_space.show(ui, self.prefs.chunk_cache_root(), true);
                ui.separator();
                ui.label("Default Epic Vault Cache");
                ui.label(&self.prefs.default_vault_cache);
                self.vault_cache_space.show(ui, &self.prefs.default_vault_cache, false);
                ui.label("Vault inventory and verification across all configured locations are available on the Vault Cache page.");
                });
                settings_section(ui,&theme,|ui|{
                    ui.heading("Custom Asset Cache");
                    ui.label("ZIP archive locations. Original archives remain unchanged; catalogue verification is performed separately.");
                    let mut remove = None;
                    for (index, path) in self.prefs.custom_asset_caches.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(path);
                            if ui.small_button("Remove").clicked() { remove = Some(index); }
                        });
                    }
                    if let Some(index) = remove { self.prefs.custom_asset_caches.remove(index); self.persist(); }
                    if ui.add(buttons::secondary_button("Add archive folder")).clicked() {
                        if let Some(folder) = rfd::FileDialog::new().set_title("Select custom asset archive folder").pick_folder() {
                            let path = dialog::display_path(&folder.to_string_lossy());
                            if !self.prefs.custom_asset_caches.iter().any(|p| p == &path) {
                                self.prefs.custom_asset_caches.push(path); self.persist();
                            }
                        }
                    }
                });
                settings_section(ui,&theme,|ui|{

                ui.heading("Appearance");
                if downloads::toggle(ui, &theme, &mut self.prefs.show_vault_cache_button, "Show Vault Cache in sidebar", "View the cache folder configured in Download settings, including sizes and cached-file removal.") { self.persist(); }
                if downloads::toggle(ui, &theme, &mut self.prefs.show_free_offers_button, "Show Limited-Time Free in sidebar", "Show the Fab limited-time free offers button in the sidebar.") { self.persist(); }
                ui.horizontal_wrapped(|ui|{
                    if ui.add(buttons::secondary_button("Midnight")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.theme=Theme::dark();self.prefs.theme="dark".into();self.theme.apply(ctx);self.persist();}
                    if ui.add(buttons::secondary_button("Daylight")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.theme=Theme::light();self.prefs.theme="light".into();self.theme.apply(ctx);self.persist();}
                });
                });
                settings_section(ui,&theme,|ui|{
                    let heading = ui.heading("Vault Cache verification");
                    if std::env::args().any(|a|a == "--cache-health-settings-preview") { heading.scroll_to_me(Some(Align::TOP)); }
                    self.cache_health_controls(ui);
                });
                settings_section(ui,&theme,|ui|{
                ui.heading("Engine release checks");ui.label("Check for available engine releases while the launcher is open. Updates are always installed manually.");
                ui.horizontal_wrapped(|ui|{
                    ui.label("Check every");if ui.scope(|ui| { buttons::compact_style(ui); ui.add(DragValue::new(&mut self.prefs.check_minutes).range(5..=1440).suffix(" minutes")) }).inner.changed(){self.persist();}
                    if ui.add(buttons::secondary_button("Check now")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.check(ctx.clone());}
                });
                });
                settings_section(ui,&theme,|ui|{
                ui.heading("Plugin updates");
                let busy = self.plugin_work.busy() || self.library.checking_updates() || self.library_work_pending();
                if ui.add_enabled(!busy, buttons::checkbox(&mut self.prefs.wait_for_library_plugin_builds, "Wait for builds listed in the Fab library", &self.theme)).on_disabled_hover_text("Wait for the current plugin check or installation to finish.").changed() {
                    self.persist();
                    self.library.catalogue.clear();
                    self.refresh_library(ctx);
                }
                ui.label("On (default): wait until the compatible package matches the build listed in your Fab library. Off: offer the latest compatible package available from Fab.");
                ui.label("This checks library availability, not publisher approval. If build information is missing, updates wait until it becomes available. Existing installations are unchanged.");
                ui.separator();
                ui.label("A copy of the current plugin is saved before every update. Restore it from Manage plugins → Plugin backups.");
                let before = self.prefs.plugin_backup_days;
                ui.horizontal_wrapped(|ui| {
                    ui.label("Keep new plugin backups");
                    AppComboBox::new(&theme, "plugin-backup-retention")
                        .selected_text(if before == 0 { "Until manually deleted".into() } else { format!("{before} days") })
                        .width(205.)
                        .show_ui(ui, |ui| {
                            for (days, label) in [(7, "7 days (default)"), (14, "14 days"), (30, "30 days"), (0, "Until manually deleted")] {
                                buttons::selectable_value(ui, &mut self.prefs.plugin_backup_days, days, label);
                            }
                        });
                });
                if before != self.prefs.plugin_backup_days { self.persist(); }
                ui.label("Existing backups keep their expiry date. Mark any backup Keep to prevent automatic deletion. Expired backups are removed when you open Plugin backups or update plugins for that engine.");
                });
                settings_section(ui,&theme,|ui|{
                ui.heading("Workspace");ui.horizontal_wrapped(|ui|{
                    if ui.add(buttons::secondary_button("Locate installed engine")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.add_engine();}
                    if ui.add(buttons::secondary_button("Rescan engines")).clone().on_hover_cursor(CursorIcon::PointingHand).clicked(){self.engines=platform::display_engines();self.engine_space.checked=None;self.plugin_counts.clear(); self.workspace_cache.invalidate();self.notify(format!("Engine rescan complete: {} installed engines found.",self.engines.len()));}
                });
                });
                settings_section(ui,&theme,|ui|{
                self.desktop_preferences(ui);
                });
                settings_section(ui,&theme,|ui|{self.launcher_update_settings(ui,ctx);});
                ui.label(RichText::new("Independent developer tool. Not affiliated with or endorsed by Epic Games.").color(color(theme.muted)));
                if let Some(e)=&self.error{ui.label(RichText::new(e).color(color(theme.warning)));}
            });
        });
        if let Some(i) = remove {
            self.prefs.project_roots.remove(i);
            self.reconcile_project_roots(ctx);
        }
    }
}

pub(super) fn settings_section(ui: &mut Ui, theme: &Theme, body: impl FnOnce(&mut Ui)) -> Response {
    Frame::new()
        .fill(color(theme.card))
        .stroke(Stroke::new(1_f32, color(theme.border)))
        .corner_radius(6.)
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        })
        .response
}

/// Painter-drawn cache location symbols, independent of icon fonts.
fn cache_location_icon(ui: &mut egui::Ui, vault: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
    let c = if vault {
        egui::Color32::from_rgb(70, 165, 235)
    } else {
        egui::Color32::from_rgb(55, 190, 145)
    };
    let p = ui.painter();
    let r = rect.shrink(3.0);
    p.rect_stroke(
        r,
        3.0,
        egui::Stroke::new(1.8_f32, c),
        egui::StrokeKind::Inside,
    );
    if vault {
        p.line_segment(
            [
                egui::pos2(r.left() + 3.0, r.center().y),
                egui::pos2(r.right() - 3.0, r.center().y),
            ],
            egui::Stroke::new(1.8_f32, c),
        );
    } else {
        for dx in [-3.0_f32, 3.0] {
            p.circle_filled(r.center() + egui::vec2(dx, 0.0), 1.6, c);
        }
    }
}
