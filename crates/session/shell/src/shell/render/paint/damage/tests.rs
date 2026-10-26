use std::time::Instant;

use super::*;

fn rect(x: u32, y: u32, width: u32, height: u32) -> FineDamageRect {
    FineDamageRect {
        x,
        y,
        width,
        height,
    }
}

/// A fixed-seed xorshift: the same lists on every run and every host.
struct Lists(u64);

impl Lists {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }

    /// `count` rectangles inside a `width` x `height` canvas, empty ones included.
    fn small(&mut self, count: usize, width: u32, height: u32) -> Vec<FineDamageRect> {
        (0..count)
            .map(|_| {
                let x = self.below(width);
                let y = self.below(height);
                rect(
                    x,
                    y,
                    self.below(width - x + 1).min(24),
                    self.below(height - y + 1).min(24),
                )
            })
            .collect()
    }

    /// What several busy clients damage on a 2944x1840 output in one frame: a terminal's runs
    /// of changed cells, a video's macroblocks and a browser's scattered boxes.
    fn busy_frame(&mut self, count: usize) -> Vec<FineDamageRect> {
        (0..count)
            .map(|i| match i % 3 {
                0 => rect(
                    20 + self.below(80) * 8,
                    40 + self.below(60) * 18,
                    8 * (1 + self.below(40)),
                    18,
                ),
                1 => rect(
                    1500 + self.below(60) * 16,
                    200 + self.below(40) * 16,
                    16,
                    16,
                ),
                _ => rect(
                    self.below(2800),
                    self.below(1700),
                    1 + self.below(140),
                    1 + self.below(120),
                ),
            })
            .collect()
    }
}

/// The pixels `rects` cover, counted one by one.
fn pixels(rects: &[FineDamageRect], width: u32, height: u32) -> Vec<bool> {
    let mut covered = vec![false; (width * height) as usize];
    for r in rects {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                covered[(y * width + x) as usize] = true;
            }
        }
    }
    covered
}

/// Every pixel of `input` lies in `merged`: adding the input to the merged list covers nothing
/// more.
fn covers_every_pixel(merged: &[FineDamageRect], input: &[FineDamageRect]) -> bool {
    let before = damage_area(merged);
    let mut both = merged.to_vec();
    both.extend_from_slice(input);
    damage_area(&both) == before
}

#[test]
fn the_area_counts_each_pixel_once_however_many_rectangles_overlap_it() {
    assert_eq!(damage_area(&[]), 0);
    assert_eq!(damage_area(&[rect(3, 4, 10, 20)]), 200);
    // Two window-sized rectangles overlapping by half count their union, not their sum.
    assert_eq!(
        damage_area(&[rect(0, 0, 100, 50), rect(50, 0, 100, 50)]),
        150 * 50
    );
    // The same rectangle repeated is its own area.
    assert_eq!(damage_area(&vec![rect(10, 10, 2, 16); 300]), 32);
    // A cross: the centre square counted once.
    assert_eq!(
        damage_area(&[rect(10, 0, 10, 30), rect(0, 10, 30, 10)]),
        300 + 300 - 100
    );

    let (width, height) = (96, 64);
    let mut lists = Lists(0x9e37_79b9_7f4a_7c15);
    for case in 0..400 {
        let rects = lists.small(case % 40, width, height);
        let expected = pixels(&rects, width, height)
            .into_iter()
            .filter(|&p| p)
            .count() as u64;
        assert_eq!(damage_area(&rects), expected, "{rects:?}");
    }
}

#[test]
fn merging_covers_every_damaged_pixel_within_the_limit_and_inside_the_damage_bounds() {
    let (width, height) = (96, 64);
    let mut lists = Lists(0x2545_f491_4f6c_dd1d);
    for case in 0..600 {
        let count = [0, 1, 2, 5, 17, 64, 65, 130, 300][case % 9];
        let limit = [1, 2, 7, 64][(case / 9) % 4];
        let input = lists.small(count, width, height);
        let merged = coalesce_damage(input.clone(), limit);

        assert!(
            merged.len() <= limit,
            "{} rects for a limit of {limit}",
            merged.len()
        );
        let before = pixels(&input, width, height);
        let after = pixels(&merged, width, height);
        for (index, (&damaged, &repainted)) in before.iter().zip(&after).enumerate() {
            assert!(
                !damaged || repainted,
                "pixel {index} of {input:?} is lost by {merged:?}"
            );
        }
        // Merging repaints more, but never outside the box the damage spans.
        if let Some(bounds) = input.iter().copied().reduce(damage_union) {
            assert!(merged.iter().all(|&m| contains(bounds, m)), "{merged:?}");
        }
    }
}

#[test]
fn a_short_list_only_loses_the_rectangles_another_contains_and_keeps_its_order() {
    let list = vec![
        rect(50, 50, 4, 4),
        rect(0, 0, 10, 10),
        rect(2, 2, 3, 3),
        rect(50, 50, 4, 4),
        rect(30, 0, 5, 40),
        rect(0, 0, 10, 10),
    ];
    assert_eq!(
        coalesce_damage(list, 64),
        [rect(50, 50, 4, 4), rect(0, 0, 10, 10), rect(30, 0, 5, 40)]
    );
    assert_eq!(
        coalesce_damage(vec![rect(100, 100, 2, 16); 500], 64),
        [rect(100, 100, 2, 16)]
    );
}

#[test]
fn merging_joins_the_pair_whose_union_adds_the_least_first() {
    // Two carets side by side and one far away: the neighbours merge, the far one stays.
    let merged = coalesce_damage(
        vec![
            rect(10, 10, 2, 16),
            rect(1000, 900, 2, 16),
            rect(13, 10, 2, 16),
        ],
        2,
    );
    assert_eq!(merged, [rect(10, 10, 5, 16), rect(1000, 900, 2, 16)]);
}

/// The merge used to search every pair for every merge: cubic, about 0.5, 3.6 and 23 ms for
/// 128, 256 and 512 such rectangles in an optimised build on an Apple M-series laptop.
/// Remembering each rectangle's cheapest partner brings that to about 0.07, 0.2 and 0.7 ms.
/// The timing is printed, not asserted: a loaded runner must not fail it.
#[test]
fn busy_frames_of_128_256_and_512_rectangles_merge_to_the_dispatch_limit() {
    let mut lists = Lists(0xdead_beef_cafe_f00d);
    for count in [128, 256, 512] {
        let frames: Vec<_> = (0..8).map(|_| lists.busy_frame(count)).collect();
        let start = Instant::now();
        let merged: Vec<_> = frames
            .iter()
            .map(|frame| coalesce_damage(frame.clone(), 64))
            .collect();
        let per_frame = start.elapsed() / 8;
        eprintln!("coalesce_damage: {count} rects in {per_frame:?} per frame");
        for (frame, merged) in frames.iter().zip(&merged) {
            assert!(merged.len() <= 64, "{} rects", merged.len());
            assert!(covers_every_pixel(merged, frame));
            assert!(damage_area(merged) >= damage_area(frame));
        }
    }
}
