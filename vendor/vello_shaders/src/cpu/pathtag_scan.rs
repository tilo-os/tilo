// Copyright 2023 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT OR Unlicense

use vello_encoding::{ConfigUniform, Monoid, PathMonoid};

use super::CpuBinding;

const WG_SIZE: usize = 256;

fn pathtag_scan_main(
    n_wg: u32,
    config: &ConfigUniform,
    scene: &[u32],
    reduced: &[PathMonoid],
    tag_monoids: &mut [PathMonoid],
) {
    let pathtag_base = config.layout.path_tag_base;
    let mut prefix = PathMonoid::default();
    for i in 0..n_wg {
        let mut m = prefix;
        for j in 0..WG_SIZE {
            let ix = (i * WG_SIZE as u32) as usize + j;
            tag_monoids[ix] = m;
            let tag = scene[pathtag_base as usize + ix];
            m = m.combine(&PathMonoid::new(tag));
        }
        prefix = prefix.combine(&reduced[i as usize]);
    }
}

pub fn pathtag_scan(n_wg: u32, resources: &[CpuBinding<'_>]) {
    let config = resources[0].as_typed();
    let scene = resources[1].as_slice();
    let reduced = resources[2].as_slice();
    let mut tag_monoids = resources[3].as_slice_mut();
    pathtag_scan_main(n_wg, &config, &scene, &reduced, &mut tag_monoids);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::mem::size_of;

    use vello_encoding::PathTag;

    use super::*;

    #[test]
    fn reduce_and_scan_cross_the_small_scan_workgroup_boundary() {
        // 257 workgroups is one past the GPU small-scan threshold. Keeping this
        // CPU-only makes the boundary regression compact and deterministic.
        const N_WG: u32 = 257;
        const PATH_TAG_BASE: usize = 7;
        let patterns = [
            u32::from_le_bytes([
                PathTag::PATH.0,
                PathTag::TRANSFORM.0,
                PathTag::LINE_TO_F32.0,
                PathTag::STYLE.0,
            ]),
            u32::from_le_bytes([
                PathTag::QUAD_TO_I16.0,
                PathTag::CUBIC_TO_F32.0 | PathTag::SUBPATH_END_BIT,
                PathTag::PATH.0,
                PathTag::LINE_TO_I16.0,
            ]),
            u32::from_le_bytes([
                PathTag::STYLE.0,
                PathTag::TRANSFORM.0,
                PathTag::QUAD_TO_F32.0,
                PathTag::PATH.0,
            ]),
        ];
        let tag_count = N_WG as usize * WG_SIZE;
        let mut scene = vec![0xfeed_face; PATH_TAG_BASE];
        scene.extend((0..tag_count).map(|ix| patterns[ix % patterns.len()]));

        let mut config = ConfigUniform::default();
        config.layout.path_tag_base = PATH_TAG_BASE as u32;
        let reduced = RefCell::new(vec![0; N_WG as usize * size_of::<PathMonoid>()]);
        let tag_monoids = RefCell::new(vec![0; tag_count * size_of::<PathMonoid>()]);

        let reduce_bindings = [
            CpuBinding::Buffer(bytemuck::bytes_of(&config)),
            CpuBinding::Buffer(bytemuck::cast_slice(&scene)),
            CpuBinding::BufferRW(&reduced),
        ];
        super::super::pathtag_reduce(N_WG, &reduce_bindings);

        let scan_bindings = [
            CpuBinding::Buffer(bytemuck::bytes_of(&config)),
            CpuBinding::Buffer(bytemuck::cast_slice(&scene)),
            CpuBinding::BufferRW(&reduced),
            CpuBinding::BufferRW(&tag_monoids),
        ];
        pathtag_scan(N_WG, &scan_bindings);

        let tag_monoids = tag_monoids.into_inner();
        let tag_monoids: &[PathMonoid] = bytemuck::cast_slice(&tag_monoids);
        let mut expected = PathMonoid::default();
        for (ix, tag) in scene[PATH_TAG_BASE..].iter().copied().enumerate() {
            let actual = tag_monoids[ix];
            assert_eq!(
                (
                    actual.trans_ix,
                    actual.pathseg_ix,
                    actual.pathseg_offset,
                    actual.style_ix,
                    actual.path_ix,
                ),
                (
                    expected.trans_ix,
                    expected.pathseg_ix,
                    expected.pathseg_offset,
                    expected.style_ix,
                    expected.path_ix,
                ),
                "exclusive PathMonoid prefix mismatch at word {ix}",
            );
            expected = expected.combine(&PathMonoid::new(tag));
        }
    }
}
