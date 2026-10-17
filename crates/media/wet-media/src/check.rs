//! A self-check that runs the simulation without a document or a display.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{
    Canvas, Engines, Heightfield, LayerPainter, Mark, Painted, Settings, Stroke, StrokePoint,
    parse_paint, presets,
};

/// The side of the square each medium paints in [`self_check`].
const SIDE: usize = 48;

/// Paint one stroke with every built-in medium and report what each left.
///
/// Runs the simulation's own code (outlines, coverage, the media's steps) and
/// so proves a build of this crate works where it is loaded, as Canvas's
/// engine library does in every runtime build. It needs no document, display
/// or network.
///
/// # Errors
/// Names the medium that has no engine or a colour it cannot mix, or that
/// left no paint.
pub fn self_check() -> Result<String, String> {
    let engines = Engines::builtin();
    let mut report = Vec::new();
    for preset in presets::builtin() {
        let engine = engines
            .get(&preset.engine)
            .ok_or_else(|| format!("medium {} names no engine {:?}", preset.id, preset.engine))?;
        let paint = parse_paint("#3a5f8a", &preset.palette)
            .ok_or_else(|| format!("medium {} cannot mix #3a5f8a", preset.id))?;
        let ground = Arc::new(Heightfield::new(
            SIDE,
            SIDE,
            preset.ground.unwrap_or_default(),
            1.0,
            1,
        ));
        let settings = Settings::new(engine.parameters(), &preset.params);
        let mut painter = LayerPainter::new(engine, Canvas::whole(1.0, ground), settings);
        let stroke = Mark::Stroke(Stroke {
            points: vec![
                StrokePoint::new(8.0, 24.0, 0.8),
                StrokePoint::new(24.0, 20.0, 1.0),
                StrokePoint::new(40.0, 26.0, 0.7),
            ],
            size: 8.0,
            paint,
            opacity: 1.0,
            wet: true,
            params: BTreeMap::new(),
            form: None,
            clip: Vec::new(),
        });
        let layer = painter.paint(&[Painted::new("self-check", stroke)]);
        let covered = layer
            .chunks_exact(4)
            .filter(|pixel| matches!(pixel, [_, _, _, alpha] if *alpha > 0))
            .count();
        if covered == 0 {
            return Err(format!("medium {} left no paint", preset.id));
        }
        let digest = blake3::hash(layer).to_hex();
        report.push(format!(
            "{} {covered} px {}",
            preset.id,
            digest.get(..12).unwrap_or_default()
        ));
    }
    if report.is_empty() {
        return Err("no built-in medium to paint with".to_owned());
    }
    Ok(report.join(", "))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_built_in_medium_paints_the_same_stroke_alike() {
        let first = super::self_check().expect("self-check");
        assert_eq!(first.split(", ").count(), crate::presets::builtin().len());
        assert_eq!(
            first,
            super::self_check().expect("self-check"),
            "deterministic"
        );
    }
}
