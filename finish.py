from pathlib import Path
p=Path('src/ui/custom_zip_view.rs');s=p.read_text()
# scan UI owns catalogue saves
s=s.replace('''                c.save(&path)?;
                Ok(c)''','''                Ok(c)''',1)
s=s.replace('''                Done::Scan(Ok(c)) => {
                    self.catalogue = c;
                    self.notice = "ZIP Library scan completed".into();
                }''','''                Done::Scan(Ok(c)) => {
                    self.catalogue = c;
                    self.notice = "ZIP Library scan completed".into();
                    self.save();
                }''')
# click UI always manual call log record
s=s.replace('''    show_hidden: bool,
''','''    show_hidden: bool,
    session_log: Option<PathBuf>,
''')
s=s.replace('''        self.verify_queue.push_back(id);
''','''        self.verify_queue.push_back(id);
''')
s=s.replace('''        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            // Verification workers do not load, mutate or save the Library catalogue.
            let result = zipcache::check_archive(&archive);
            let _ = tx.send(Done::Verify(id, result));
        });''','''        let known = self.catalogue.entries.get(&id).and_then(|entry| {
            if entry.verification == Verification::Valid {
                entry.fingerprint.clone().map(|hash| (hash, entry.category.clone(), entry.verified_entries, entry.uncompressed_bytes))
            } else { None }
        });
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            // Work is strictly read-only; only the Library manager persists results.
            let result = (|| {
                if let Some((previous, category, entries, expanded_bytes)) = known {
                    let fresh = zipcache::quick_fingerprint(&archive)?;
                    if fresh == previous {
                        let meta = std::fs::metadata(&archive).map_err(|e| zipcache::VerifyError::Io(e.to_string()))?;
                        let time = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
                        return Ok(zipcache::ArchiveCheck { fingerprint: fresh, entries, expanded_bytes,
                            category, source_bytes: meta.len(), source_modified_unix_secs: time });
                    }
                }
                zipcache::check_archive(&archive)
            })();
            let _ = tx.send(Done::Verify(id, result));
        });''')
# session log helper
point=s.index('    fn extract(&mut self,')
s=s[:point]+'''    fn write_manual_validation_log(&mut self, id: &str, outcome: &str, detail: &str) {
        use std::io::Write;
        let folder = storage::data_dir().join("ValidationLogs");
        if std::fs::create_dir_all(&folder).is_err() { return; }
        if self.session_log.is_none() {
            let now = time::OffsetDateTime::now_utc();
            let (year, month, day) = now.to_calendar_date();
            let filename = format!("Validation_{year:04}-{:02}-{day:02}_{:02}-{:02}-{:02}.log",
                u8::from(month), now.hour(), now.minute(), now.second());
            self.session_log = Some(folder.join(filename));
            if let Ok(mut files) = std::fs::read_dir(&folder).map(|r| r.flatten()
                .filter_map(|e| { let path = e.path();
                    path.file_name().and_then(|n| n.to_str()).filter(|n| n.starts_with("Validation_") && n.ends_with(".log"))?;
                    Some(path)
                }).collect::<Vec<_>>()) {
                files.sort();
                while files.len() > 10 { let old = files.remove(0); let _ = std::fs::remove_file(old); }
            }
        }
        let Some(file) = self.session_log.as_ref() else { return; };
        if let Ok(mut output) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
            let now = time::OffsetDateTime::now_utc();
            let (year, month, day) = now.to_calendar_date();
            let (path, root) = self.catalogue.entries.get(id).map(|entry| {
                let parent = entry.path.parent().unwrap_or(&entry.path);
                (entry.path.display().to_string(), parent.display().to_string())
            }).unwrap_or_default();
            let _ = writeln!(output,
                "\\n------------------------------------------------------------\\nUTC: {year:04}-{:02}-{day:02} {:02}:{:02}:{:02}\\nLibrary: Custom ZIP Library\\nCache/Parent: {root}\\nArchive: {path}\\nOutcome: {outcome}\\nDetails: {detail}\\n------------------------------------------------------------",
                u8::from(month), now.hour(), now.minute(), now.second());
        }
    }
''' +s[point:]
# log each verify poll: after match before -> easiest branch
s=s.replace('''                    self.save();
                }
                Done::Verify(id, Err(e)) =>''','''                    self.write_manual_validation_log(&id, "VALID", "ZIP CRC / stored XXH3-128 fingerprint verified");
                    self.save();
                }
                Done::Verify(id, Err(e)) =>''')
s=s.replace('''                    self.notice = format!("ZIP verification {}: {e}", if matches!(e, zipcache::VerifyError::Invalid(_)) { "invalid" } else { "filesystem error" });
                    self.save();''','''                    self.notice = format!("ZIP verification {}: {e}", if matches!(e, zipcache::VerifyError::Invalid(_)) { "invalid" } else { "filesystem error" });
                    self.write_manual_validation_log(&id, if matches!(e, zipcache::VerifyError::Invalid(_)) { "INVALID" } else { "FILESYSTEM ERROR" }, &e.to_string());
                    self.save();''')
# list log link - simpler open folder button on platform
s=s.replace('''                    ui.heading("Custom ZIP Assets");''','''                    ui.heading("Custom ZIP Assets");
                    if ui.button("Validation Logs").clicked() {
                        let folder = storage::data_dir().join("ValidationLogs");
                        let _ = std::fs::create_dir_all(&folder);
                        #[cfg(windows)] { let _ = std::process::Command::new("explorer.exe").arg(folder).spawn(); }
                        #[cfg(not(windows))] { self.notice = format!("Validation logs: {}", folder.display()); }
                    }''')
# delete artificial horizontal margin block, thumbnail center via horizontal layout + content all centered
s=s.replace('''                                    if grid { ui.horizontal(|ui| { ui.add_space((card_width - image_size.x - 16.0).max(0.0) / 2.0); }); }
''','')
s=s.replace('''                                        let response = draw_thumbnail(ui);
                                        response.context_menu''','''                                        let response = ui.horizontal(|ui| {
                                            ui.add_space(((card_width - image_size.x - 16.0) / 2.0).max(0.0));
                                            draw_thumbnail(ui)
                                        }).inner;
                                        response.context_menu''',1)
s=s.replace('''                                        ui.add_sized(
                                            [card_width - 16.0, 38.0],
                                            egui::Label::new(egui::RichText::new(&name).size(14.0).strong().color(ui.visuals().text_color()))
                                                .wrap(),
                                        ).on_hover_text(&name);''','''                                        ui.add_sized(
                                            [card_width - 16.0, 27.0],
                                            egui::Label::new(egui::RichText::new(&name).size(14.0).strong().color(ui.visuals().text_color()))
                                                .truncate(),
                                        ).on_hover_text(&name);''')
p.write_text(s)
