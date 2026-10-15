//! One spacing rhythm for the editor's own chrome (HIG §7b), and the measured
//! bands of the surfaces that used to be laid out from hand-tuned offsets.
//!
//! Every gap here is a step on the spacing scale, read from the unscaled
//! reference [`Spacing::BASE`] rather than restated as a number, so the same
//! relationship always gets the same step: `S1` between a label and its
//! control, between lanes and between the pills on a track head; `S2`
//! between related rows (the bin's list, the two add-track buttons); `S3`
//! between siblings of equal weight (the topbar commands, a pane's content
//! inset). A track head is the one place the old offsets produced a touch —
//! the kind line sat a pixel or two off the level meter on a 46 px audio
//! lane — so its bands are modelled here and a test asserts they clear.
//!
//! These are reference dimensions. The editor caches the effective theme
//! during layout and scales these bands for both painting and hit-testing.

use ui::tokens::metrics::Metrics;
use ui::tokens::spacing::Spacing;
use vello::kurbo::Rect;

/// `spacing.s1`: label ↔ its control, lane ↔ lane, pill ↔ pill, the inset a
/// track head keeps from its lane stripe.
pub(crate) const S1: f64 = Spacing::BASE.s1 as f64;
/// `spacing.s2`: related rows that read as one group — the bin's list rows,
/// the two add-track buttons, a row's inner inset.
pub(crate) const S2: f64 = Spacing::BASE.s2 as f64;
/// `spacing.s3`: siblings of equal weight — the topbar commands — and the
/// content inset of every pane.
pub(crate) const S3: f64 = Spacing::BASE.s3 as f64;
/// `spacing.s4`: section padding; the master strip's readout band.
pub(crate) const S4: f64 = Spacing::BASE.s4 as f64;
/// `spacing.s6`: the content gutter; the room a message keeps from the edges
/// of the canvas it is written on.
pub(crate) const S6: f64 = Spacing::BASE.s6 as f64;

/// The square a track head's mute / lock / solo toggle occupies: the
/// toolkit's medium icon size, which is also the band the lane's name is
/// centred on.
pub(crate) const HEAD_PILL: f64 = Metrics::BASE.icon_md as f64;
/// The band a head's kind line ("VIDEO", "AUDIO · +4.5 dB") occupies: a
/// 9 px line box fits in one `s3` step.
pub(crate) const HEAD_KIND_H: f64 = S3;
/// A level meter's thickness, in the head and the master strip: half a kind
/// line, so the two bars it holds stay legible without taking a text row.
pub(crate) const METER_H: f64 = HEAD_KIND_H / 2.0;
/// The shortest lane a head fits in with every band clear of the next:
/// inset, name band, gap, kind line, gap, meter, inset.
pub(crate) const MIN_LANE_H: f64 = S1 + HEAD_PILL + S1 + HEAD_KIND_H + S1 + METER_H + S1;

/// Where a track head's parts go. Built from the head's card by
/// [`track_head`], and read by both the paint and the hit-test so the pill
/// a pointer lands on is the pill that was drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackHead {
    /// The head's own surface: the gutter cell, inset `S1` from the lane
    /// stripe beside it so the two never share an edge.
    pub card: Rect,
    /// The lane's name: the band the three pills share, centred on one line,
    /// ending a step short of the first pill so a long name is elided to it
    /// rather than painted under them.
    pub name: Rect,
    pub mute: Rect,
    pub lock: Rect,
    pub solo: Rect,
    /// What the lane is and where its fader sits.
    pub kind: Rect,
    /// The lane's level, along the foot of the head.
    pub meter: Rect,
}

/// The head's card for `lane` inside `gutter`: full gutter width less the
/// `S1` that keeps it clear of the lane stripe.
pub(crate) fn track_head_card(gutter: Rect, lane: &Rect) -> Rect {
    Rect::new(gutter.x0, lane.y0, gutter.x1 - S1, lane.y1)
}

/// Lay a track head out inside `card`.
pub(crate) fn track_head(card: Rect) -> TrackHead {
    let x0 = card.x0 + S3;
    let x1 = card.x1 - S2;
    let top = card.y0 + S1;
    let mute = Rect::new(x1 - HEAD_PILL, top, x1, top + HEAD_PILL);
    let lock = Rect::new(mute.x0 - S1 - HEAD_PILL, top, mute.x0 - S1, top + HEAD_PILL);
    let solo = Rect::new(lock.x0 - S1 - HEAD_PILL, top, lock.x0 - S1, top + HEAD_PILL);
    let name = Rect::new(x0, top, (solo.x0 - S1).max(x0), top + HEAD_PILL);
    let kind = Rect::new(x0, name.y1 + S1, x1, name.y1 + S1 + HEAD_KIND_H);
    let meter = Rect::new(x0, card.y1 - S1 - METER_H, x1, card.y1 - S1);
    TrackHead {
        card,
        name,
        mute,
        lock,
        solo,
        kind,
        meter,
    }
}

/// The master strip's two bands — the readout line (`MIX` and the peak or
/// the standing CLIP flag) and the master meter under it — centred as one
/// block in the strip, `S1` apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MasterStrip {
    pub readout: Rect,
    pub meter: Rect,
}

pub(crate) fn master_strip(strip: Rect, scale: f64) -> MasterStrip {
    let x0 = strip.x0 + S3 * scale;
    let x1 = strip.x1 - S2 * scale;
    let block = (S4 + S1 + METER_H) * scale;
    let top = strip.y0 + ((strip.height() - block) / 2.0).max(0.0);
    MasterStrip {
        readout: Rect::new(x0, top, x1, top + S4 * scale),
        meter: Rect::new(x0, top + (S4 + S1) * scale, x1, top + block),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUTTER: Rect = Rect::new(0.0, 30.0, 128.0, 320.0);

    fn head_for(lane_h: f64) -> TrackHead {
        let lane = Rect::new(128.0, 34.0, 1280.0, 34.0 + lane_h);
        track_head(track_head_card(GUTTER, &lane))
    }

    #[test]
    fn lane_heads_and_master_strip_keep_token_gaps() {
        // Case `a_head_keeps_a_token_gap_from_its_lane_stripe`.
        {
            let lane = Rect::new(128.0, 34.0, 1280.0, 96.0);
            let card = track_head_card(GUTTER, &lane);
            assert!(
                ((lane.x0 - card.x1) - (S1)).abs() < 1e-9,
                "head ↔ lane stripe"
            );
            assert!(
                ((card.y0) - (lane.y0)).abs() < 1e-9,
                "card.y0 is {}, want {}",
                card.y0,
                lane.y0
            );
            assert!(
                ((card.y1) - (lane.y1)).abs() < 1e-9,
                "card.y1 is {}, want {}",
                card.y1,
                lane.y1
            );
        }
        // Case `the_pills_sit_one_step_apart_on_the_name_band`.
        {
            let h = head_for(62.0);
            assert!(((h.lock.x0 - h.solo.x1) - (S1)).abs() < 1e-9, "solo ↔ lock");
            assert!(((h.mute.x0 - h.lock.x1) - (S1)).abs() < 1e-9, "lock ↔ mute");
            assert!(
                ((h.card.x1 - h.mute.x1) - (S2)).abs() < 1e-9,
                "mute ↔ card edge"
            );
            for pill in [h.mute, h.lock, h.solo] {
                assert!(
                    ((pill.width()) - (HEAD_PILL)).abs() < 1e-9,
                    "pill.width() is {}, want {}",
                    pill.width(),
                    HEAD_PILL
                );
                assert!(
                    ((pill.height()) - (HEAD_PILL)).abs() < 1e-9,
                    "pill.height() is {}, want {}",
                    pill.height(),
                    HEAD_PILL
                );
                assert!(
                    ((pill.y0) - (h.name.y0)).abs() < 1e-9,
                    "pills share the name band"
                );
                assert!(
                    ((pill.y1) - (h.name.y1)).abs() < 1e-9,
                    "pill.y1 is {}, want {}",
                    pill.y1,
                    h.name.y1
                );
            }
            assert!(
                ((h.solo.x0 - h.name.x1) - (S1)).abs() < 1e-9,
                "name ↔ first pill"
            );
            assert!(h.name.width() > 0.0, "a 128 px gutter leaves the name room");
        }
        // Case `every_band_clears_the_next_on_the_shortest_lane`.
        {
            for lane_h in [MIN_LANE_H, 62.0] {
                let h = head_for(lane_h);
                assert!(
                    h.kind.y0 - h.name.y1 >= S1,
                    "{lane_h}: name ↔ kind gap {}",
                    h.kind.y0 - h.name.y1
                );
                assert!(
                    h.meter.y0 - h.kind.y1 >= S1,
                    "{lane_h}: kind ↔ meter gap {}",
                    h.meter.y0 - h.kind.y1
                );
                assert!(h.name.y0 - h.card.y0 >= S1, "{lane_h}: top inset");
                assert!(h.card.y1 - h.meter.y1 >= S1, "{lane_h}: bottom inset");
            }
        }
        // Case `the_master_strip_centres_its_two_bands_a_step_apart`.
        {
            let strip = Rect::new(0.0, 0.0, 128.0, 30.0);
            let m = master_strip(strip, 1.0);
            assert!(
                ((m.meter.y0 - m.readout.y1) - (S1)).abs() < 1e-9,
                "readout ↔ meter"
            );
            assert!(m.readout.y0 >= strip.y0);
            assert!(m.meter.y1 <= strip.y1);
            let top = m.readout.y0 - strip.y0;
            let bottom = strip.y1 - m.meter.y1;
            assert!((top - bottom).abs() < 1e-9, "centred: {top} vs {bottom}");
        }
    }
}
