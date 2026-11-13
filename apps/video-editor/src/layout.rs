//! The editor's spacing steps and the bands of a track head and master strip,
//! shared by painting and hit-testing. All are unscaled reference dimensions;
//! callers multiply by the theme scale.

use ui::tokens::metrics::Metrics;
use ui::tokens::spacing::Spacing;
use vello::kurbo::Rect;

/// `spacing.s1`: a label and its control, lanes, the pills on a track head.
pub(crate) const S1: f64 = Spacing::BASE.s1 as f64;
/// `spacing.s2`: rows that read as one group; a row's inner inset.
pub(crate) const S2: f64 = Spacing::BASE.s2 as f64;
/// `spacing.s3`: siblings of equal weight, and a pane's content inset.
pub(crate) const S3: f64 = Spacing::BASE.s3 as f64;
/// `spacing.s4`: section padding.
pub(crate) const S4: f64 = Spacing::BASE.s4 as f64;
/// `spacing.s6`: the content gutter.
pub(crate) const S6: f64 = Spacing::BASE.s6 as f64;

pub(crate) const HEAD_PILL: f64 = Metrics::BASE.icon_md as f64;
/// A 9 px line box: the head's kind line, "VIDEO", "AUDIO · +4.5 dB".
pub(crate) const HEAD_KIND_H: f64 = S3;
pub(crate) const METER_H: f64 = HEAD_KIND_H / 2.0;
/// The shortest lane that fits a head with every band clear of the next.
pub(crate) const MIN_LANE_H: f64 = S1 + HEAD_PILL + S1 + HEAD_KIND_H + S1 + METER_H + S1;

/// Where a track head's parts go, built once and used by both the paint and the
/// hit-test, so the pill a pointer lands on is the pill that was drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackHead {
    /// The gutter cell, `S1` clear of the lane stripe beside it.
    pub card: Rect,
    /// The band the three pills share; a long name is elided before the pills.
    pub name: Rect,
    pub mute: Rect,
    pub lock: Rect,
    pub solo: Rect,
    pub kind: Rect,
    pub meter: Rect,
}

pub(crate) fn track_head_card(gutter: Rect, lane: &Rect) -> Rect {
    Rect::new(gutter.x0, lane.y0, gutter.x1 - S1, lane.y1)
}

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

/// The master strip's two bands, centred as one block in the strip.
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
