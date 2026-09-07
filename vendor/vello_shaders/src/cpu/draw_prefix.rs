// Copyright 2026 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT OR Unlicense

//! CPU-exclusive prefix generation for the bounded draw-prefix fast path.

use vello_encoding::{ConfigUniform, DrawMonoid, DrawTag, Monoid};

use super::{CpuBinding, util::read_draw_tag_from_scene};

fn draw_prefix_main(config: &ConfigUniform, scene: &[u32], draw_monoid: &mut [DrawMonoid]) {
    if config.layout.n_draw_objects == 0 {
        // GPU buffer bindings must contain at least one element. Keep that
        // allocation an identity table even though no draw-leaf workgroup runs.
        if let Some(identity) = draw_monoid.first_mut() {
            *identity = DrawMonoid::default();
        }
        return;
    }
    let mut prefix = DrawMonoid::default();
    for ix in 0..config.layout.n_draw_objects {
        draw_monoid[ix as usize] = prefix;
        let tag = DrawTag(read_draw_tag_from_scene(config, scene, ix));
        prefix = prefix.combine(&DrawMonoid::new(tag));
    }
}

/// Build the exclusive draw-monoid prefix table used by `draw_leaf_prefix`.
///
/// The CPU-only dispatch has exactly three bindings: config, scene, and the
/// read-write `draw_monoid` prefix table.
pub fn draw_prefix(_n_wg: u32, resources: &[CpuBinding<'_>]) {
    let config = resources[0].as_typed();
    let scene = resources[1].as_slice();
    let mut draw_monoid = resources[2].as_slice_mut();
    draw_prefix_main(&config, &scene, &mut draw_monoid);
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, mem::size_of};

    use vello_encoding::DrawTag;

    use super::*;

    fn prefix_for(tags: &[DrawTag], draw_tag_base: usize) -> Vec<DrawMonoid> {
        let mut config = ConfigUniform::default();
        config.layout.draw_tag_base = draw_tag_base as u32;
        config.layout.n_draw_objects = tags.len() as u32;
        let mut scene = vec![0xfeed_face; draw_tag_base];
        scene.extend(tags.iter().map(|tag| tag.0));
        // Buffer allocations are at least one element even for an empty stream.
        let draw_monoid = RefCell::new(vec![0xa5; tags.len().max(1) * size_of::<DrawMonoid>()]);
        let bindings = [
            CpuBinding::Buffer(bytemuck::bytes_of(&config)),
            CpuBinding::Buffer(bytemuck::cast_slice(&scene)),
            CpuBinding::BufferRW(&draw_monoid),
        ];
        draw_prefix(1, &bindings);
        bytemuck::cast_slice(&draw_monoid.into_inner()).to_vec()
    }

    fn expected_prefix(tags: &[DrawTag]) -> Vec<DrawMonoid> {
        let mut prefix = DrawMonoid::default();
        tags.iter()
            .copied()
            .map(|tag| {
                let result = prefix;
                prefix = prefix.combine(&DrawMonoid::new(tag));
                result
            })
            .collect()
    }

    #[test]
    fn draw_prefix_matches_sequential_prefix_across_boundaries() {
        let pattern = [
            DrawTag::NOP,
            DrawTag::BEGIN_CLIP,
            DrawTag::COLOR,
            DrawTag::LINEAR_GRADIENT,
            DrawTag::RADIAL_GRADIENT,
            DrawTag::SWEEP_GRADIENT,
            DrawTag::IMAGE,
            DrawTag::END_CLIP,
        ];
        for n_draw_objects in [0, 1, 255, 256, 257, 513] {
            let tags: Vec<_> = pattern
                .iter()
                .copied()
                .cycle()
                .take(n_draw_objects)
                .collect();
            let actual = prefix_for(&tags, 11);
            if tags.is_empty() {
                assert_eq!(actual, vec![DrawMonoid::default()]);
            } else {
                assert_eq!(actual, expected_prefix(&tags));
            }
        }
    }
}
