//! Shared library thumbnail painting. Cover the requested image rectangle without
//! stretching: image centre is preserved and excess edges are cropped by UV.
//! Both library views own their thumbnail size, interaction and hover regions.
use eframe::egui::{self, Color32, CornerRadius, Rect, TextureHandle, Ui, Vec2};

pub(super) fn paint_contained_thumbnail(
    ui: &Ui,
    texture: &TextureHandle,
    viewport: Rect,
    corners: CornerRadius,
    background: Color32,
) {
    ui.painter().rect_filled(viewport, corners, background);
    let native = texture.size_vec2();
    if native.x <= 0.0_f32 || native.y <= 0.0_f32
        || viewport.width() <= 0.0_f32 || viewport.height() <= 0.0_f32
    {
        return;
    }
    // Cover, not contain: at least one dimension fills the whole viewport.
    // Cropping only the excess edges preserves the original image aspect ratio.
    let scale = (viewport.width() / native.x).max(viewport.height() / native.y);
    let visible_native = viewport.size() / scale;
    let uv_half = egui::vec2(
        (visible_native.x / native.x).min(1.0_f32) * 0.5_f32,
        (visible_native.y / native.y).min(1.0_f32) * 0.5_f32,
    );
    let uv = Rect::from_min_max(
        egui::pos2(0.5_f32 - uv_half.x, 0.5_f32 - uv_half.y),
        egui::pos2(0.5_f32 + uv_half.x, 0.5_f32 + uv_half.y),
    );
    ui.painter().add(
        egui::epaint::RectShape::filled(viewport, corners, Color32::WHITE)
            .with_texture(texture.id(), uv),
    );
}

/// Shared Fab / Custom ZIP grid-card width policy. A setting of zero
/// preserves the original proportional-card layout; otherwise the chosen
/// square thumbnail size drives the minimum width. The caller still owns
/// its existing interior padding and arrangement.
pub(super) fn preferred_card_width(chosen_thumbnail_size: u16, original_card_width: f32) -> f32 {
    if chosen_thumbnail_size == 0 {
        original_card_width
    } else {
        (f32::from(chosen_thumbnail_size) + 2.0_f32).max(original_card_width)
    }
}

/// Return the thumbnail viewport for the selected Settings size. Explicit
/// sizes are square; Original keeps the legacy source-specific card inset.
/// Centralizing this prevents the two library views from drifting on 64/128/
/// 192/256 while retaining their already-tested original-size layout.
pub(super) fn grid_thumbnail_viewport(
    chosen_thumbnail_size: u16,
    card_width: f32,
    original_horizontal_inset: f32,
    original_aspect_ratio: f32,
) -> Vec2 {
    if chosen_thumbnail_size != 0 {
        return Vec2::splat(f32::from(chosen_thumbnail_size));
    }
    let width = (card_width - original_horizontal_inset).max(0.0_f32);
    let height = if original_aspect_ratio > 0.0_f32 {
        width / original_aspect_ratio
    } else {
        0.0_f32
    };
    egui::vec2(width, height)
}

/// Shared dimensions for the complete outlined + labelled asset-type badge.
/// The vector drawings themselves remain authoritative in asset_badges.rs.
pub(super) fn labelled_badge_size() -> Vec2 {
    egui::vec2(86.0_f32, 30.0_f32)
}

/// Fixed-height ZIP grid title slot keeps the lower action row aligned
/// without exposing catalogue tags on the card.
pub(super) fn zip_grid_title_height() -> f32 {
    27.0_f32
}

#[cfg(test)]
mod geometry_tests {
    use super::{grid_thumbnail_viewport, preferred_card_width, labelled_badge_size, zip_grid_title_height};
    use eframe::egui::{self, Vec2};

    #[test]
    fn footer_metrics_are_stable() {
        assert_eq!(labelled_badge_size(), Vec2::new(86.0_f32, 30.0_f32));
        assert_eq!(zip_grid_title_height(), 27.0_f32);
    }

    #[test]
    fn fixed_settings_produce_equal_square_viewports() {
        for size in [64_u16, 128, 192, 256] {
            let fab = grid_thumbnail_viewport(size, 280.0_f32, 2.0_f32, 1.5_f32);
            let zip = grid_thumbnail_viewport(size, 280.0_f32, 16.0_f32, 1.5_f32);
            assert_eq!(fab, zip);
            assert_eq!(fab, Vec2::splat(f32::from(size)));
        }
    }

    #[test]
    fn original_mode_preserves_legacy_insets() {
        assert_eq!(grid_thumbnail_viewport(0, 242.0_f32, 2.0_f32, 1.5_f32), egui::vec2(240.0_f32, 160.0_f32));
        assert_eq!(grid_thumbnail_viewport(0, 242.0_f32, 16.0_f32, 2.0_f32), egui::vec2(226.0_f32, 113.0_f32));
    }

    #[test]
    fn original_setting_preserves_original_width() {
        assert_eq!(preferred_card_width(0, 240.8_f32), 240.8_f32);
    }

    #[test]
    fn shared_thumbnail_settings_preserve_existing_width_policy() {
        for size in [64_u16, 128, 192, 256] {
            assert_eq!(
                preferred_card_width(size, 240.8_f32),
                (f32::from(size) + 2.0_f32).max(240.8_f32),
            );
        }
    }
}
