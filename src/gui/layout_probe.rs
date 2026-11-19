//! Test-only helpers that draw egui frames headless and look at what was
//! painted: where every piece of text landed. Used by the layout tests of the
//! top bar, the player and the scene deck to prove that nothing overlaps or
//! leaves its container at any window width.

use egui::{Context, FullOutput, Rect, Shape};

/// One painted piece of text.
#[derive(Debug, Clone)]
pub struct TextBox {
    /// Where the glyphs are (before clipping).
    pub rect: Rect,
    /// What it says, upper-cased (the console tracks its labels in capitals).
    pub text: String,
    /// The clip rectangle it was painted under.
    pub clip: Rect,
}

/// Every piece of text painted in `output`, in paint order.
pub fn text_boxes(output: &FullOutput) -> Vec<TextBox> {
    fn walk(shape: &Shape, clip: Rect, out: &mut Vec<TextBox>) {
        match shape {
            Shape::Text(text) => {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                if !text.galley.text().trim().is_empty() {
                    out.push(TextBox {
                        rect,
                        text: text.galley.text().to_uppercase(),
                        clip,
                    });
                }
            }
            Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, clip, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, clipped.clip_rect, &mut out);
    }
    out
}

/// Run `frames` frames of `run` on a `size` screen and return the last output.
/// Two frames let egui settle sizes that depend on the previous frame.
pub fn run_frames(
    ctx: &Context,
    size: egui::Vec2,
    frames: usize,
    mut run: impl FnMut(&Context),
) -> FullOutput {
    let mut last = None;
    for n in 0..frames {
        let input = egui::RawInput {
            time: Some(n as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        last = Some(ctx.run(input, &mut run));
    }
    last.expect("at least one frame")
}

/// [`run_frames`] at `pixels_per_point`: the screen is `size` points.
pub fn run_frames_at(
    ctx: &Context,
    size: egui::Vec2,
    pixels_per_point: f32,
    frames: usize,
    mut run: impl FnMut(&Context),
) -> FullOutput {
    let mut last = None;
    for n in 0..frames {
        let mut input = egui::RawInput {
            time: Some(n as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let info = egui::ViewportInfo {
            native_pixels_per_point: Some(pixels_per_point),
            ..Default::default()
        };
        input.viewports.insert(egui::ViewportId::ROOT, info);
        ctx.set_pixels_per_point(pixels_per_point);
        last = Some(ctx.run(input, &mut run));
    }
    last.expect("at least one frame")
}

/// Panics when two texts overlap by more than a hair, or when one of them
/// leaves `bounds`. `what` names the scene in the failure message.
pub fn assert_clean(boxes: &[TextBox], bounds: Rect, what: &str) {
    // Glyph boxes of neighbours may touch; real overlap is deeper than this.
    const SLACK: f32 = 0.5;
    for (i, a) in boxes.iter().enumerate() {
        assert!(
            a.rect.left() >= bounds.left() - SLACK
                && a.rect.right() <= bounds.right() + SLACK
                && a.rect.top() >= bounds.top() - SLACK
                && a.rect.bottom() <= bounds.bottom() + SLACK,
            "{what}: {:?} at {:?} leaves {:?}",
            a.text,
            a.rect,
            bounds
        );
        for b in &boxes[i + 1..] {
            let shared = a.rect.intersect(b.rect);
            assert!(
                shared.width() <= SLACK || shared.height() <= SLACK,
                "{what}: {:?} {:?} overlaps {:?} {:?}",
                a.text,
                a.rect,
                b.text,
                b.rect
            );
        }
    }
}
