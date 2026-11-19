//! Width budgets of the library panel.
//!
//! Everything here is a pure function of the width it is given, so the
//! "nothing leaves the panel" rule is tested over a whole range of widths
//! without opening a window. The drawing code in [`super`] places its pieces
//! exactly where these functions say.
//!
//! All x positions of a row are relative to the row's left edge.

use crate::gui::theme::Metrics;

/// A horizontal extent, relative to the left edge of its container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub left: f32,
    pub right: f32,
}

impl Span {
    pub fn new(left: f32, right: f32) -> Self {
        Self { left, right }
    }

    #[cfg(test)]
    pub fn width(&self) -> f32 {
        (self.right - self.left).max(0.0)
    }
}

// ------------------------------------------------------------- sound row ----

/// Left edge of the transport button.
pub const TRANSPORT_X: f32 = 16.0;
/// Left edge of the repeat toggle (transport 32 + gap 8).
pub const LOOP_X: f32 = TRANSPORT_X + Metrics::TRANSPORT + Metrics::SM;
/// Side of the small square buttons of a row.
pub const SMALL_BTN: f32 = 18.0;
/// Left edge of the headphones button (gap 6).
pub const PREVIEW_X: f32 = LOOP_X + SMALL_BTN + 6.0;
/// Where the name starts (after a 12px gap).
pub const TEXT_X: f32 = PREVIEW_X + SMALL_BTN + Metrics::MD;
/// Gap between the right edge of the row and the remove button.
pub const EDGE_PAD: f32 = 10.0;
/// Width of the remove button.
pub const REMOVE_W: f32 = SMALL_BTN;
/// Gap between the percent readout and the remove button.
pub const REMOVE_GAP: f32 = 6.0;
/// Fixed width of the percent readout: "100%" is wider than "70%", and a width
/// that follows the text shifted the slider from row to row.
pub const PCT_W: f32 = 34.0;
/// Preferred width of the volume slider.
pub const SLIDER_W: f32 = Metrics::ROW_SLIDER_W;
/// Narrowest volume slider.
pub const SLIDER_MIN_W: f32 = 40.0;
/// Narrowest group tag.
pub const TAG_MIN_W: f32 = 44.0;
/// Widest group tag.
pub const TAG_MAX_W: f32 = 112.0;
/// Gap between the name area and the first control on its right.
pub const NAME_GAP: f32 = Metrics::SM;
/// Narrowest name area a row accepts before it gives up the slider.
pub const NAME_MIN_W: f32 = 60.0;
/// Name width the group tag and the slider are not allowed to eat into.
pub const NAME_COMFORT_W: f32 = 96.0;
/// Name width at which the `48 kHz / 00:42 / WAV` line fits under the name.
pub const NAME_FOR_META_W: f32 = 150.0;
/// Group tag width assumed when a caller only knows the row width.
#[cfg(test)]
pub const TAG_NOMINAL_W: f32 = 80.0;

/// Which pieces of a sound row are drawn, and where.
///
/// Order of giving way when the row gets narrow: the time / frequency line,
/// then the group tag, then the slider shrinks to its minimum (and only
/// below that vanishes). The name always keeps what is left and is cut with
/// an ellipsis; the percent readout, the remove button and the transport
/// controls on the left always stay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowLayout {
    /// Width of the row this layout was made for.
    pub width: f32,
    /// Where the name (and the telemetry line under it) may be drawn.
    pub name: Span,
    /// Show `48 kHz / 00:42 / WAV` under the name.
    pub show_meta: bool,
    /// The group tag, when it fits.
    pub tag: Option<Span>,
    /// The volume slider, when there is room for one.
    pub slider: Option<Span>,
    /// The percent readout.
    pub percent: Span,
    /// The remove button.
    pub remove: Span,
}

/// [`row_layout_for`] with a typical group tag width.
#[cfg(test)]
pub fn row_layout(available_width: f32) -> RowLayout {
    row_layout_for(available_width, TAG_NOMINAL_W)
}

/// Decide what a sound row of `width` shows. `tag_w` is the width the group
/// tag would like (its text plus padding).
pub fn row_layout_for(width: f32, tag_w: f32) -> RowLayout {
    let tag_w = tag_w.clamp(TAG_MIN_W, TAG_MAX_W);
    let remove = Span::new(width - EDGE_PAD - REMOVE_W, width - EDGE_PAD);
    let percent = Span::new(remove.left - REMOVE_GAP - PCT_W, remove.left - REMOVE_GAP);
    // Everything optional lives between the name and the percent readout.
    let name_w = |slider: f32, tag: Option<f32>| -> f32 {
        let mut right = percent.left;
        if slider > 0.0 {
            right -= Metrics::SM + slider;
        }
        if let Some(tag) = tag {
            right -= Metrics::MD + tag;
        }
        right - NAME_GAP - TEXT_X
    };

    let mut slider = SLIDER_W;
    let mut tag = Some(tag_w);
    let mut show_meta = true;
    if name_w(slider, tag) < NAME_FOR_META_W {
        show_meta = false;
        if name_w(slider, tag) < NAME_COMFORT_W {
            tag = None;
            if name_w(slider, tag) < NAME_COMFORT_W {
                // Keep the name comfortable by shrinking the slider, but not
                // below its minimum.
                let room = percent.left - Metrics::SM - NAME_GAP - NAME_COMFORT_W - TEXT_X;
                slider = room.clamp(SLIDER_MIN_W, SLIDER_W);
                if name_w(slider, tag) < NAME_MIN_W {
                    slider = 0.0;
                }
            }
        }
    }

    // Lay the optional controls out from the right.
    let mut x = percent.left;
    let slider_span = (slider > 0.0).then(|| {
        let right = x - Metrics::SM;
        let span = Span::new(right - slider, right);
        x = span.left;
        span
    });
    let tag_span = tag.map(|tag| {
        let right = x - Metrics::MD;
        let span = Span::new(right - tag, right);
        x = span.left;
        span
    });
    let name = Span::new(TEXT_X, (x - NAME_GAP).max(TEXT_X));

    RowLayout {
        width,
        name,
        show_meta,
        tag: tag_span,
        slider: slider_span,
        percent,
        remove,
    }
}

// ----------------------------------------------------------- group rail ----

/// Narrowest the group rail may get before it is dropped.
pub const RAIL_MIN_W: f32 = 112.0;
/// Width the sound rows should keep when the rail has to shrink.
pub const ROWS_PREFERRED_W: f32 = 320.0;

/// Width of the group rail for a body that is `available` wide, or `None`
/// when the rail has to give way to the rows.
///
/// The rail never takes more than [`Metrics::RAIL_W`]; it shrinks toward
/// [`RAIL_MIN_W`] before the rows go below their preferred width, and it is
/// dropped only when the rows would fall under [`Metrics::MIN_ROW_W`].
pub fn rail_width(available: f32) -> Option<f32> {
    let room = available - Metrics::MD - ROWS_PREFERRED_W;
    let width = room.clamp(RAIL_MIN_W, Metrics::RAIL_W);
    (available - Metrics::MD - width >= Metrics::MIN_ROW_W).then_some(width)
}

/// How many characters of a rail label fit in a rail of `rail_w` (12px text
/// with the count badge on the right).
pub fn rail_label_chars(rail_w: f32) -> usize {
    (((rail_w - 10.0 - 34.0) / 6.6).floor() as i64).max(4) as usize
}

// --------------------------------------------------------- panel header ----

/// Preferred widths of the header buttons.
pub const ADD_BTN_W: f32 = 150.0;
pub const YOUTUBE_BTN_W: f32 = 110.0;
/// Narrowest widths of the header buttons.
pub const ADD_BTN_MIN_W: f32 = 110.0;
pub const YOUTUBE_BTN_MIN_W: f32 = 84.0;

/// Widths of the two header buttons next to the title block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeaderLayout {
    pub add_w: f32,
    /// `None` when there is no room for the YouTube button.
    pub youtube_w: Option<f32>,
}

/// Fit `Add Sound` (and `YouTube`) into `room`, the width left of the title
/// block. The YouTube button is dropped before `Add Sound` shrinks below its
/// minimum.
pub fn header_layout(room: f32) -> HeaderLayout {
    let gap = Metrics::SM;
    let both = ADD_BTN_W + gap + YOUTUBE_BTN_W;
    if room >= both {
        return HeaderLayout {
            add_w: ADD_BTN_W,
            youtube_w: Some(YOUTUBE_BTN_W),
        };
    }
    let min_both = ADD_BTN_MIN_W + gap + YOUTUBE_BTN_MIN_W;
    if room >= min_both {
        let extra = room - min_both;
        let add_extra = extra.min(ADD_BTN_W - ADD_BTN_MIN_W);
        let yt_extra = (extra - add_extra).min(YOUTUBE_BTN_W - YOUTUBE_BTN_MIN_W);
        return HeaderLayout {
            add_w: ADD_BTN_MIN_W + add_extra,
            youtube_w: Some(YOUTUBE_BTN_MIN_W + yt_extra),
        };
    }
    HeaderLayout {
        add_w: room.clamp(ADD_BTN_MIN_W.min(room.max(0.0)), ADD_BTN_W),
        youtube_w: None,
    }
}

// -------------------------------------------------------- master output ----

/// Preferred and narrowest widths of the master strip's controls.
pub const MASTER_SLIDER_PREF_W: f32 = 120.0;
pub const MASTER_SLIDER_MIN_W: f32 = 56.0;
pub const METER_PREF_W: f32 = 160.0;
pub const METER_MIN_W: f32 = 56.0;
pub const MUTE_W: f32 = 92.0;
pub const MUTE_MIN_W: f32 = 72.0;
pub const MASTER_PCT_W: f32 = 34.0;

/// Where the pieces of the `MASTER OUTPUT` strip go, left to right:
/// slider, level meter, percent readout, mute button.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MasterLayout {
    pub slider_w: f32,
    /// `None` when the meter is hidden for lack of room.
    pub meter_w: Option<f32>,
    pub mute_w: f32,
}

impl MasterLayout {
    /// Total width of the controls including the gaps between them.
    #[cfg(test)]
    pub fn total(&self) -> f32 {
        let mut total = self.mute_w + Metrics::SM + MASTER_PCT_W;
        if let Some(meter) = self.meter_w {
            total += Metrics::SM + meter + Metrics::MD;
        } else {
            total += Metrics::SM;
        }
        total + self.slider_w
    }
}

/// Fit the strip's controls into `room`, the width right of the caption.
///
/// The meter and the slider shrink first; the meter is hidden when even its
/// minimum does not fit; Mute stays (and only then narrows) so it can never
/// leave the panel.
pub fn master_layout(room: f32) -> MasterLayout {
    let sm = Metrics::SM;
    let md = Metrics::MD;
    // slider | md | meter | sm | percent | sm | mute
    let fixed_with_meter = md + sm + MASTER_PCT_W + sm + MUTE_W;
    let min_with_meter = MASTER_SLIDER_MIN_W + METER_MIN_W + fixed_with_meter;
    if room >= min_with_meter {
        let extra = room - min_with_meter;
        // The meter takes the first share of the spare room, the slider the
        // second, each up to its preferred width.
        let meter_extra = extra.min(METER_PREF_W - METER_MIN_W);
        let slider_extra = (extra - meter_extra).min(MASTER_SLIDER_PREF_W - MASTER_SLIDER_MIN_W);
        return MasterLayout {
            slider_w: MASTER_SLIDER_MIN_W + slider_extra,
            meter_w: Some(METER_MIN_W + meter_extra),
            mute_w: MUTE_W,
        };
    }
    // No meter: slider | sm | percent | sm | mute.
    let fixed = sm + MASTER_PCT_W + sm;
    let slider = room - fixed - MUTE_W;
    if slider >= MASTER_SLIDER_MIN_W {
        return MasterLayout {
            slider_w: slider.min(MASTER_SLIDER_PREF_W),
            meter_w: None,
            mute_w: MUTE_W,
        };
    }
    // Very narrow: Mute narrows, then the slider.
    let mute = (room - fixed - MASTER_SLIDER_MIN_W).clamp(MUTE_MIN_W, MUTE_W);
    let slider = (room - fixed - mute).max(0.0);
    MasterLayout {
        slider_w: slider.min(MASTER_SLIDER_MIN_W),
        meter_w: None,
        mute_w: mute,
    }
}

/// Number of meter cells for a meter of `width` (cells stay at least 3px).
pub fn meter_cells(width: f32) -> usize {
    ((width / 8.0).floor() as usize).clamp(6, 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Widths 360..=1600 in steps of 10.
    fn widths() -> impl Iterator<Item = f32> {
        (36..=160).map(|n| n as f32 * 10.0)
    }

    #[test]
    fn row_pieces_stay_inside_the_row_and_do_not_overlap() {
        for tag_w in [TAG_MIN_W, TAG_NOMINAL_W, TAG_MAX_W, 300.0] {
            for width in widths() {
                let lay = row_layout_for(width, tag_w);
                // Left to right: name, tag, slider, percent, remove.
                let mut pieces = vec![("name", lay.name)];
                if let Some(tag) = lay.tag {
                    pieces.push(("tag", tag));
                }
                if let Some(slider) = lay.slider {
                    pieces.push(("slider", slider));
                }
                pieces.push(("percent", lay.percent));
                pieces.push(("remove", lay.remove));
                let mut prev_right = TEXT_X;
                for (what, span) in &pieces {
                    assert!(
                        span.left >= prev_right - 0.001,
                        "{what} overlaps at {width}"
                    );
                    assert!(span.right >= span.left, "{what} inverted at {width}");
                    assert!(
                        span.right <= width - EDGE_PAD + 0.001,
                        "{what} leaves the row at {width}"
                    );
                    prev_right = span.right;
                }
                // The name keeps a gap to whatever follows.
                let next_left = pieces[1].1.left;
                assert!(lay.name.right + NAME_GAP <= next_left + 0.001);
            }
        }
    }

    #[test]
    fn row_minimums_hold() {
        for width in widths() {
            let lay = row_layout(width);
            assert!(
                lay.name.width() >= NAME_MIN_W,
                "name too narrow at {width}: {}",
                lay.name.width()
            );
            let slider = lay.slider.expect("a slider at every tested width");
            assert!(slider.width() >= SLIDER_MIN_W - 0.001);
            assert!(slider.width() <= SLIDER_W + 0.001);
            assert!((lay.percent.width() - PCT_W).abs() < 0.001);
            assert!((lay.remove.width() - REMOVE_W).abs() < 0.001);
            if let Some(tag) = lay.tag {
                assert!(tag.width() >= TAG_MIN_W - 0.001 && tag.width() <= TAG_MAX_W + 0.001);
            }
        }
    }

    #[test]
    fn row_hides_things_in_order() {
        // Walking from wide to narrow: once a piece is gone it stays gone, and
        // the pieces go in the documented order.
        let mut meta_gone = false;
        let mut tag_gone = false;
        let mut slider_shrunk = false;
        let mut width = 1600.0_f32;
        while width >= 240.0 {
            let lay = row_layout(width);
            let slider_w = lay.slider.map_or(0.0, |s| s.width());
            if !lay.show_meta {
                meta_gone = true;
            }
            if lay.tag.is_none() {
                assert!(
                    !lay.show_meta,
                    "tag hidden while meta still shown at {width}"
                );
                tag_gone = true;
            }
            if slider_w < SLIDER_W - 0.001 {
                assert!(
                    lay.tag.is_none(),
                    "slider shrank before the tag left at {width}"
                );
                slider_shrunk = true;
            }
            if meta_gone {
                assert!(!lay.show_meta, "meta came back at {width}");
            }
            if tag_gone {
                assert!(lay.tag.is_none(), "tag came back at {width}");
            }
            if slider_shrunk {
                assert!(slider_w < SLIDER_W - 0.001, "slider regrew at {width}");
            }
            width -= 1.0;
        }
        assert!(meta_gone && tag_gone && slider_shrunk);
    }

    #[test]
    fn roomy_row_shows_everything_and_a_tiny_one_keeps_the_essentials() {
        let wide = row_layout(1600.0);
        assert!(wide.show_meta && wide.tag.is_some());
        assert!((wide.slider.unwrap().width() - SLIDER_W).abs() < 0.001);
        // Far below the supported range nothing is inverted or negative.
        for width in [120.0, 200.0, 260.0] {
            let lay = row_layout(width);
            assert!(lay.name.right >= lay.name.left);
            assert!(!lay.show_meta && lay.tag.is_none());
        }
    }

    #[test]
    fn rail_leaves_the_rows_enough_room() {
        for width in widths() {
            let width = width - 100.0; // 260..1500: bodies, not windows
            match rail_width(width) {
                Some(rail) => {
                    assert!((RAIL_MIN_W..=Metrics::RAIL_W).contains(&rail));
                    assert!(width - Metrics::MD - rail >= Metrics::MIN_ROW_W - 0.001);
                }
                None => assert!(width - Metrics::MD - RAIL_MIN_W < Metrics::MIN_ROW_W),
            }
        }
        // The usual narrow console keeps the rail.
        assert!(rail_width(438.0).is_some());
        assert_eq!(rail_width(1000.0), Some(Metrics::RAIL_W));
        assert!(rail_width(300.0).is_none());
        assert!(rail_label_chars(112.0) >= 4);
        assert!(rail_label_chars(148.0) > rail_label_chars(112.0));
    }

    #[test]
    fn header_buttons_fit_their_room() {
        for room in (0..=700).step_by(5) {
            let room = room as f32;
            let lay = header_layout(room);
            let total = lay.add_w + lay.youtube_w.map_or(0.0, |w| w + Metrics::SM);
            if room >= ADD_BTN_MIN_W {
                assert!(total <= room + 0.001, "room {room}: {lay:?}");
            }
            if let Some(w) = lay.youtube_w {
                assert!(w >= YOUTUBE_BTN_MIN_W - 0.001);
            }
            assert!(lay.add_w <= ADD_BTN_W);
        }
        assert_eq!(header_layout(400.0).youtube_w, Some(YOUTUBE_BTN_W));
        assert_eq!(header_layout(150.0).youtube_w, None);
    }

    #[test]
    fn master_strip_fits_and_mute_stays() {
        // `label` stands in for the caption of the strip plus its gap.
        for label in [90.0, 130.0] {
            for width in widths() {
                let room = width - label;
                let lay = master_layout(room);
                assert!(lay.total() <= room + 0.001, "width {width}: {lay:?}");
                assert!(lay.mute_w >= MUTE_MIN_W - 0.001);
                assert!(lay.slider_w >= MASTER_SLIDER_MIN_W - 0.001);
                assert!(lay.slider_w <= MASTER_SLIDER_PREF_W + 0.001);
                if let Some(meter) = lay.meter_w {
                    assert!((METER_MIN_W - 0.001..=METER_PREF_W + 0.001).contains(&meter));
                }
            }
        }
    }

    #[test]
    fn master_meter_shrinks_then_hides_and_never_comes_back() {
        let mut hidden = false;
        let mut shrunk = false;
        let mut room = 900.0_f32;
        while room >= 150.0 {
            let lay = master_layout(room);
            match lay.meter_w {
                None => hidden = true,
                Some(meter) => {
                    assert!(!hidden, "meter came back at {room}");
                    if meter < METER_PREF_W - 0.001 {
                        shrunk = true;
                    }
                }
            }
            if shrunk {
                assert!(lay.slider_w <= MASTER_SLIDER_PREF_W + 0.001);
            }
            // Even without a meter the strip fits and Mute keeps its size.
            if room >= 150.0 {
                assert!(lay.total() <= room + 0.001, "room {room}: {lay:?}");
            }
            room -= 1.0;
        }
        assert!(shrunk && hidden);
        assert_eq!(master_layout(900.0).meter_w, Some(METER_PREF_W));
        assert!((6..=20).contains(&meter_cells(METER_MIN_W)));
        assert_eq!(meter_cells(METER_PREF_W), 20);
    }
}
