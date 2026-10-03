// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT OR Unlicense

// The coarse rasterization stage.

#import config
#import bump
#import drawtag
#import ptcl
#import tile

@group(0) @binding(0)
var<uniform> config: Config;

@group(0) @binding(1)
var<storage> scene: array<u32>;

@group(0) @binding(2)
var<storage> draw_monoids: array<DrawMonoid>;

// TODO: dedup
struct BinHeader {
    element_count: u32,
    chunk_offset: u32,
}

@group(0) @binding(3)
var<storage> bin_headers: array<BinHeader>;

@group(0) @binding(4)
var<storage> info_bin_data: array<u32>;

@group(0) @binding(5)
var<storage> paths: array<Path>;

@group(0) @binding(6)
var<storage, read_write> tiles: array<Tile>;

@group(0) @binding(7)
var<storage, read_write> bump: BumpAllocators;

@group(0) @binding(8)
var<storage, read_write> ptcl: array<u32>;

#ifdef sparse
@group(0) @binding(9)
var<uniform> selected_tiles: array<vec4<u32>, 512>;

fn tile_is_selected(tile_ix: u32) -> bool {
    let words = selected_tiles[tile_ix >> 7u];
    let word = words[(tile_ix >> 5u) & 3u];
    return (word & (1u << (tile_ix & 31u))) != 0u;
}

#ifdef active_bins
// The active-bin list is packed after the used tile-selector words in the
// fixed 8 KiB uniform. Access it as scalar words without changing the bind
// group layout or the selector storage used by `tile_is_selected`.
fn selected_tile_word(word_ix: u32) -> u32 {
    let words = selected_tiles[word_ix >> 2u];
    return words[word_ix & 3u];
}
#endif
#endif



// Much of this code assumes WG_SIZE == N_TILE. If these diverge, then
// a fair amount of fixup is needed.
const WG_SIZE = 256u;
const N_SLICE = WG_SIZE / 32u;

var<workgroup> sh_bitmaps: array<array<atomic<u32>, N_TILE>, N_SLICE>;
var<workgroup> sh_part_count: array<u32, WG_SIZE>;
var<workgroup> sh_part_offsets: array<u32, WG_SIZE>;
var<workgroup> sh_drawobj_ix: array<u32, WG_SIZE>;
var<workgroup> sh_tile_stride: array<u32, WG_SIZE>;
var<workgroup> sh_tile_width: array<u32, WG_SIZE>;
var<workgroup> sh_tile_x0y0: array<u32, WG_SIZE>;
var<workgroup> sh_tile_count: array<u32, WG_SIZE>;
var<workgroup> sh_tile_base: array<u32, WG_SIZE>;

// helper functions for writing ptcl

var<private> cmd_offset: u32;
var<private> cmd_limit: u32;
#ifdef opaque_prune
// The per-tile fixed allocation is preserved as a command-list header plus
// the initial command block. Pruning only makes any prior dynamic chain
// unreachable; it never gives an allocation back to the global bump counter.
var<private> cmd_initial_offset: u32;
var<private> cmd_initial_limit: u32;
#endif

// Make sure there is space for a command of given size, plus a jump if needed
fn alloc_cmd(size: u32) {
    if cmd_offset + size >= cmd_limit {
        // We might be able to save a little bit of computation here
        // by setting the initial value of the bump allocator.
        let ptcl_dyn_start = config.width_in_tiles * config.height_in_tiles * PTCL_INITIAL_ALLOC;
        var new_cmd = ptcl_dyn_start + atomicAdd(&bump.ptcl, PTCL_INCREMENT);
        if new_cmd + PTCL_INCREMENT > config.ptcl_size {
            // This sets us up for technical UB, as lots of threads will be writing
            // to the same locations. But I think it's fine, and predicating the
            // writes would probably slow things down.
            new_cmd = 0u;
            atomicOr(&bump.failed, STAGE_COARSE);
        }
        ptcl[cmd_offset] = CMD_JUMP;
        ptcl[cmd_offset + 1u] = new_cmd;
        cmd_offset = new_cmd;
        cmd_limit = cmd_offset + (PTCL_INCREMENT - PTCL_HEADROOM);
    }
}

#ifdef solid_fuse
fn write_path(tile: Tile, tile_ix: u32, draw_flags: u32) -> bool {
#else
fn write_path(tile: Tile, tile_ix: u32, draw_flags: u32) {
#endif
    // We overload the "segments" field to store both count (written by
    // path_count stage) and segment allocation (used by path_tiling and
    // fine).
    let n_segs = tile.segment_count_or_ix;
    if n_segs != 0u {
        var seg_ix = atomicAdd(&bump.segments, n_segs);
        tiles[tile_ix].segment_count_or_ix = ~seg_ix;
        alloc_cmd(4u);
        ptcl[cmd_offset] = CMD_FILL;
        let even_odd = (draw_flags & DRAW_INFO_FLAGS_FILL_RULE_BIT) != 0u;
        let size_and_rule = (n_segs << 1u) | u32(even_odd);
        let fill = CmdFill(size_and_rule, seg_ix, tile.backdrop);
        ptcl[cmd_offset + 1u] = fill.size_and_rule;
        ptcl[cmd_offset + 2u] = fill.seg_data;
        ptcl[cmd_offset + 3u] = u32(fill.backdrop);
        cmd_offset += 4u;
#ifdef solid_fuse
        return false;
#endif
    } else {
#ifdef solid_fuse
        // The immediate successor owns the ordinary SOLID area assignments.
        // It may follow a dynamic jump, so mark that command rather than the
        // current cursor.
        return true;
#else
        alloc_cmd(1u);
        ptcl[cmd_offset] = CMD_SOLID;
        cmd_offset += 1u;
#endif
    }
}

#ifdef solid_fuse
fn write_color(color: CmdColor, fused_solid: bool) {
#else
fn write_color(color: CmdColor) {
#endif
    alloc_cmd(2u);
#ifdef solid_fuse
    ptcl[cmd_offset] = CMD_COLOR | select(0u, CMD_SOLID_FUSED, fused_solid);
#else
    ptcl[cmd_offset] = CMD_COLOR;
#endif
    ptcl[cmd_offset + 1u] = color.rgba_color;
    cmd_offset += 2u;
}

#ifdef solid_fuse
fn write_grad(ty: u32, index: u32, info_offset: u32, fused_solid: bool) {
#else
fn write_grad(ty: u32, index: u32, info_offset: u32) {
#endif
    alloc_cmd(3u);
#ifdef solid_fuse
    ptcl[cmd_offset] = ty | select(0u, CMD_SOLID_FUSED, fused_solid);
#else
    ptcl[cmd_offset] = ty;
#endif
    ptcl[cmd_offset + 1u] = index;
    ptcl[cmd_offset + 2u] = info_offset;
    cmd_offset += 3u;
}

#ifdef solid_fuse
fn write_image(info_offset: u32, fused_solid: bool) {
#else
fn write_image(info_offset: u32) {
#endif
    alloc_cmd(2u);
#ifdef solid_fuse
    ptcl[cmd_offset] = CMD_IMAGE | select(0u, CMD_SOLID_FUSED, fused_solid);
#else
    ptcl[cmd_offset] = CMD_IMAGE;
#endif
    ptcl[cmd_offset + 1u] = info_offset;
    cmd_offset += 2u;
}

fn write_begin_clip() {
    alloc_cmd(1u);
    ptcl[cmd_offset] = CMD_BEGIN_CLIP;
    cmd_offset += 1u;
}

#ifdef solid_fuse
fn write_end_clip(end_clip: CmdEndClip, fused_solid: bool) {
#else
fn write_end_clip(end_clip: CmdEndClip) {
#endif
    alloc_cmd(3u);
#ifdef solid_fuse
    ptcl[cmd_offset] = CMD_END_CLIP | select(0u, CMD_SOLID_FUSED, fused_solid);
#else
    ptcl[cmd_offset] = CMD_END_CLIP;
#endif
    ptcl[cmd_offset + 1u] = end_clip.blend;
    ptcl[cmd_offset + 2u] = bitcast<u32>(end_clip.alpha);
    cmd_offset += 3u;
}

#ifdef solid_fuse
fn write_blurred_rounded_rect(color: CmdColor, info_offset: u32, fused_solid: bool) {
#else
fn write_blurred_rounded_rect(color: CmdColor, info_offset: u32) {
#endif
    alloc_cmd(3u);
#ifdef solid_fuse
    ptcl[cmd_offset] = CMD_BLUR_RECT | select(0u, CMD_SOLID_FUSED, fused_solid);
#else
    ptcl[cmd_offset] = CMD_BLUR_RECT;
#endif
    ptcl[cmd_offset + 1u] = info_offset;
    ptcl[cmd_offset + 2u] = color.rgba_color;
    cmd_offset += 3u;
}

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_id) local_id: vec3<u32>,
    @builtin(workgroup_id) wg_id: vec3<u32>,
) {
    // Exit early if prior stages failed, as we can't run this stage.
    // We need to check only prior stages, as if this stage has failed in another workgroup,
    // we still want to know this workgroup's memory requirement.
    if local_id.x == 0u {
        var failed = atomicLoad(&bump.failed) & (STAGE_BINNING | STAGE_TILE_ALLOC | STAGE_FLATTEN);
        if atomicLoad(&bump.seg_counts) > config.seg_counts_size {
            failed |= STAGE_PATH_COUNT;
        }
        // Reuse sh_part_count to hold failed flag, shmem is tight
        sh_part_count[0] = u32(failed);
    }
    let failed = workgroupUniformLoad(&sh_part_count[0]);
    if failed != 0u {
        // In the active-bin permutation, wg_id.x is the physical dispatch
        // index rather than a native x bin. It still identifies the one
        // workgroup which must forward failure to path_tiling_setup.
        if wg_id.x == 0u && local_id.x == 0u {
            // propagate PATH_COUNT failure to path_tiling_setup so it doesn't need to bind config
            atomicOr(&bump.failed, failed);
        }
        return;
    }
    let width_in_bins = (config.width_in_tiles + N_TILE_X - 1u) / N_TILE_X;
#ifdef active_bins
    let tile_count = config.width_in_tiles * config.height_in_tiles;
    let selector_words = (tile_count + 31u) / 32u;
    let bin_ix = selected_tile_word(selector_words + wg_id.x);
    let bin_x = bin_ix % width_in_bins;
    let bin_y = bin_ix / width_in_bins;
#else
    let bin_ix = width_in_bins * wg_id.y + wg_id.x;
    let bin_x = wg_id.x;
    let bin_y = wg_id.y;
#endif
    let n_partitions = (config.n_drawobj + N_TILE - 1u) / N_TILE;

    // Coordinates of the top left of this bin, in tiles.
    let bin_tile_x = N_TILE_X * bin_x;
    let bin_tile_y = N_TILE_Y * bin_y;

    let tile_x = local_id.x % N_TILE_X;
    let tile_y = local_id.x / N_TILE_X;
    let this_tile_ix = (bin_tile_y + tile_y) * config.width_in_tiles + bin_tile_x + tile_x;
#ifdef sparse
#ifndef active_bins
    // This is workgroup-uniform, so an empty bin can return without making
    // any later workgroup barrier divergent.
    if local_id.x == 0u {
        var any_selected = 0u;
        for (var lane = 0u; lane < WG_SIZE; lane += 1u) {
            let x = bin_tile_x + lane % N_TILE_X;
            let y = bin_tile_y + lane / N_TILE_X;
            if x < config.width_in_tiles && y < config.height_in_tiles {
                any_selected |= u32(tile_is_selected(y * config.width_in_tiles + x));
            }
        }
        sh_part_count[0] = any_selected;
    }
    if workgroupUniformLoad(&sh_part_count[0]) == 0u {
        return;
    }
#endif
    let tile_selected = bin_tile_x + tile_x < config.width_in_tiles
        && bin_tile_y + tile_y < config.height_in_tiles
        && tile_is_selected(this_tile_ix);
#endif
    cmd_offset = this_tile_ix * PTCL_INITIAL_ALLOC;
    cmd_limit = cmd_offset + (PTCL_INITIAL_ALLOC - PTCL_HEADROOM);
#ifdef opaque_prune
    cmd_initial_offset = cmd_offset + 1u;
    cmd_initial_limit = cmd_limit;
#endif

    // clip state
    var clip_zero_depth = 0u;
    var clip_depth = 0u;

    var partition_ix = 0u;
    var rd_ix = 0u;
    var wr_ix = 0u;
    var part_start_ix = 0u;
    var ready_ix = 0u;

    // blend state
    var render_blend_depth = 0u;
    var max_blend_depth = 0u;

    let blend_offset = cmd_offset;
    cmd_offset += 1u;

    while true {
        for (var i = 0u; i < N_SLICE; i += 1u) {
            atomicStore(&sh_bitmaps[i][local_id.x], 0u);
        }

        while true {
            if ready_ix == wr_ix && partition_ix < n_partitions {
                part_start_ix = ready_ix;
                var count = 0u;
                if partition_ix + local_id.x < n_partitions {
                    let in_ix = (partition_ix + local_id.x) * N_TILE + bin_ix;
                    let bin_header = bin_headers[in_ix];
                    count = bin_header.element_count;
                    sh_part_offsets[local_id.x] = bin_header.chunk_offset;
                }
                // prefix sum the element counts
                for (var i = 0u; i < firstTrailingBit(WG_SIZE); i += 1u) {
                    sh_part_count[local_id.x] = count;
                    workgroupBarrier();
                    if local_id.x >= (1u << i) {
                        count += sh_part_count[local_id.x - (1u << i)];
                    }
                    workgroupBarrier();
                }
                sh_part_count[local_id.x] = part_start_ix + count;
                ready_ix = workgroupUniformLoad(&sh_part_count[WG_SIZE - 1u]);
                partition_ix += WG_SIZE;
            }
            // use binary search to find draw object to read
            var ix = rd_ix + local_id.x;
            if ix >= wr_ix && ix < ready_ix {
                var part_ix = 0u;
                for (var i = 0u; i < firstTrailingBit(WG_SIZE); i += 1u) {
                    let probe = part_ix + ((N_TILE / 2u) >> i);
                    if ix >= sh_part_count[probe - 1u] {
                        part_ix = probe;
                    }
                }
                // `select` evaluates both arms, so the first element would read index -1.
                var part_base = part_start_ix;
                if part_ix > 0u {
                    part_base = sh_part_count[part_ix - 1u];
                }
                ix -= part_base;
                let offset = config.bin_data_start + sh_part_offsets[part_ix];
                sh_drawobj_ix[local_id.x] = info_bin_data[offset + ix];
            }
            wr_ix = min(rd_ix + N_TILE, ready_ix);
            if wr_ix - rd_ix >= N_TILE || (wr_ix >= ready_ix && partition_ix >= n_partitions) {
                break;
            }
            workgroupBarrier();
        }
        // At this point, sh_drawobj_ix[0.. wr_ix - rd_ix] contains merged binning results.
        var tag = DRAWTAG_NOP;
        var drawobj_ix: u32;
        if local_id.x + rd_ix < wr_ix {
            drawobj_ix = sh_drawobj_ix[local_id.x];
            tag = scene[config.drawtag_base + drawobj_ix];
        }

        var tile_count = 0u;
        // I think this predicate is the same as the last, maybe they can be combined
        if tag != DRAWTAG_NOP {
            let path_ix = draw_monoids[drawobj_ix].path_ix;
            let path = paths[path_ix];
            let stride = path.bbox.z - path.bbox.x;
            sh_tile_stride[local_id.x] = stride;
            let dx = i32(path.bbox.x) - i32(bin_tile_x);
            let dy = i32(path.bbox.y) - i32(bin_tile_y);
            let x0 = clamp(dx, 0, i32(N_TILE_X));
            let y0 = clamp(dy, 0, i32(N_TILE_Y));
            let x1 = clamp(i32(path.bbox.z) - i32(bin_tile_x), 0, i32(N_TILE_X));
            let y1 = clamp(i32(path.bbox.w) - i32(bin_tile_y), 0, i32(N_TILE_Y));
            sh_tile_width[local_id.x] = u32(x1 - x0);
            sh_tile_x0y0[local_id.x] = u32(x0) | u32(y0 << 16u);
            tile_count = u32(x1 - x0) * u32(y1 - y0);
            // base relative to bin
            let base = path.tiles - u32(dy * i32(stride) + dx);
            sh_tile_base[local_id.x] = base;
            // TODO: there's a write_tile_alloc here in the source, not sure what it's supposed to do
        }

        // Prefix sum of tile counts
        sh_tile_count[local_id.x] = tile_count;
        for (var i = 0u; i < firstTrailingBit(N_TILE); i += 1u) {
            workgroupBarrier();
            if local_id.x >= (1u << i) {
                tile_count += sh_tile_count[local_id.x - (1u << i)];
            }
            workgroupBarrier();
            sh_tile_count[local_id.x] = tile_count;
        }
        workgroupBarrier();
        let total_tile_count = sh_tile_count[N_TILE - 1u];
        // Parallel iteration over all tiles
        for (var ix = local_id.x; ix < total_tile_count; ix += N_TILE) {
            // Binary search to find draw object which contains this tile
            var el_ix = 0u;
            for (var i = 0u; i < firstTrailingBit(N_TILE); i += 1u) {
                let probe = el_ix + ((N_TILE / 2u) >> i);
                if ix >= sh_tile_count[probe - 1u] {
                    el_ix = probe;
                }
            }
            drawobj_ix = sh_drawobj_ix[el_ix];
            tag = scene[config.drawtag_base + drawobj_ix];
            // `select` evaluates both arms, so the first element would read index -1.
            var prev_count = 0u;
            if el_ix > 0u {
                prev_count = sh_tile_count[el_ix - 1u];
            }
            let seq_ix = ix - prev_count;
            let width = sh_tile_width[el_ix];
            let x0y0 = sh_tile_x0y0[el_ix];
            let x = (x0y0 & 0xffffu) + seq_ix % width;
            let y = (x0y0 >> 16u) + seq_ix / width;
            let tile_ix = sh_tile_base[el_ix] + sh_tile_stride[el_ix] * y + x;
            let tile = tiles[tile_ix];
            let is_clip = (tag & 1u) != 0u;
            var is_blend = false;
            if is_clip {
                let BLEND_CLIP = (128u << 8u) | 3u;
                let scene_offset = draw_monoids[drawobj_ix].scene_offset;
                let dd = config.drawdata_base + scene_offset;
                let blend = scene[dd];
                is_blend = blend != BLEND_CLIP;
            }

            let di = draw_monoids[drawobj_ix].info_offset;
            let draw_flags = info_bin_data[di];
            let even_odd = (draw_flags & DRAW_INFO_FLAGS_FILL_RULE_BIT) != 0u;
            let n_segs = tile.segment_count_or_ix;

            // If this draw object represents an even-odd fill and we know that no line segment
            // crosses this tile and then this draw object should not contribute to the tile if its
            // backdrop (i.e. the winding number of its top-left corner) is even.
            let backdrop_clear = select(tile.backdrop, abs(tile.backdrop) & 1, even_odd) == 0;
            let include_tile = n_segs != 0u || (backdrop_clear == is_clip) || is_blend;
            var selected_candidate = true;
#ifdef sparse
            let candidate_x = bin_tile_x + x;
            let candidate_y = bin_tile_y + y;
            selected_candidate = candidate_x < config.width_in_tiles
                && candidate_y < config.height_in_tiles
                && tile_is_selected(candidate_y * config.width_in_tiles + candidate_x);
#endif
            if include_tile && selected_candidate {
                let el_slice = el_ix / 32u;
                let el_mask = 1u << (el_ix & 31u);
                atomicOr(&sh_bitmaps[el_slice][y * N_TILE_X + x], el_mask);
            }
        }
        workgroupBarrier();
        // At this point bit drawobj % 32 is set in sh_bitmaps[drawobj / 32][y * N_TILE_X + x]
        // if drawobj touches tile (x, y).

        // Write per-tile command list for this tile
        var slice_ix = 0u;
        var bitmap = atomicLoad(&sh_bitmaps[0u][local_id.x]);
        while true {
            if bitmap == 0u {
                slice_ix += 1u;
                // potential optimization: make iteration limit dynamic
                if slice_ix == N_SLICE {
                    break;
                }
                bitmap = atomicLoad(&sh_bitmaps[slice_ix][local_id.x]);
                if bitmap == 0u {
                    continue;
                }
            }

            let el_ix = slice_ix * 32u + firstTrailingBit(bitmap);
            drawobj_ix = sh_drawobj_ix[el_ix];
            // clear LSB of bitmap, using bit magic
            bitmap &= bitmap - 1u;
            let drawtag = scene[config.drawtag_base + drawobj_ix];
            let dm = draw_monoids[drawobj_ix];
            let dd = config.drawdata_base + dm.scene_offset;
            let di = dm.info_offset;
            let draw_flags = info_bin_data[di];
            if clip_zero_depth == 0u {
                let tile_ix = sh_tile_base[el_ix] + sh_tile_stride[el_ix] * tile_y + tile_x;
                let tile = tiles[tile_ix];
                switch drawtag {
                    case DRAWTAG_FILL_COLOR: {
                        let rgba_color = scene[dd];
#ifdef opaque_prune
                        let even_odd = (draw_flags & DRAW_INFO_FLAGS_FILL_RULE_BIT) != 0u;
                        let backdrop_clear = select(tile.backdrop, abs(tile.backdrop) & 1, even_odd) == 0;
                        // Only a completely un-clipped, unsegmented, opaque
                        // fill over a non-clear backdrop can replace the
                        // earlier list. Keep historical blend accounting and
                        // every prior bump allocation intact.
                        if clip_zero_depth == 0u
                            && clip_depth == 0u
                            && render_blend_depth == 0u
                            && tile.segment_count_or_ix == 0u
                            && !backdrop_clear
                            && (rgba_color >> 24u) == 255u
                        {
                            cmd_offset = cmd_initial_offset;
                            cmd_limit = cmd_initial_limit;
                        }
#endif
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_color(CmdColor(rgba_color), fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_color(CmdColor(rgba_color));
#endif
                    }
                    case DRAWTAG_BLURRED_ROUNDED_RECT: {
                        let rgba_color = scene[dd];
                        let info_offset = di + 1u;
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_blurred_rounded_rect(CmdColor(rgba_color), info_offset, fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_blurred_rounded_rect(CmdColor(rgba_color), info_offset);
#endif
                    }
                    case DRAWTAG_FILL_LIN_GRADIENT: {
                        let index = scene[dd];
                        let info_offset = di + 1u;
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_LIN_GRAD, index, info_offset, fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_LIN_GRAD, index, info_offset);
#endif
                    }
                    case DRAWTAG_FILL_RAD_GRADIENT: {
                        let index = scene[dd];
                        let info_offset = di + 1u;
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_RAD_GRAD, index, info_offset, fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_RAD_GRAD, index, info_offset);
#endif
                    }
                    case DRAWTAG_FILL_SWEEP_GRADIENT: {
                        let index = scene[dd];
                        let info_offset = di + 1u;
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_SWEEP_GRAD, index, info_offset, fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_grad(CMD_SWEEP_GRAD, index, info_offset);
#endif
                    }
                    case DRAWTAG_FILL_IMAGE: {
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_image(di + 1u, fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_image(di + 1u);
#endif
                    }
                    case DRAWTAG_BEGIN_CLIP: {
                        let even_odd = (draw_flags & DRAW_INFO_FLAGS_FILL_RULE_BIT) != 0u;
                        let backdrop_clear = select(tile.backdrop, abs(tile.backdrop) & 1, even_odd) == 0;
                        if tile.segment_count_or_ix == 0u && backdrop_clear {
                            clip_zero_depth = clip_depth + 1u;
                        } else {
                            write_begin_clip();
                            render_blend_depth += 1u;
                            max_blend_depth = max(max_blend_depth, render_blend_depth);
                        }
                        clip_depth += 1u;
                    }
                    case DRAWTAG_END_CLIP: {
                        clip_depth -= 1u;
                        let blend = scene[dd];
                        let alpha = bitcast<f32>(scene[dd + 1u]);
#ifdef solid_fuse
                        let fused_solid = write_path(tile, tile_ix, draw_flags);
                        write_end_clip(CmdEndClip(blend, alpha), fused_solid);
#else
                        write_path(tile, tile_ix, draw_flags);
                        write_end_clip(CmdEndClip(blend, alpha));
#endif
                        render_blend_depth -= 1u;
                    }
                    default: {}
                }
            } else {
                // In "clip zero" state, suppress all drawing
                switch drawtag {
                    case DRAWTAG_BEGIN_CLIP: {
                        clip_depth += 1u;
                    }
                    case DRAWTAG_END_CLIP: {
                        if clip_depth == clip_zero_depth {
                            clip_zero_depth = 0u;
                        }
                        clip_depth -= 1u;
                    }
                    default: {}
                }
            }
        }

        rd_ix += N_TILE;
        if rd_ix >= ready_ix && partition_ix >= n_partitions {
            break;
        }
        workgroupBarrier();
    }
    if bin_tile_x + tile_x < config.width_in_tiles && bin_tile_y + tile_y < config.height_in_tiles
#ifdef sparse
        && tile_selected
#endif
    {
        ptcl[cmd_offset] = CMD_END;
        var blend_ix = 0u;
        if max_blend_depth > BLEND_STACK_SPLIT {
            let scratch_size = (max_blend_depth - BLEND_STACK_SPLIT) * TILE_WIDTH * TILE_HEIGHT;
            blend_ix = atomicAdd(&bump.blend, scratch_size);
            if blend_ix + scratch_size > config.blend_size {
                atomicOr(&bump.failed, STAGE_COARSE);
            }
        }
        ptcl[blend_offset] = blend_ix;
    }
}
