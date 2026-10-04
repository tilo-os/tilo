//! What a staged session shows: the terminal of a machine someone is
//! working on and the desktop of a remote computer. Screenshot scenarios
//! stage these in place of a transport, so a capture shows a connected
//! session the way a real one looks, and the same every run.

use crate::vt::VtGrid;

/// The size an SSH session asks the remote PTY for.
pub(crate) const TERMINAL_SIZE: (usize, usize) = (80, 24);

/// A shell on the build server, a few commands into the morning.
pub(crate) fn terminal() -> VtGrid {
    let (cols, rows) = TERMINAL_SIZE;
    let mut grid = VtGrid::new(cols, rows);
    let prompt = "\x1b[1;32madmin@build\x1b[0m:\x1b[1;34m~\x1b[0m$ ";
    let session = [
        "Last login: Tue Jun 30 10:29:14 2026 from 10.0.4.31".to_owned(),
        format!("{prompt}uptime"),
        " 10:42:07 up 23 days,  4:11,  2 users,  load average: 1.84, 1.62, 1.40".to_owned(),
        format!("{prompt}systemctl status ci-runner --no-pager"),
        "\x1b[1;32m\u{25cf}\x1b[0m ci-runner.service - CI job runner".to_owned(),
        "     Loaded: loaded (/etc/systemd/system/ci-runner.service; enabled)".to_owned(),
        "     Active: \x1b[1;32mactive (running)\x1b[0m since Sun 2026-06-07 06:31:52 UTC; 3 weeks ago"
            .to_owned(),
        "   Main PID: 1184 (ci-runner)".to_owned(),
        "      Tasks: 38 (limit: 76918)".to_owned(),
        "     Memory: 2.1G".to_owned(),
        "        CPU: 4d 6h 12min".to_owned(),
        format!("{prompt}df -h /srv"),
        "Filesystem      Size  Used Avail Use% Mounted on".to_owned(),
        "/dev/nvme1n1p1  1.8T  1.1T  644G  63% /srv".to_owned(),
        format!("{prompt}ls ~/builds"),
        "\x1b[1;34m2026-06-29-nightly\x1b[0m  \x1b[1;34m2026-06-30-nightly\x1b[0m  \
         \x1b[1;34mrelease-0.9.4\x1b[0m  \x1b[1;34mrelease-0.9.5\x1b[0m"
            .to_owned(),
    ];
    for line in session {
        grid.feed(line.as_bytes());
        grid.feed(b"\r\n");
    }
    grid.feed(prompt.as_bytes());
    grid
}

/// The desktop size a remote computer shares.
pub(crate) const DESKTOP_SIZE: (u32, u32) = (1440, 900);

/// A colour stop of the wallpaper, in sRGB.
type Rgb = [f64; 3];

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// The skyline of one mountain range at column `x` (0..1), as a share of
/// the screen height from the top.
fn ridge(x: f64, base: f64, scale: f64, phase: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    base - scale
        * (0.55 * (x * tau * 1.3 + phase).sin()
            + 0.30 * (x * tau * 3.1 + phase * 1.7).sin()
            + 0.15 * (x * tau * 7.9 + phase * 2.3).sin())
}

/// An idle desktop: a mountain range at dusk filling the screen, as
/// RGBA rows of [`DESKTOP_SIZE`].
pub(crate) fn desktop() -> Vec<u8> {
    let (width, height) = DESKTOP_SIZE;
    let (w, h) = (f64::from(width), f64::from(height));
    // Sky from the zenith to the horizon, then three ranges from the
    // farthest (hazy, lit by the sky) to the nearest (dark).
    let zenith: Rgb = [28.0, 36.0, 78.0];
    let middle: Rgb = [104.0, 92.0, 150.0];
    let horizon: Rgb = [242.0, 164.0, 116.0];
    let sun: Rgb = [255.0, 226.0, 178.0];
    let ranges: [(f64, f64, f64, Rgb); 3] = [
        (0.58, 0.06, 0.4, [148.0, 112.0, 150.0]),
        (0.68, 0.07, 2.1, [84.0, 66.0, 110.0]),
        (0.80, 0.08, 4.4, [34.0, 30.0, 58.0]),
    ];
    let sun_centre = (0.66 * w, 0.55 * h);
    let sun_radius = 0.045 * h;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let skylines: Vec<[f64; 3]> = (0..width)
        .map(|x| {
            let x = f64::from(x) / w;
            ranges.map(|(base, scale, phase, _)| ridge(x, base, scale, phase) * h)
        })
        .collect();
    for y in 0..height {
        let fy = f64::from(y);
        let t = fy / (0.62 * h);
        let sky = if t < 0.5 {
            mix(zenith, middle, t * 2.0)
        } else {
            mix(middle, horizon, (t - 0.5) * 2.0)
        };
        for (x, skyline) in skylines.iter().enumerate() {
            let fx = x as f64;
            let distance =
                ((fx - sun_centre.0).powi(2) + (fy - sun_centre.1).powi(2)).sqrt() / sun_radius;
            let glow = (1.0 - (distance - 1.0) / 6.0).clamp(0.0, 1.0).powi(2) * 0.5;
            let mut colour = if distance <= 1.0 {
                sun
            } else {
                mix(sky, sun, glow)
            };
            for ((_, _, _, tone), top) in ranges.iter().zip(skyline) {
                if fy >= *top {
                    // Each range darkens a little toward its foot.
                    let depth = ((fy - top) / (0.25 * h)).clamp(0.0, 1.0);
                    colour = mix(*tone, mix(*tone, [12.0, 12.0, 24.0], 0.35), depth);
                }
            }
            pixels.extend(colour.map(|c| c.round().clamp(0.0, 255.0) as u8));
            pixels.push(u8::MAX);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_staged_surfaces_are_complete() {
        let grid = terminal();
        let lines = grid.lines();
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.cells.iter().map(|c| c.ch).collect())
            .collect();
        assert!(text.iter().any(|line| line.contains("load average")));
        assert!(
            text.iter()
                .all(|line| line.chars().count() <= TERMINAL_SIZE.0),
            "every line fits the PTY width"
        );
        assert_eq!(grid.cursor(), (15, 16), "the caret waits after the prompt");

        let (width, height) = DESKTOP_SIZE;
        let pixels = desktop();
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert!(pixels.chunks_exact(4).all(|px| px[3] == u8::MAX), "opaque");
    }
}
