//! Width budget of the library toolbar.
//!
//! A pure function of the available width, so "nothing leaves the panel" is
//! tested over a whole range of widths without opening a window.

use crate::gui::theme::Metrics;

/// Preferred widths of `Folder…`, `Export…`, `Import…`.
pub const BTN_W: [f32; 3] = [112.0, 84.0, 84.0];
/// Narrowest widths of the same buttons.
pub const BTN_MIN_W: [f32; 3] = [92.0, 68.0, 68.0];
/// Narrowest search field.
pub const SEARCH_MIN_W: f32 = 120.0;
/// Width of the "clear search" button.
pub const CLEAR_W: f32 = 26.0;
/// Slack kept on every row so a rounding error never wraps the last widget.
pub const SLACK: f32 = 1.0;

/// Where the search field and the three buttons go.
///
/// The toolbar is a wrapped row: when the buttons do not fit next to a field
/// of at least [`SEARCH_MIN_W`], the field takes a row of its own and the
/// buttons wrap to the next one (shrinking toward their minimum when even
/// that row is too short).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchLayout {
    /// Width of the search field.
    pub field_w: f32,
    /// Widths of `Folder…`, `Export…`, `Import…`.
    pub button_w: [f32; 3],
    /// The buttons sit on their own row below the field.
    pub stacked: bool,
}

impl SearchLayout {
    /// Width of the row that holds the field (and the clear button).
    #[cfg(test)]
    pub fn field_row_w(&self, has_text: bool) -> f32 {
        self.field_w + if has_text { Metrics::SM + CLEAR_W } else { 0.0 }
    }

    /// Width of the row of buttons, gaps included.
    #[cfg(test)]
    pub fn buttons_w(&self) -> f32 {
        self.button_w.iter().sum::<f32>() + 2.0 * Metrics::SM
    }
}

/// Lay the search row out in `available` pixels.
pub fn search_layout(available: f32, has_text: bool) -> SearchLayout {
    let usable = (available - SLACK).max(0.0);
    let clear = if has_text { Metrics::SM + CLEAR_W } else { 0.0 };
    let gap = Metrics::SM;
    let nominal = BTN_W.iter().sum::<f32>() + 2.0 * gap;

    let beside = usable - clear - gap - nominal;
    if beside >= SEARCH_MIN_W {
        return SearchLayout {
            field_w: beside,
            button_w: BTN_W,
            stacked: false,
        };
    }

    // Own row for the field; the buttons shrink to fit the next one.
    let field_w = (usable - clear).max(SEARCH_MIN_W.min(usable));
    let min_total = BTN_MIN_W.iter().sum::<f32>() + 2.0 * gap;
    let button_w = if usable >= nominal {
        BTN_W
    } else if usable <= min_total {
        BTN_MIN_W
    } else {
        // Share the shrink in proportion to how much each can give.
        let give: Vec<f32> = BTN_W
            .iter()
            .zip(BTN_MIN_W.iter())
            .map(|(w, m)| w - m)
            .collect();
        let total_give: f32 = give.iter().sum();
        let missing = nominal - usable;
        let mut out = BTN_W;
        for (w, g) in out.iter_mut().zip(give) {
            *w -= missing * g / total_give;
        }
        out
    };
    SearchLayout {
        field_w,
        button_w,
        stacked: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths() -> impl Iterator<Item = f32> {
        (36..=160).map(|n| n as f32 * 10.0)
    }

    #[test]
    fn every_row_fits_at_every_width() {
        for has_text in [false, true] {
            for width in widths() {
                let lay = search_layout(width, has_text);
                assert!(
                    lay.field_row_w(has_text) <= width + 0.001,
                    "field row at {width}: {lay:?}"
                );
                assert!(
                    lay.buttons_w() <= width + 0.001,
                    "button row at {width}: {lay:?}"
                );
                if !lay.stacked {
                    // One row: field and buttons share it.
                    assert!(
                        lay.field_row_w(has_text) + Metrics::SM + lay.buttons_w() <= width + 0.001,
                        "single row at {width}: {lay:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn minimums_hold() {
        for width in widths() {
            let lay = search_layout(width, true);
            assert!(lay.field_w >= SEARCH_MIN_W - 0.001, "field at {width}");
            for (w, min) in lay.button_w.iter().zip(BTN_MIN_W) {
                assert!(*w >= min - 0.001, "button at {width}");
            }
            for (w, max) in lay.button_w.iter().zip(BTN_W) {
                assert!(*w <= max + 0.001);
            }
        }
    }

    #[test]
    fn buttons_wrap_below_the_field_when_they_do_not_fit_beside_it() {
        assert!(!search_layout(1600.0, false).stacked);
        assert!(search_layout(400.0, false).stacked);
        // Once stacked, going narrower never un-stacks.
        let mut stacked_seen = false;
        let mut width = 1600.0;
        while width >= 360.0 {
            let stacked = search_layout(width, true).stacked;
            if stacked_seen {
                assert!(stacked, "un-stacked at {width}");
            }
            stacked_seen |= stacked;
            width -= 1.0;
        }
        assert!(stacked_seen);
    }

    #[test]
    fn field_stretches_with_the_width() {
        let wide = search_layout(1200.0, false);
        let narrower = search_layout(900.0, false);
        assert!(wide.field_w > narrower.field_w);
        assert_eq!(wide.button_w, BTN_W);
    }

    #[test]
    fn buttons_shrink_before_they_overflow() {
        // 270 is between the minimum (244) and the nominal (296) button rows.
        let lay = search_layout(270.0, false);
        assert!(lay.stacked);
        assert!(lay.buttons_w() <= 270.0);
        assert!(lay.button_w[0] < BTN_W[0]);
    }
}
