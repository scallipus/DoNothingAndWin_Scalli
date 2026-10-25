from pathlib import Path
p=Path('src/ui/custom_zip_view.rs'); s=p.read_text()
s=s.replace('    Verify(String, Result<Catalogue, String>),','    Verify(String, Result<zipcache::ArchiveCheck, zipcache::VerifyError>),')
a=s.index('        self.save();\n        self.busy = true;',s.index('fn start_next_verification'))
b=s.index('    fn extract(',a)
s=s[:a]+'''        self.busy = true;
        self.active_verification = Some(id.clone());
        if let Some(e) = self.catalogue.entries.get_mut(&id) {
            e.verification = Verification::Checking;
        }
        let Some(archive) = self.catalogue.entries.get(&id).map(|e| e.path.clone()) else {
            self.busy = false;
            self.active_verification = None;
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            // Verification workers do not load, mutate or save the Library catalogue.
            let result = zipcache::check_archive(&archive);
            let _ = tx.send(Done::Verify(id, result));
        });
    }
''' + s[b:]
a=s.index('                Done::Verify(_, Ok(c)) =>'); b=s.index('                Done::Extract(',a)
s=s[:a]+'''                Done::Verify(id, Ok(check)) => {
                    // The UI Library manager is the sole owner of catalogue state.
                    if let Some(record) = self.catalogue.entries.get_mut(&id) {
                        let still_same = std::fs::metadata(&record.path).ok().is_some_and(|meta| {
                            let modified = meta.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
                            meta.len() == check.source_bytes && modified == check.source_modified_unix_secs
                        });
                        if still_same {
                            record.verification = Verification::Valid;
                            record.fingerprint = Some(check.fingerprint);
                            record.verified_entries = check.entries;
                            record.uncompressed_bytes = check.expanded_bytes;
                            record.category = check.category;
                            self.notice.clear();
                        } else {
                            record.verification = Verification::Unchecked;
                            self.notice = "ZIP changed during verification; result discarded".into();
                        }
                    }
                    self.save();
                }
                Done::Verify(id, Err(e)) => {
                    if let Some(record) = self.catalogue.entries.get_mut(&id) {
                        record.verification = match &e {
                            zipcache::VerifyError::Invalid(reason) => Verification::Invalid(reason.clone()),
                            zipcache::VerifyError::Io(_) => Verification::Unchecked,
                        };
                        if matches!(e, zipcache::VerifyError::Invalid(_)) {
                            record.fingerprint = None;
                        }
                    }
                    self.notice = format!("ZIP verification {}: {e}", if matches!(e, zipcache::VerifyError::Invalid(_)) { "invalid" } else { "filesystem error" });
                    self.save();
                }
''' + s[b:]
s=s.replace('''                        let card_width = (if chosen_size == 0 { 220.0 } else {
                            f32::from(chosen_size) + 18.0
                        }).max(180.0_f32);
                        let columns = if grid {
                            ((ui.available_width() + 10.0) / (card_width + 10.0)).floor().max(1.0) as usize
                        } else { 1 };''','''                        let desired_width = if chosen_size == 0 { LIBRARY_CARD_WIDTH } else {
                            (f32::from(chosen_size) + 2.0).max(LIBRARY_CARD_WIDTH)
                        };
                        let (columns, card_width) = if grid {
                            card_grid_metrics(ui.available_width() - scrollbar::GRID_ALLOCATED_WIDTH
                                - 2.0 * scrollbar::GRID_MARGIN - PROJECT_CARD_GAP,
                                desired_width, PROJECT_CARD_GAP)
                        } else { (1, ui.available_width()) };''')
s=s.replace('''egui::vec2(card_width - 14.0, (card_width - 14.0) / 1.6)''','''egui::vec2(card_width - 2.0, (card_width - 2.0) / LIBRARY_THUMBNAIL_ASPECT)''')
s=s.replace('''ui.add(egui::Image::new(texture).fit_to_exact_size(image_size)
                                                .sense(egui::Sense::click()))''','''ui.add(egui::Image::new(texture).fit_to_exact_size(image_size)
                                                .sense(egui::Sense::click()))''')
s=s.replace('''                                    let draw_thumbnail = |ui: &mut egui::Ui| {''','''                                    if grid { ui.horizontal(|ui| { ui.add_space((card_width - image_size.x - 16.0).max(0.0) / 2.0); }); }
                                    let draw_thumbnail = |ui: &mut egui::Ui| {''') # rework later
s=s.replace('''egui::Label::new(egui::RichText::new(&name).size(14.0).strong())''','''egui::Label::new(egui::RichText::new(&name).size(14.0).strong().color(ui.visuals().text_color()))''')
p.write_text(s)

p=Path('src/custom_zip_cache.rs'); s=p.read_text()
index=s.index('fn verify_archive(')
s=s[:index]+'''/// Worker outcome: Valid on full inspection; Invalid only for proven ZIP corruption.
/// Filesystem failures are execution errors, not archive validity results.
#[derive(Debug)]
pub enum VerifyError { Invalid(String), Io(String) }
impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Invalid(reason) => write!(f, "{reason}"), Self::Io(reason) => write!(f, "{reason}") }
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
    let meta = fs::metadata(path).map_err(|e| VerifyError::Io(format!("{}: {e}", path.display())))?;
    let result = verify_archive(path);
    let (fingerprint, entries, expanded_bytes, category) = match result {
        Ok(data) => data,
        Err(error) => {
            // OS failures are not proof of archive damage.
            if error.contains("os error") || error.contains("Access is denied") || error.contains("Permission denied") || error.contains("failed to fill whole buffer") {
                return Err(VerifyError::Io(error));
            }
            return Err(VerifyError::Invalid(error));
        }
    };
    Ok(ArchiveCheck { fingerprint, entries, expanded_bytes, category,
        source_bytes: meta.len(), source_modified_unix_secs: metadata_time(&meta) })
}

/// Recheck a previously full-CRC-validated archive without decompression.
pub fn quick_fingerprint(path: &Path) -> Result<String, VerifyError> {
    let mut file = fs::File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut hasher = XxHash3_128::default();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| VerifyError::Io(e.to_string()))?;
        if n == 0 { break; }
        hasher.write(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.finish_128()))
}
''' + s[index:]
# use robust primary save with backup never delete unless needed: keep operation approach but fix direct lock issue via windows replace via rename backup? leave as is for now
p.write_text(s)
