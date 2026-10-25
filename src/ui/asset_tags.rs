//! Local-only shared tag vocabulary and asset assignments. Never sent to Fab.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const DEFAULT_TAGS: &[&str] = &[
    "Environment",
    "Character",
    "FX",
    "Audio",
    "Foley",
    "Music",
    "Animation",
    "Blueprint",
    "Materials",
    "Textures",
    "Props",
    "UI",
    "VFX",
    "Niagara",
    "Landscape",
    "Architecture",
    "Vehicles",
    "Weapons",
    "Gameplay",
    "Tools",
    "Code",
    "Multiplayer",
];

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Registry {
    pub custom_tags: Vec<String>,
    pub assignments: BTreeMap<String, Vec<String>>,
    #[serde(skip)]
    pub editing: Option<String>,
    #[serde(skip)]
    pub selected: Vec<String>,
    #[serde(skip)]
    pub new_tag: String,
    #[serde(skip)]
    pub filter: String,
    #[serde(skip)]
    pub error: String,
}
impl Registry {
    fn path() -> std::path::PathBuf {
        storage::data_dir().join("asset-tags.json")
    }
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&mut self) {
        match serde_json::to_vec_pretty(self)
            .map_err(|e| e.to_string())
            .and_then(|b| {
                engine_workspace::storage::write_atomic(&Self::path(), &b)
                    .map_err(|e| e.to_string())
            }) {
            Ok(()) => self.error.clear(),
            Err(e) => self.error = format!("Could not save asset tags: {e}"),
        }
    }
    pub fn all_tags(&self) -> Vec<String> {
        let mut result: Vec<String> = DEFAULT_TAGS.iter().map(|s| s.to_string()).collect();
        for tag in self
            .custom_tags
            .iter()
            .chain(self.assignments.values().flatten())
        {
            if !result.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                result.push(tag.clone());
            }
        }
        result.sort_by_key(|s| s.to_lowercase());
        result
    }
    pub fn get(&self, id: &str) -> Vec<String> {
        self.assignments.get(id).cloned().unwrap_or_default()
    }
    pub fn assign(&mut self, id: String, tags: Vec<String>) {
        let mut clean: Vec<String> = Vec::new();
        for tag in tags {
            let tag = tag.trim();
            if !tag.is_empty()
                && tag.len() <= 80
                && !clean.iter().any(|t| t.eq_ignore_ascii_case(tag))
            {
                clean.push(tag.to_owned());
            }
        }
        self.assignments.insert(id, clean);
        self.save();
    }
    pub fn import_zip(&mut self, id: &str, old_tags: &[String]) {
        let key = format!("zip:{id}");
        if !self.assignments.contains_key(&key) && !old_tags.is_empty() {
            self.assign(key, old_tags.to_vec());
        }
    }
    pub fn edit(&mut self, id: String) {
        self.selected = self.get(&id);
        self.editing = Some(id);
        self.new_tag.clear();
    }
    pub fn popup(&mut self, ctx: &egui::Context) {
        let Some(id) = self.editing.clone() else {
            return;
        };
        let mut save = false;
        let mut close = false;
        egui::Window::new("Add / Edit Tags")
            .id(egui::Id::new("shared-asset-tag-editor"))
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label("Local tags only — never uploaded to Fab.");
                egui::ComboBox::from_id_salt("shared-tag-selector")
                    .selected_text("Select existing tag…")
                    .show_ui(ui, |ui| {
                        for tag in self.all_tags() {
                            let has = self.selected.iter().any(|t| t.eq_ignore_ascii_case(&tag));
                            if ui.selectable_label(has, &tag).clicked() {
                                if has {
                                    self.selected.retain(|t| !t.eq_ignore_ascii_case(&tag));
                                } else {
                                    self.selected.push(tag);
                                }
                            }
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    let mut removed = None;
                    for tag in &self.selected {
                        if ui.small_button(format!("{tag} ×")).clicked() {
                            removed = Some(tag.clone());
                        }
                    }
                    if let Some(tag) = removed {
                        self.selected.retain(|t| t != &tag);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("New custom tag:");
                    ui.text_edit_singleline(&mut self.new_tag);
                    if ui.button("Add").clicked() {
                        let name = self.new_tag.trim();
                        if !name.is_empty() && name.len() <= 80 {
                            let name = name.to_owned();
                            if !self
                                .all_tags()
                                .iter()
                                .any(|t| t.eq_ignore_ascii_case(&name))
                            {
                                self.custom_tags.push(name.clone());
                            }
                            if !self.selected.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
                                self.selected.push(name);
                            }
                            self.new_tag.clear();
                        }
                    }
                });
                if !self.error.is_empty() {
                    ui.colored_label(egui::Color32::RED, &self.error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Save Tags").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if save {
            self.assign(id, self.selected.clone());
            close = true;
        }
        if close {
            self.editing = None;
        }
    }
}
