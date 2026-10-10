//! The pictures the screenshot folders hold: small, deterministic scenes
//! (a sunset, a harbour, a forest path, a flower, a mountain lake) and a book
//! cover, drawn per pixel so the demo needs no image assets. They are real
//! image files that go through the same decode, downscale and cache path as a
//! person's own photos.

type Rgb = [f32; 3];

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash(x: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Smooth one-dimensional noise in `0..1`.
fn noise(pos: f32, seed: u32) -> f32 {
    let cell = pos.floor();
    let frac = pos - cell;
    let ease = frac * frac * (3.0 - 2.0 * frac);
    let low = hash(cell as i32, seed);
    let high = hash(cell as i32 + 1, seed);
    low + (high - low) * ease
}

/// Whether `(px, py)` is inside the triangle `a`, `b`, `c`.
fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

fn sunset(fx: f32, fy: f32) -> Rgb {
    let horizon = 0.64;
    let mut c = if fy < horizon {
        mix(
            [64.0, 54.0, 122.0],
            [255.0, 164.0, 92.0],
            smooth(0.0, horizon, fy).powf(1.5),
        )
    } else {
        [40.0, 26.0, 60.0]
    };
    let (dx, dy) = (fx - 0.5, fy - 0.57);
    let d = (dx * dx + dy * dy).sqrt();
    if fy < horizon + 0.04 {
        c = mix(c, [255.0, 214.0, 140.0], (1.0 - smooth(0.0, 0.5, d)) * 0.6);
    }
    c = mix(c, [255.0, 246.0, 210.0], 1.0 - smooth(0.07, 0.085, d));
    let far = 0.62 + 0.05 * (noise(fx * 3.4, 1) * 2.0 - 1.0);
    if fy > far {
        c = mix(
            [128.0, 74.0, 112.0],
            [40.0, 26.0, 60.0],
            smooth(far, 1.0, fy),
        );
    }
    let near = 0.76 + 0.06 * (noise(fx * 2.2 + 4.0, 2) * 2.0 - 1.0);
    if fy > near {
        c = [34.0, 22.0, 52.0];
    }
    c
}

fn harbour(fx: f32, fy: f32) -> Rgb {
    let horizon = 0.5;
    let mut c = if fy < horizon {
        let sky = mix(
            [92.0, 158.0, 222.0],
            [216.0, 234.0, 246.0],
            smooth(0.0, horizon, fy),
        );
        let cloud = smooth(0.58, 0.82, noise(fx * 4.0 + fy * 9.0, 3));
        mix(sky, [255.0, 255.0, 255.0], cloud * 0.55)
    } else {
        let t = (fy - horizon) / (1.0 - horizon);
        let sea = mix([74.0, 152.0, 192.0], [12.0, 52.0, 98.0], t.powf(0.8));
        let ripple = ((fy * 120.0) + noise(fx * 9.0, 4) * 7.0).sin() * 0.5 + 0.5;
        mix(sea, [206.0, 232.0, 246.0], ripple * 0.1 * (1.0 - t))
    };
    for (bx, by, s) in [(0.28, 0.6, 0.12), (0.64, 0.7, 0.17), (0.84, 0.56, 0.08)] {
        let p = (fx, fy);
        if in_triangle(p, (bx, by - s), (bx, by - 0.02), (bx + 0.6 * s, by - 0.02)) {
            c = [246.0, 246.0, 240.0];
        }
        if in_triangle(
            p,
            (bx - 0.04 * s, by - 0.7 * s),
            (bx - 0.04 * s, by - 0.02),
            (bx - 0.5 * s, by - 0.02),
        ) {
            c = [226.0, 232.0, 236.0];
        }
        if (fx - bx).abs() < 0.62 * s && fy > by - 0.02 && fy < by + 0.16 * s {
            c = [34.0, 44.0, 66.0];
        }
    }
    c
}

fn forest(fx: f32, fy: f32) -> Rgb {
    // A portrait frame: x spans 0.75 of the picture's height.
    let x = fx;
    let mut c = mix(
        [206.0, 228.0, 212.0],
        [120.0, 170.0, 140.0],
        smooth(0.0, 0.8, fy),
    );
    let layers: [(f32, f32, f32, Rgb, u32); 2] = [
        (0.62, 0.3, 0.075, [96.0, 142.0, 114.0], 11),
        (0.74, 0.4, 0.1, [58.0, 106.0, 80.0], 12),
    ];
    for (base, height, half, color, seed) in layers {
        for i in 0..16 {
            let cx = hash(i, seed) * 1.2 - 0.1;
            let top = base - height * (0.7 + 0.3 * hash(i, seed + 40));
            if in_triangle((x, fy), (cx, top), (cx - half, base), (cx + half, base)) {
                c = color;
            }
        }
        if fy > base {
            c = color;
        }
    }
    // The path narrows toward the middle of the picture.
    if fy > 0.7 {
        let half = (fy - 0.7) * 0.6;
        if (x - 0.5).abs() < half {
            let grain = noise(fy * 40.0 + x * 6.0, 13) * 0.18;
            c = mix(
                [196.0, 168.0, 124.0],
                [150.0, 124.0, 88.0],
                grain + (fy - 0.7),
            );
        }
    }
    // Tall trees frame the path on both sides.
    for (cx, half, top) in [(0.1, 0.2, 0.0), (0.92, 0.22, -0.05), (0.26, 0.12, 0.22)] {
        if in_triangle((x, fy), (cx, top), (cx - half, 1.1), (cx + half, 1.1)) {
            c = mix([18.0, 48.0, 36.0], [30.0, 72.0, 52.0], fy * 0.6);
        }
    }
    c
}

fn bloom(fx: f32, fy: f32) -> Rgb {
    let (dx, dy) = (fx - 0.5, fy - 0.5);
    let r = (dx * dx + dy * dy).sqrt();
    let th = dy.atan2(dx);
    let mut c = mix([12.0, 50.0, 46.0], [32.0, 96.0, 78.0], fy);
    c = mix(c, [6.0, 28.0, 26.0], smooth(0.35, 0.75, r));
    for (rot, scale, near, far) in [
        (
            0.0_f32,
            1.0_f32,
            [196.0, 70.0, 128.0],
            [238.0, 140.0, 182.0],
        ),
        (
            std::f32::consts::PI / 9.0,
            0.78,
            [232.0, 96.0, 150.0],
            [252.0, 214.0, 230.0],
        ),
    ] {
        let petal = (0.5 + 0.5 * (9.0 * (th + rot)).cos()).powf(0.7);
        let reach = (0.17 + 0.22 * petal) * scale;
        if r < reach {
            let t = r / reach;
            let vein = ((9.0 * (th + rot)).cos().abs() * 18.0).min(1.0) * 0.12;
            c = mix(
                mix(near, far, t),
                [120.0, 30.0, 70.0],
                (0.12 - vein).max(0.0),
            );
        }
    }
    if r < 0.075 {
        let speck = hash((th * 40.0) as i32 + (r * 300.0) as i32, 21) * 0.25;
        c = mix([250.0, 198.0, 60.0], [196.0, 132.0, 20.0], speck + r * 3.0);
    }
    c
}

fn lake(fx: f32, fy: f32) -> Rgb {
    let horizon = 0.56;
    let ridge = |x: f32| 0.2 + 0.3 * noise(x * 3.0, 5) + 0.08 * noise(x * 9.0, 6);
    let mountain = |y: f32, mirrored: bool| {
        let peak = ridge(fx);
        if y >= peak && y <= horizon {
            let snow = smooth(peak + 0.07, peak, y);
            let rock = mix(
                [96.0, 108.0, 138.0],
                [60.0, 70.0, 100.0],
                smooth(peak, horizon, y),
            );
            let rock = if mirrored {
                mix(rock, [20.0, 40.0, 70.0], 0.35)
            } else {
                rock
            };
            Some(mix(
                rock,
                [244.0, 246.0, 250.0],
                snow * if mirrored { 0.6 } else { 1.0 },
            ))
        } else {
            None
        }
    };
    if fy <= horizon {
        let sky = mix(
            [132.0, 176.0, 232.0],
            [240.0, 226.0, 214.0],
            smooth(0.0, horizon, fy),
        );
        mountain(fy, false).unwrap_or(sky)
    } else {
        let water = mix(
            [60.0, 110.0, 160.0],
            [20.0, 52.0, 92.0],
            smooth(horizon, 1.0, fy),
        );
        let mirrored = 2.0 * horizon - fy;
        let reflected = mountain(mirrored, true).unwrap_or(water);
        let ripple = ((fy * 150.0) + noise(fx * 12.0, 7) * 8.0).sin() * 0.5 + 0.5;
        let mut c = mix(reflected, water, 0.25 + ripple * 0.15);
        for (cx, half, top) in [(0.06, 0.12, 0.7), (0.17, 0.1, 0.78), (0.94, 0.12, 0.72)] {
            if in_triangle((fx, fy), (cx, top), (cx - half, 1.05), (cx + half, 1.05)) {
                c = [16.0, 34.0, 36.0];
            }
        }
        c
    }
}

/// One picture of the demo set: a sunset, a harbour, a forest path, a flower
/// or a mountain lake, at `width` x `height`.
pub fn scene_image(width: u32, height: u32, kind: u8) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let [r, g, b] = match kind % 5 {
            0 => sunset(fx, fy),
            1 => harbour(fx, fy),
            2 => forest(fx, fy),
            3 => bloom(fx, fy),
            _ => lake(fx, fy),
        };
        *px = image::Rgba([r as u8, g as u8, b as u8, 255]);
    }
    img
}

/// A book cover: a lantern glowing on a night-blue ground.
pub fn cover_image(width: u32, height: u32) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let mut c = mix([12.0, 20.0, 46.0], [34.0, 44.0, 86.0], fy);
        let (dx, dy) = (fx - 0.5, (fy - 0.44) * 0.62);
        let d = (dx * dx + dy * dy).sqrt();
        c = mix(c, [236.0, 170.0, 70.0], (1.0 - smooth(0.0, 0.42, d)) * 0.5);
        // The lantern: a cap, a glass body, a base and a ring to carry it by.
        let ring = ((dx * dx + ((fy - 0.27) * 0.62).powi(2)).sqrt() - 0.06).abs() < 0.008;
        let cap = in_triangle((fx, fy), (0.5, 0.31), (0.39, 0.37), (0.61, 0.37));
        let body = (fx - 0.5).abs() < 0.085 && fy > 0.37 && fy < 0.55;
        let glass = (fx - 0.5).abs() < 0.06 && fy > 0.39 && fy < 0.53;
        let base = (fx - 0.5).abs() < 0.1 && (0.55..0.58).contains(&fy);
        if ring || cap || base || (body && !glass) {
            c = [18.0, 16.0, 24.0];
        }
        if glass {
            c = mix(
                [255.0, 232.0, 150.0],
                [246.0, 166.0, 60.0],
                smooth(0.39, 0.53, fy),
            );
        }
        // A gold rule near the foot of the cover.
        if (fy - 0.88).abs() < 0.004 && (fx - 0.5).abs() < 0.3 {
            c = [212.0, 170.0, 90.0];
        }
        *px = image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255]);
    }
    img
}

fn png(img: image::RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

/// [`cover_image`] as PNG bytes.
pub fn cover_png(width: u32, height: u32) -> Vec<u8> {
    png(cover_image(width, height))
}
