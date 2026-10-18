//! [`IccLut`] against qcms, on a real adapter, through the production SDR final pass.
//!
//! Each panel is synthetic and fully known: qcms reads it as an ICC profile (matrix and
//! curves, or a lut16 `B2A0` table built here), and the same model turns the panel's 8-bit
//! signals back into light, so a difference between the GPU and qcms is measured as ΔE2000
//! on that panel, not only in code values.

use qcms::{CIE_xyY, CIE_xyYTRIPLE, DataType, Intent, Profile, Transform};
use settings_store::hardware::OutputTransform;
use vello::wgpu;

use crate::shell::render::icc_lut::IccLut;
use crate::shell::render::orientation::OrientedCopy;

/// A panel's tone curve: signal (0..1) to linear light (0..1).
#[derive(Clone, Copy)]
pub(crate) enum Curve {
    Srgb,
    /// As qcms takes it.
    Gamma(f32),
    /// A measured-looking calibration: gamma 2.2 with a ripple, stored as a 1024-entry table.
    Ripple,
    /// Gamma 2.2 with green and red crosstalk, reachable only through a lut16 `B2A0` table.
    Lut16,
}

impl Curve {
    fn light(self, signal: f64) -> f64 {
        match self {
            Self::Srgb => {
                if signal <= 0.040_45 {
                    signal / 12.92
                } else {
                    ((signal + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Gamma(gamma) => signal.powf(f64::from(gamma)),
            Self::Ripple => (signal.powf(2.2)
                * (1.0 + 0.04 * (signal * 12.0).sin() * signal * (1.0 - signal)))
                .clamp(0.0, 1.0),
            Self::Lut16 => signal.powf(2.2),
        }
    }
}

/// A synthetic display: what qcms reads as its profile, and the model ΔE is measured on.
pub(crate) struct Panel {
    pub(crate) name: &'static str,
    white: (f64, f64),
    primaries: [(f64, f64); 3],
    curve: Curve,
    /// The largest per-channel difference from qcms, in 8-bit code values, this panel may
    /// show (`IccLut`'s stated tolerance).
    pub(crate) max_code_delta: i32,
    /// The share of colours that must land within one code value of qcms.
    pub(crate) min_within_one: f64,
}

const D65: (f64, f64) = (0.3127, 0.3290);
const D50: (f64, f64) = (0.3457, 0.3585);
const P3: [(f64, f64); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];

/// Profiles a calibrated desktop meets: in-gamut matrix profiles, a table curve, a lut-based
/// profile, and a panel narrower than sRGB that qcms clips into.
pub(crate) fn representative_panels() -> [Panel; 7] {
    [
        Panel {
            name: "sRGB",
            white: D65,
            primaries: [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)],
            curve: Curve::Srgb,
            max_code_delta: 1,
            min_within_one: 1.0,
        },
        Panel {
            name: "Display P3",
            white: D65,
            primaries: P3,
            curve: Curve::Srgb,
            max_code_delta: 1,
            min_within_one: 1.0,
        },
        adobe_rgb(),
        Panel {
            name: "BT.2020 primaries, gamma 2.4",
            white: D65,
            primaries: [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)],
            curve: Curve::Gamma(2.4),
            max_code_delta: 1,
            min_within_one: 1.0,
        },
        Panel {
            name: "calibration table, D50",
            white: D50,
            primaries: P3,
            curve: Curve::Ripple,
            max_code_delta: 1,
            min_within_one: 1.0,
        },
        lut16(),
        Panel {
            name: "narrow gamut, gamma 1.8 (clipped)",
            white: D65,
            primaries: [(0.59, 0.35), (0.33, 0.56), (0.155, 0.11)],
            curve: Curve::Gamma(1.8),
            max_code_delta: 10,
            min_within_one: 0.975,
        },
    ]
}

/// A lut-based profile, as calibration tools write them: qcms reads its `B2A0` table, which
/// it interpolates on its own 33-point grid. [`Panel::lut16_profile`] is its ICC file.
pub(crate) fn lut16() -> Panel {
    Panel {
        name: "lut16 B2A0",
        white: D50,
        primaries: P3,
        curve: Curve::Lut16,
        max_code_delta: 3,
        min_within_one: 0.998,
    }
}

/// Adobe RGB (1998) primaries at gamma 2.2: a wide-gamut matrix profile.
pub(crate) fn adobe_rgb() -> Panel {
    Panel {
        name: "Adobe RGB, gamma 2.2",
        white: D65,
        primaries: [(0.64, 0.33), (0.21, 0.71), (0.15, 0.06)],
        curve: Curve::Gamma(2.2),
        max_code_delta: 1,
        min_within_one: 1.0,
    }
}

fn xy((x, y): (f64, f64)) -> CIE_xyY {
    CIE_xyY { x, y, Y: 1.0 }
}

impl Panel {
    /// The panel as qcms reads it.
    pub(crate) fn profile(&self) -> Box<Profile> {
        let primaries = CIE_xyYTRIPLE {
            red: xy(self.primaries[0]),
            green: xy(self.primaries[1]),
            blue: xy(self.primaries[2]),
        };
        let profile = match self.curve {
            Curve::Gamma(gamma) => {
                Profile::new_rgb_with_gamma_set(xy(self.white), primaries, gamma, gamma, gamma)
            }
            Curve::Lut16 => Profile::new_from_slice(&self.lut16_profile(), false),
            curve => {
                let table: Vec<u16> = (0..1024)
                    .map(|i| to_u16(curve.light(f64::from(i) / 1023.0)))
                    .collect();
                Profile::new_rgb_with_table(xy(self.white), primaries, &table)
            }
        };
        profile.unwrap_or_else(|| panic!("qcms reads the {} profile", self.name))
    }

    /// sRGB to this panel, as the readback presenter builds it.
    pub(crate) fn transform(&self) -> Transform {
        Transform::new(
            &Profile::new_sRGB(),
            &self.profile(),
            DataType::RGBA8,
            Intent::Perceptual,
        )
        .unwrap_or_else(|| panic!("qcms builds the {} transform", self.name))
    }

    /// Linear RGB to XYZ, scaled so white has Y = 1.
    fn rgb_to_xyz(&self) -> [[f64; 3]; 3] {
        let column = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
        let [r, g, b] = self.primaries.map(column);
        let primaries = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let scale = solve(primaries, column(self.white));
        primaries.map(|row| [row[0] * scale[0], row[1] * scale[1], row[2] * scale[2]])
    }

    /// CIELAB, relative to the panel's white, of the colour the panel shows for `signal`.
    fn lab(&self, signal: [u8; 3]) -> [f64; 3] {
        let m = self.rgb_to_xyz();
        let light = signal.map(|c| self.curve.light(f64::from(c) / 255.0));
        let xyz = m.map(|row| row[0] * light[0] + row[1] * light[1] + row[2] * light[2]);
        let white = m.map(|row| row[0] + row[1] + row[2]);
        let f = |t: f64| {
            let delta: f64 = 6.0 / 29.0;
            if t > delta.powi(3) {
                t.cbrt()
            } else {
                t / (3.0 * delta * delta) + 4.0 / 29.0
            }
        };
        let [fx, fy, fz] = [0, 1, 2].map(|i| f(xyz[i] / white[i]));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    /// An ICC v2 display profile whose only device mapping is a 33-point lut16 `B2A0`: XYZ
    /// (D50 PCS) through the inverse matrix and curve, then a crosstalk no matrix can express.
    pub(crate) fn lut16_profile(&self) -> Vec<u8> {
        const GRID: usize = 33;
        let inverse = invert(self.rgb_to_xyz());
        let mut tag = Vec::new();
        tag.extend_from_slice(b"mft2");
        tag.extend_from_slice(&[0; 4]);
        tag.extend_from_slice(&[3, 3, 33, 0]);
        for entry in 0..9 {
            let identity: i32 = if entry % 4 == 0 { 0x1_0000 } else { 0 };
            tag.extend_from_slice(&identity.to_be_bytes());
        }
        // Two-entry identity input and output curves.
        tag.extend_from_slice(&2_u16.to_be_bytes());
        tag.extend_from_slice(&2_u16.to_be_bytes());
        let identity_curves = [0_u16, u16::MAX].repeat(3);
        for value in &identity_curves {
            tag.extend_from_slice(&value.to_be_bytes());
        }
        // lut16 XYZ: 1.0 + 32767/32768 at full scale.
        let full_scale = 1.0 + 32767.0 / 32768.0;
        for x in 0..GRID {
            for y in 0..GRID {
                for z in 0..GRID {
                    let xyz = [x, y, z].map(|i| {
                        f64::from(u32::try_from(i).unwrap())
                            / f64::from(u32::try_from(GRID - 1).unwrap())
                            * full_scale
                    });
                    let signal = inverse.map(|row| {
                        (row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2])
                            .clamp(0.0, 1.0)
                            .powf(1.0 / 2.2)
                    });
                    let [r, g, b] = signal;
                    for value in [
                        r + 0.06 * r * (1.0 - r) * (g - b),
                        g + 0.05 * g * (1.0 - g) * (b - r),
                        b,
                    ] {
                        tag.extend_from_slice(&to_u16(value).to_be_bytes());
                    }
                }
            }
        }
        for value in &identity_curves {
            tag.extend_from_slice(&value.to_be_bytes());
        }
        while tag.len() % 4 != 0 {
            tag.push(0);
        }
        let tag_offset: u32 = 128 + 4 + 12;
        let total = tag_offset.saturating_add(u32::try_from(tag.len()).unwrap());
        let mut profile = vec![0_u8; 128];
        profile[0..4].copy_from_slice(&total.to_be_bytes());
        profile[8..12].copy_from_slice(&0x0210_0000_u32.to_be_bytes());
        profile[12..16].copy_from_slice(b"mntr");
        profile[16..20].copy_from_slice(b"RGB ");
        profile[20..24].copy_from_slice(b"XYZ ");
        profile[36..40].copy_from_slice(b"acsp");
        profile.extend_from_slice(&1_u32.to_be_bytes());
        profile.extend_from_slice(b"B2A0");
        profile.extend_from_slice(&tag_offset.to_be_bytes());
        profile.extend_from_slice(&u32::try_from(tag.len()).unwrap().to_be_bytes());
        profile.extend_from_slice(&tag);
        profile
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=65535 first"
)]
fn to_u16(value: f64) -> u16 {
    (value.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// `m · x = v`, by Cramer's rule.
fn solve(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let det = |a: [[f64; 3]; 3]| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let whole = det(m);
    [0, 1, 2].map(|column| {
        let mut replaced = m;
        for (row, value) in replaced.iter_mut().zip(v) {
            row[column] = value;
        }
        det(replaced) / whole
    })
}

fn invert(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let columns = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|unit| solve(m, unit));
    [0, 1, 2].map(|row| [columns[0][row], columns[1][row], columns[2][row]])
}

/// CIEDE2000 between two CIELAB colours.
fn delta_e_2000(first: [f64; 3], second: [f64; 3]) -> f64 {
    let [l1, a1, b1] = first;
    let [l2, a2, b2] = second;
    let chroma_mean = f64::midpoint(a1.hypot(b1), a2.hypot(b2));
    let g = 0.5 * (1.0 - (chroma_mean.powi(7) / (chroma_mean.powi(7) + 25_f64.powi(7))).sqrt());
    let (a1, a2) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let (c1, c2) = (a1.hypot(b1), a2.hypot(b2));
    let hue = |b: f64, a: f64| {
        if b == 0.0 && a == 0.0 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        }
    };
    let (h1, h2) = (hue(b1, a1), hue(b2, a2));
    let chromatic = c1 * c2 != 0.0;
    let dh = if !chromatic {
        0.0
    } else if (h2 - h1).abs() <= 180.0 {
        h2 - h1
    } else if h2 > h1 {
        h2 - h1 - 360.0
    } else {
        h2 - h1 + 360.0
    };
    let dl = l2 - l1;
    let dc = c2 - c1;
    let dhh = 2.0 * (c1 * c2).sqrt() * (dh.to_radians() / 2.0).sin();
    let l_mean = f64::midpoint(l1, l2);
    let c_mean = f64::midpoint(c1, c2);
    let h_mean = if !chromatic {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        f64::midpoint(h1, h2)
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) / 2.0
    } else {
        (h1 + h2 - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (h_mean - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_mean).to_radians().cos()
        + 0.32 * (3.0 * h_mean + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_mean - 63.0).to_radians().cos();
    let rotation = 30.0 * (-((h_mean - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (c_mean.powi(7) / (c_mean.powi(7) + 25_f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_mean - 50.0).powi(2) / (20.0 + (l_mean - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_mean;
    let sh = 1.0 + 0.015 * c_mean * t;
    let rt = -(2.0 * rotation.to_radians()).sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhh / sh).powi(2) + rt * (dc / sc) * (dhh / sh))
        .sqrt()
}

/// Every colour on a lattice of step 3 (0, 3, …, 255 in each channel) and every grey.
pub(crate) fn sample_colours() -> Vec<[u8; 3]> {
    let lattice: Vec<u8> = (0..=u8::MAX).step_by(3).collect();
    let mut colours = Vec::new();
    for &b in &lattice {
        for &g in &lattice {
            for &r in &lattice {
                colours.push([r, g, b]);
            }
        }
    }
    colours.extend((0..=u8::MAX).map(|grey| [grey; 3]));
    colours
}

/// Width of the strip of colours the GPU pass reads; one row is 4096 bytes, so rows need no
/// padding for the readback.
const STRIP_WIDTH: u32 = 1024;

/// `colours` through the production SDR final pass with `lut` bound, on `device`.
pub(crate) fn calibrate_on_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    lut: &IccLut,
    colours: &[[u8; 3]],
) -> Vec<[u8; 3]> {
    let count = u32::try_from(colours.len()).unwrap();
    let extent = wgpu::Extent3d {
        width: STRIP_WIDTH,
        height: count.div_ceil(STRIP_WIDTH),
        depth_or_array_layers: 1,
    };
    let texture = |label, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    // Vello's target format, and the scanout's.
    let source = texture(
        "icc-lut-test-source",
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let target = texture(
        "icc-lut-test-target",
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let mut texels: Vec<u8> = colours
        .iter()
        .flat_map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    texels.resize(
        usize::try_from(STRIP_WIDTH.saturating_mul(extent.height).saturating_mul(4)).unwrap(),
        0,
    );
    let layout = wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(STRIP_WIDTH * 4),
        rows_per_image: Some(extent.height),
    };
    queue.write_texture(source.as_image_copy(), &texels, layout, extent);
    let mut pass = OrientedCopy::new(device);
    pass.prepare(device, queue, OutputTransform::Normal, Some(lut), None);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("icc-lut-test-readback"),
        size: u64::try_from(texels.len()).unwrap(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    pass.copy(
        device,
        &mut encoder,
        &source.create_view(&wgpu::TextureViewDescriptor::default()),
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        Some(lut),
    );
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout,
        },
        extent,
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(tilo_test_env::hang_bound()),
        })
        .expect("the GPU finishes the pass");
    rx.recv().expect("map callback").expect("map succeeds");
    let mapped = slice.get_mapped_range();
    mapped
        .chunks_exact(4)
        .take(colours.len())
        .map(|bgra| [bgra[2], bgra[1], bgra[0]])
        .collect()
}

/// How far a calibration lands from qcms.
#[derive(Debug)]
pub(crate) struct Agreement {
    /// The largest per-channel difference, in 8-bit code values, and the input colour.
    pub(crate) max_code_delta: (i32, [u8; 3]),
    /// Share of colours within one code value in every channel.
    pub(crate) within_one: f64,
    pub(crate) mean_delta_e: f64,
    /// The largest ΔE2000 on the panel, and the input colour.
    pub(crate) max_delta_e: (f64, [u8; 3]),
}

/// `got[i]` (the panel signal for `colours[i]`) against qcms's.
pub(crate) fn agreement(
    panel: &Panel,
    transform: &Transform,
    colours: &[[u8; 3]],
    got: &[[u8; 3]],
) -> Agreement {
    let mut expected: Vec<u8> = colours
        .iter()
        .flat_map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    transform.apply(&mut expected);
    let mut result = Agreement {
        max_code_delta: (0, [0; 3]),
        within_one: 0.0,
        mean_delta_e: 0.0,
        max_delta_e: (0.0, [0; 3]),
    };
    let (mut within_one, mut delta_e_sum) = (0_u32, 0.0);
    for ((colour, want), got) in colours.iter().zip(expected.chunks_exact(4)).zip(got) {
        let want = [want[0], want[1], want[2]];
        let delta = (0..3)
            .map(|c| i32::from(got[c].abs_diff(want[c])))
            .max()
            .unwrap();
        if delta > result.max_code_delta.0 {
            result.max_code_delta = (delta, *colour);
        }
        within_one = within_one.saturating_add(u32::from(delta <= 1));
        if delta > 0 {
            let delta_e = delta_e_2000(panel.lab(want), panel.lab(*got));
            delta_e_sum += delta_e;
            if delta_e > result.max_delta_e.0 {
                result.max_delta_e = (delta_e, *colour);
            }
        }
    }
    let count = f64::from(u32::try_from(colours.len()).unwrap());
    result.within_one = f64::from(within_one) / count;
    result.mean_delta_e = delta_e_sum / count;
    result
}

/// Mean ΔE2000 every panel stays within: well under one just-noticeable difference.
pub(crate) const MAX_MEAN_DELTA_E: f64 = 0.15;
/// The largest ΔE2000 any colour may show: a one-code step in a near-black, near-neutral
/// colour, the panel's own quantisation.
pub(crate) const MAX_DELTA_E: f64 = 3.5;

#[test]
fn gpu_lut_matches_qcms_across_representative_profiles() {
    use pollster::FutureExt as _;
    let mut context = ui::gpu::RenderContext::new();
    let id = context.device(None).block_on().expect("real GPU adapter");
    let gpu = &context.devices[id];
    let colours = sample_colours();
    for panel in representative_panels() {
        let transform = panel.transform();
        let lut = IccLut::from_transform(&transform);
        let got = calibrate_on_gpu(&gpu.device, &gpu.queue, &lut, &colours);
        let agreement = agreement(&panel, &transform, &colours, &got);
        eprintln!("{}: {agreement:?}", panel.name);
        assert!(
            agreement.max_code_delta.0 <= panel.max_code_delta,
            "{}: {agreement:?}",
            panel.name
        );
        assert!(
            agreement.within_one >= panel.min_within_one,
            "{}: {agreement:?}",
            panel.name
        );
        assert!(
            agreement.mean_delta_e <= MAX_MEAN_DELTA_E,
            "{}: {agreement:?}",
            panel.name
        );
        assert!(
            agreement.max_delta_e.0 <= MAX_DELTA_E,
            "{}: {agreement:?}",
            panel.name
        );
    }
}

#[test]
fn the_lut_samples_qcms_exactly_at_its_grid_points() {
    // Greys that are grid codes come back as qcms maps them, untouched by interpolation.
    use pollster::FutureExt as _;
    let mut context = ui::gpu::RenderContext::new();
    let id = context.device(None).block_on().expect("real GPU adapter");
    let gpu = &context.devices[id];
    let panel = adobe_rgb();
    let transform = panel.transform();
    let lut = IccLut::from_transform(&transform);
    let colours = [
        [0, 0, 0],
        [255, 255, 255],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
    ];
    let got = calibrate_on_gpu(&gpu.device, &gpu.queue, &lut, &colours);
    let agreement = agreement(&panel, &transform, &colours, &got);
    assert_eq!(agreement.max_code_delta.0, 0, "{agreement:?}");
}
