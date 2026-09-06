// Copyright 2026 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT OR Unlicense

// Diagnostic opt-in canonicalization of the segment order consumed by Area fine.
// Each invocation owns one native tile's PTCL stream. It first validates the
// complete stream, then heapsorts every reachable fill in place. The sort key
// is the exact raw-bit lexicographic order of the five meaningful Segment
// words; the sixth physical word always moves with its record.

#import bump
#import config
#import ptcl

const SEGMENT_WORDS = 6u;
const FUSED_SOLID_MARKER = 0x80000000u;

struct SegmentWords {
    words: array<u32, 6>,
}

@group(0) @binding(0)
var<uniform> config: Config;

@group(0) @binding(1)
var<storage, read_write> bump: BumpAllocators;

@group(0) @binding(2)
var<storage, read_write> segments: array<SegmentWords>;

// Atomic access lets a malformed PTCL stream poison the established fine-pass
// sentinel before the later ordinary read-only fine dispatch begins.
@group(0) @binding(3)
var<storage, read_write> ptcl: array<atomic<u32>>;

#ifdef sparse
@group(0) @binding(4)
var<storage> damage_tiles: array<u32>;
#endif

struct Cursor {
    command: u32,
    block_start: u32,
    block_end: u32,
}

fn fail() {
    atomicOr(&bump.failed, STAGE_CANONICAL_SEGMENT_ORDER);
    if arrayLength(&ptcl) != 0u {
        atomicStore(&ptcl[0], ~0u);
    }
}

fn command_width(raw_tag: u32) -> u32 {
    let has_fused_marker = (raw_tag & FUSED_SOLID_MARKER) != 0u;
    let tag = raw_tag & ~FUSED_SOLID_MARKER;
#ifdef solid_fuse
    if has_fused_marker
        && tag != CMD_COLOR
        && tag != CMD_LIN_GRAD
        && tag != CMD_RAD_GRAD
        && tag != CMD_SWEEP_GRAD
        && tag != CMD_IMAGE
        && tag != CMD_END_CLIP
        && tag != CMD_BLUR_RECT
    {
        return 0u;
    }
#else
    if has_fused_marker {
        return 0u;
    }
#endif
    switch tag {
        case CMD_END: {
            return 1u;
        }
        case CMD_FILL: {
            return 4u;
        }
        case CMD_SOLID, CMD_BEGIN_CLIP: {
            return 1u;
        }
        case CMD_COLOR, CMD_IMAGE: {
            return 2u;
        }
        case CMD_LIN_GRAD, CMD_RAD_GRAD, CMD_SWEEP_GRAD, CMD_END_CLIP, CMD_BLUR_RECT: {
            return 3u;
        }
        case CMD_JUMP: {
            return 2u;
        }
        default: {
            return 0u;
        }
    }
}

fn advance_cursor(
    cursor: ptr<function, Cursor>,
    raw_tag: u32,
    ptcl_limit: u32,
    dynamic_base: u32,
) -> bool {
    let width = command_width(raw_tag);
    if width == 0u || (*cursor).command > (*cursor).block_end - width {
        return false;
    }
    let tag = raw_tag & ~FUSED_SOLID_MARKER;
    if tag == CMD_JUMP {
        let jump_target = atomicLoad(&ptcl[(*cursor).command + 1u]);
        if jump_target <= (*cursor).block_start
            || jump_target < dynamic_base
            || (jump_target - dynamic_base) % PTCL_INCREMENT != 0u
            || jump_target > ptcl_limit
            || PTCL_INCREMENT > ptcl_limit - jump_target
        {
            return false;
        }
        (*cursor).command = jump_target;
        (*cursor).block_start = jump_target;
        (*cursor).block_end = jump_target + PTCL_INCREMENT;
    } else {
        (*cursor).command += width;
    }
    return true;
}

fn validate_tile(tile_ix: u32, native_tiles: u32, ptcl_limit: u32, segment_limit: u32) -> bool {
    if native_tiles > ptcl_limit / PTCL_INITIAL_ALLOC {
        return false;
    }
    let initial_words = native_tiles * PTCL_INITIAL_ALLOC;
    if initial_words > ptcl_limit || tile_ix > (ptcl_limit - PTCL_INITIAL_ALLOC) / PTCL_INITIAL_ALLOC {
        return false;
    }
    var cursor = Cursor(tile_ix * PTCL_INITIAL_ALLOC + 1u, tile_ix * PTCL_INITIAL_ALLOC, (tile_ix + 1u) * PTCL_INITIAL_ALLOC);
    var previous_end = 0u;
    loop {
        if cursor.command >= cursor.block_end {
            return false;
        }
        let raw_tag = atomicLoad(&ptcl[cursor.command]);
        let tag = raw_tag & ~FUSED_SOLID_MARKER;
        if tag == CMD_END {
            return raw_tag == CMD_END;
        }
        if command_width(raw_tag) == 0u {
            return false;
        }
        if tag == CMD_FILL {
            if cursor.command > cursor.block_end - 4u {
                return false;
            }
            let size_and_rule = atomicLoad(&ptcl[cursor.command + 1u]);
            let count = size_and_rule >> 1u;
            let start = atomicLoad(&ptcl[cursor.command + 2u]);
            if count == 0u || start < previous_end || start > segment_limit || count > segment_limit - start {
                return false;
            }
            previous_end = start + count;
        }
        if !advance_cursor(&cursor, raw_tag, ptcl_limit, initial_words) {
            return false;
        }
    }
    // The loop returns on every reachable path. Keep an explicit fallback so
    // Naga can prove this non-void helper has a complete return path.
    return false;
}

fn greater_than(left: u32, right: u32) -> bool {
    for (var word = 0u; word < SEGMENT_WORDS - 1u; word += 1u) {
        let lhs = segments[left].words[word];
        let rhs = segments[right].words[word];
        if lhs != rhs {
            return lhs > rhs;
        }
    }
    return false;
}

fn swap_records(left: u32, right: u32) {
    if left == right {
        return;
    }
    var temporary: array<u32, 6>;
    for (var word = 0u; word < SEGMENT_WORDS; word += 1u) {
        temporary[word] = segments[left].words[word];
    }
    for (var word = 0u; word < SEGMENT_WORDS; word += 1u) {
        segments[left].words[word] = segments[right].words[word];
    }
    for (var word = 0u; word < SEGMENT_WORDS; word += 1u) {
        segments[right].words[word] = temporary[word];
    }
}

fn sift_down(base: u32, root_start: u32, end: u32) {
    if end < 2u {
        return;
    }
    var root = root_start;
    loop {
        // There is no child if root is in the upper half. This form avoids a
        // potentially overflowing `root * 2 + 1` for large valid fills.
        if root >= end / 2u {
            return;
        }
        let child = root * 2u + 1u;
        var candidate = root;
        if greater_than(base + child, base + candidate) {
            candidate = child;
        }
        if child + 1u < end && greater_than(base + child + 1u, base + candidate) {
            candidate = child + 1u;
        }
        if candidate == root {
            return;
        }
        swap_records(base + root, base + candidate);
        root = candidate;
    }
}

fn heap_sort(base: u32, count: u32) {
    if count < 2u {
        return;
    }
    var start = count / 2u;
    loop {
        if start == 0u {
            break;
        }
        start -= 1u;
        sift_down(base, start, count);
    }
    var end = count;
    loop {
        if end <= 1u {
            return;
        }
        end -= 1u;
        swap_records(base, base + end);
        sift_down(base, 0u, end);
    }
}

fn sort_tile(tile_ix: u32, native_tiles: u32, ptcl_limit: u32) {
    if native_tiles > ptcl_limit / PTCL_INITIAL_ALLOC {
        fail();
        return;
    }
    let initial_words = native_tiles * PTCL_INITIAL_ALLOC;
    var cursor = Cursor(tile_ix * PTCL_INITIAL_ALLOC + 1u, tile_ix * PTCL_INITIAL_ALLOC, (tile_ix + 1u) * PTCL_INITIAL_ALLOC);
    loop {
        let raw_tag = atomicLoad(&ptcl[cursor.command]);
        let tag = raw_tag & ~FUSED_SOLID_MARKER;
        if tag == CMD_END {
            return;
        }
        if tag == CMD_FILL {
            heap_sort(
                atomicLoad(&ptcl[cursor.command + 2u]),
                atomicLoad(&ptcl[cursor.command + 1u]) >> 1u,
            );
        }
        let advanced = advance_cursor(&cursor, raw_tag, ptcl_limit, initial_words);
        if !advanced {
            fail();
            return;
        }
    }
}

@compute @workgroup_size(1)
fn main(@builtin(workgroup_id) workgroup_id: vec3<u32>) {
    if atomicLoad(&bump.failed) != 0u {
        if arrayLength(&ptcl) != 0u {
            atomicStore(&ptcl[0], ~0u);
        }
        return;
    }
    if config.width_in_tiles == 0u
        || config.height_in_tiles == 0u
        || config.height_in_tiles > 0xffffffffu / config.width_in_tiles
    {
        fail();
        return;
    }
    let native_tiles = config.width_in_tiles * config.height_in_tiles;
    if native_tiles > 0xffffffffu / PTCL_INITIAL_ALLOC {
        fail();
        return;
    }
    let initial_words = native_tiles * PTCL_INITIAL_ALLOC;
    let ptcl_words = arrayLength(&ptcl);
    let segment_records = arrayLength(&segments);
    let dynamic_ptcl_words = atomicLoad(&bump.ptcl);
    let used_segments = atomicLoad(&bump.segments);
    if config.ptcl_size < initial_words
        || config.ptcl_size > ptcl_words
        || dynamic_ptcl_words > config.ptcl_size - initial_words
        || config.segments_size > segment_records
        || used_segments > config.segments_size
        || used_segments > segment_records
    {
        fail();
        return;
    }
    let used_ptcl_words = initial_words + dynamic_ptcl_words;
#ifdef sparse
    let damage_words = arrayLength(&damage_tiles);
    if workgroup_id.x > damage_words / 9u {
        fail();
        return;
    }
    let damage_base = workgroup_id.x * 9u;
    if 9u > damage_words - damage_base {
        fail();
        return;
    }
    let tile_ix = damage_tiles[damage_base];
#else
    if workgroup_id.x >= config.width_in_tiles || workgroup_id.y >= config.height_in_tiles {
        fail();
        return;
    }
    let tile_ix = workgroup_id.y * config.width_in_tiles + workgroup_id.x;
#endif
    if tile_ix >= native_tiles || !validate_tile(tile_ix, native_tiles, used_ptcl_words, used_segments) {
        fail();
        return;
    }
    sort_tile(tile_ix, native_tiles, used_ptcl_words);
}
