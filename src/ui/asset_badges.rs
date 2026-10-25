//! Shared, vector Library type badges. Icons are tinted directly; only archive zippers use amber.
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};

#[derive(Clone, Copy)]
pub(super) enum BadgeState {
    Present,
    Verified,
    Checking,
    Failed,
    Missing,
}

impl BadgeState {
    pub(super) fn color(self) -> Color32 {
        match self {
            Self::Present => Color32::from_rgb(37, 117, 71),
            Self::Verified => Color32::from_rgb(108, 224, 141),
            Self::Checking => Color32::from_rgb(55, 150, 245),
            Self::Failed => Color32::from_rgb(230, 64, 75),
            Self::Missing => Color32::from_rgb(155, 166, 181),
        }
    }
}

fn segment(p: &Painter, a: Pos2, b: Pos2, color: Color32, width: f32) {
    p.line_segment([a, b], Stroke::new(width, color));
}

/// Draw the common Project/Plugin/Asset badge at the requested size.
/// ZIP archives receive a separate contrasting, amber-trimmed zipper and all
/// badges receive a shape-distinct status marker, without any backdrop glow.
pub(super) fn paint(p: &Painter, bounds: Rect, kind: &str, archive: bool, state: BadgeState) {
    paint_tinted(p, bounds, kind, archive, state, state.color());
}

/// Paint the *complete* labelled library type badge using the existing vector
/// artwork. This presentation is intentionally source-neutral: `archive` changes
/// only the amber zipper overlay, never the Project/Asset/Plugin geometry.
pub(super) fn paint_labelled(
    ui: &egui::Ui,
    bounds: Rect,
    kind: &str,
    archive: bool,
    state: BadgeState,
    tint: Color32,
    border: Color32,
    text_color: Color32,
) {
    ui.painter().rect_stroke(bounds, 5., Stroke::new(1., border), StrokeKind::Inside);
    paint_tinted(
        ui.painter(),
        Rect::from_min_size(bounds.min + vec2(10., 5.5), vec2(19., 19.)),
        kind,
        archive,
        state,
        tint,
    );
    ui.painter().text(
        bounds.min + vec2(33., 15.),
        egui::Align2::LEFT_CENTER,
        kind,
        egui::FontId::proportional(14.),
        text_color,
    );
}

/// Preserve the existing Fab/Vault health colour while using the shared artwork.
pub(super) fn paint_tinted(
    p: &Painter,
    bounds: Rect,
    kind: &str,
    archive: bool,
    state: BadgeState,
    tint: Color32,
) {
    let s = bounds.width().min(bounds.height());
    let origin = bounds.center() - vec2(s * 0.5, s * 0.5);
    let at = |x: f32, y: f32| origin + vec2(x * s, y * s);
    let stroke = Stroke::new((s * 0.047).max(1.), tint);
    let fill = Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 52);
    if kind == "Plugin" {
        let rect = Rect::from_min_max(at(0.16, 0.14), at(0.76, 0.80));
        p.rect_filled(rect, s * 0.07, fill);
        p.rect_stroke(rect, s * 0.07, stroke, StrokeKind::Inside);
        for y in [0.31, 0.51, 0.71] {
            segment(p, at(0.08, y), at(0.16, y), tint, s * 0.07);
        }
        p.circle_stroke(at(0.41, 0.36), s * 0.09, stroke);
        segment(p, at(0.41, 0.45), at(0.41, 0.59), tint, s * 0.047);
        p.circle_filled(at(0.41, 0.64), s * 0.09, tint);
    } else if kind == "Project" {
        let points = vec![
            at(0.09, 0.29),
            at(0.09, 0.17),
            at(0.37, 0.17),
            at(0.46, 0.29),
            at(0.79, 0.29),
            at(0.79, 0.81),
            at(0.09, 0.81),
        ];
        p.add(Shape::convex_polygon(points, fill, stroke));
        p.circle_stroke(at(0.42, 0.52), s * 0.16, Stroke::new(s * 0.045, tint));
        p.text(
            at(0.42, 0.52),
            egui::Align2::CENTER_CENTER,
            "U",
            egui::FontId::proportional(s * 0.28),
            tint,
        );
    } else {
        // Compact, shaded crate silhouette with defined parcel edges.
        let points = vec![
            at(0.12, 0.31),
            at(0.47, 0.12),
            at(0.80, 0.31),
            at(0.80, 0.72),
            at(0.46, 0.89),
            at(0.12, 0.72),
        ];
        p.add(Shape::convex_polygon(points, fill, stroke));
        for (a, b) in [
            ((0.12, 0.31), (0.46, 0.48)),
            ((0.46, 0.48), (0.80, 0.31)),
            ((0.46, 0.48), (0.46, 0.89)),
            ((0.46, 0.12), (0.46, 0.48)),
        ] {
            segment(p, at(a.0, a.1), at(b.0, b.1), tint, s * 0.044);
        }
    }
    if archive {
        // Contrast belongs to zipper alone, NOT the icon background.
        let x = 0.73;
        let gold = Color32::from_rgb(245, 172, 58);
        let metal = Color32::from_rgb(247, 234, 191);
        segment(p, at(x, 0.19), at(x, 0.73), gold, s * 0.14);
        segment(
            p,
            at(x, 0.19),
            at(x, 0.73),
            Color32::from_rgb(47, 43, 53),
            s * 0.092,
        );
        for i in 0..9 {
            let y = 0.23 + i as f32 * 0.049;
            let side = if i % 2 == 0 { -0.04 } else { 0.04 };
            segment(p, at(x, y), at(x + side, y + 0.012), metal, s * 0.034);
        }
        p.rect_filled(
            Rect::from_center_size(at(x, 0.68), vec2(s * 0.14, s * 0.19)),
            s * 0.032,
            metal,
        );
        p.circle_stroke(at(x, 0.79), s * 0.055, Stroke::new(s * 0.036, gold));
    }
    // Independent marker; never use an animated verifying indicator.
    let center = at(0.82, 0.84);
    let r = s * 0.145;
    let border = Stroke::new((s * 0.06).max(1.), tint);
    match state {
        BadgeState::Present => {
            p.circle_stroke(center, r, border);
        }
        BadgeState::Verified => {
            p.circle_stroke(center, r, border);
            segment(
                p,
                center + vec2(-r * 0.55, 0.),
                center + vec2(-r * 0.12, r * 0.44),
                tint,
                s * 0.065,
            );
            segment(
                p,
                center + vec2(-r * 0.12, r * 0.44),
                center + vec2(r * 0.7, -r * 0.55),
                tint,
                s * 0.065,
            );
        }
        BadgeState::Checking | BadgeState::Missing => {
            for n in 0..7 {
                let a = n as f32 * std::f32::consts::TAU / 9.;
                let b = a + std::f32::consts::TAU / 13.;
                segment(
                    p,
                    center + vec2(a.cos() * r, a.sin() * r),
                    center + vec2(b.cos() * r, b.sin() * r),
                    tint,
                    s * 0.055,
                );
            }
        }
        BadgeState::Failed => {
            p.circle_stroke(center, r, border);
            let q = r * 0.46;
            segment(
                p,
                center + vec2(-q, -q),
                center + vec2(q, q),
                tint,
                s * 0.064,
            );
            segment(
                p,
                center + vec2(-q, q),
                center + vec2(q, -q),
                tint,
                s * 0.064,
            );
        }
    }
}
