//! Flattening layers onto a ground on the CPU, for hosts without a GPU
//! scene: headless checks and exports. The blends match what a vector
//! renderer does with the same straight-alpha layers.

use crate::colour::encode8;
use crate::engine::Blend;

fn unit(value: u8) -> f32 {
    f32::from(value) / 255.0
}

/// Draw straight-alpha sRGB `layer` over opaque `backdrop` (both RGBA8 of
/// the same size) with `blend` at `opacity`.
pub fn onto(backdrop: &mut [u8], layer: &[u8], blend: Blend, opacity: f32) {
    let opacity = opacity.clamp(0.0, 1.0);
    for (under, over) in backdrop.chunks_exact_mut(4).zip(layer.chunks_exact(4)) {
        let &[r, g, b, a] = over else {
            continue;
        };
        let [r, g, b, a] = [r, g, b, a].map(unit);
        let alpha = a * opacity;
        if alpha <= 0.0 {
            continue;
        }
        for (channel, source) in under.iter_mut().take(3).zip([r, g, b]) {
            let below = unit(*channel);
            let blended = match blend {
                Blend::Normal => source,
                Blend::Multiply => source * below,
            };
            *channel = encode8(below + (blended - below) * alpha);
        }
    }
}
