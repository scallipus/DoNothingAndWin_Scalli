//! Shared Fab/Custom ZIP view-mode toolbar. Preserves the original Fab icon painter,
//! themed active/inactive states, hover behaviour and high-DPI scaling.
use super::*;

pub(super) fn draw(ui: &mut Ui, c: &Canvas, list_view: &mut bool) {
    let theme = c.theme.clone();
    // Keep the visual order Grid, List even inside the ZIP right-aligned toolbar.
    let modes = if ui.layout().main_dir == egui::Direction::RightToLeft {
        [(true, Icon::List), (false, Icon::Grid)]
    } else {
        [(false, Icon::Grid), (true, Icon::List)]
    };
    for (list, icon) in modes {
                    let (r, response) =
                        ui.allocate_exact_size(Vec2::splat(COMPACT_BUTTON_HEIGHT) * c.scale(), Sense::click());
                    ui.painter().rect_filled(
                        r,
                        BUTTON_RADIUS,
                        color(if *list_view == list {
                            if response.hovered() || response.is_pointer_button_down_on() {
                                theme.accent_hover
                            } else { theme.accent }
                        } else if response.hovered() {
                            SIDEBAR_HIGHLIGHT
                        } else {
                            theme.card
                        }),
                    );
                    let local = Canvas {
                        origin: r.min,
                        width: 32.,
                        height: 32.,
                        sx: c.scale(),
                        sy: c.scale(),
                        theme: theme.clone(),
                    };
                    local.icon(
                        ui,
                        icon,
                        (32. - VIEW_MODE_ICON_SIZE) / 2.,
                        (32. - VIEW_MODE_ICON_SIZE) / 2.,
                        VIEW_MODE_ICON_SIZE,
                        if *list_view == list {
                            theme.on_accent
                        } else if response.hovered() {
                            SIDEBAR_ON_HIGHLIGHT
                        } else {
                            theme.muted
                        },
                    );
                    ui.painter().rect_stroke(
                        r,
                        BUTTON_RADIUS,
                        Stroke::new(1_f32, color(theme.border)),
                        StrokeKind::Inside,
                    );
                    if response
                        .on_hover_text(if list { "List view" } else { "Grid view" })
                        .clone()
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .clicked()
                    {
                        *list_view = list;
                    }
    }
}
