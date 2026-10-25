//! Shared right-click actions for Fab and Custom ZIP library cards.
//! Only the presentation is shared: the caller dispatches each request using
//! its own authoritative integrity and catalogue implementation.
use eframe::egui::{self, Ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommonAction {
    VerifyIntegrity,
    EditTags,
    ToggleHidden,
}

/// Render common actions in consistent order for thumbnail and badge menus.
/// Omit operations which the caller cannot actually perform.
pub(super) fn common_actions(
    ui: &mut Ui,
    can_verify: bool,
    hidden: bool,
) -> Option<CommonAction> {
    if ui.add_enabled(can_verify, egui::Button::new("Verify Integrity…")).clicked() {
        ui.close();
        return Some(CommonAction::VerifyIntegrity);
    }
    if ui.button("Add / Edit Tags…").clicked() {
        ui.close();
        return Some(CommonAction::EditTags);
    }
    if ui.button(if hidden { "Unhide Asset" } else { "Hide Asset" }).clicked() {
        ui.close();
        return Some(CommonAction::ToggleHidden);
    }
    None
}
