//! Acoustic calibration of a speaker group from a measured sweep.
//!
//! Each member plays an exponential sine sweep at a scheduled time while a
//! microphone at the listening position records it. Convolving the recording
//! with the sweep's inverse filter (Farina's method) yields the speaker's
//! impulse response at the microphone, from which [`LogSweep::measure`] reads:
//!
//! * the arrival delay relative to the scheduled play time, with sub-sample
//!   precision from parabolic interpolation around the impulse peak;
//! * the level in dB, as the log-average of the magnitude response over the
//!   mid band, where room reflections average out;
//! * the magnitude response, smoothed to 1/6 octave;
//! * a room-EQ fit of at most eight peaking cuts below 500 Hz that flattens
//!   room modes toward that level, never boosting.
//!
//! [`CalibrationResult::align`] then turns the measurements of every member
//! into per-member trims: each member is delayed to the latest arrival and
//! attenuated to the quietest level, so the group sounds as one speaker.
//!
//! Harmonic distortion of the speaker lands at negative time in the
//! deconvolved response, ahead of the scheduled play time, so the measured
//! response window never contains it.

use std::f64::consts::PI;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Peak amplitude of the generated sweep: 6 dB of headroom for the playback
/// path.
pub const SWEEP_AMPLITUDE: f64 = 0.5;
/// Fade applied to each end of the sweep so it starts and stops without a
/// click.
const SWEEP_FADE_S: f64 = 0.005;
/// Longest arrival delay searched for: over 100 m of air.
pub const MAX_ARRIVAL_S: f64 = 0.3;
/// Part of the impulse response analysed for the magnitude response, from
/// just before the direct sound. Long enough to resolve 20 Hz room modes.
const RESPONSE_WINDOW_S: f64 = 0.25;
/// Response kept before the peak, so a smeared onset is not cut.
const PRE_PEAK_S: f64 = 0.002;
/// Band whose log-average magnitude is the speaker's level. Above the room
/// modes, below where directivity dominates.
const LEVEL_BAND_HZ: (f64, f64) = (300.0, 6_000.0);
/// Spacing of the reported response points, in octaves.
const RESPONSE_STEP_OCTAVES: f64 = 1.0 / 24.0;
/// Width of the smoothing window around each response point, in octaves.
const SMOOTHING_OCTAVES: f64 = 1.0 / 6.0;
/// A peak must be this much stronger than the impulse response's RMS to
/// count as an arrival rather than noise.
const MIN_PEAK_TO_RMS: f64 = 8.0;

/// Room-EQ bands a single member may use.
pub const EQ_MAX_BANDS: usize = 8;
/// Highest centre frequency of a room-EQ band: above this, response dips and
/// peaks depend on the listening position too much to correct.
pub const EQ_MAX_HZ: f64 = 500.0;
/// Deepest cut the room EQ applies at any frequency.
pub const EQ_MAX_CUT_DB: f64 = 12.0;
/// Excess over the target that is left alone.
const EQ_TOLERANCE_DB: f64 = 1.0;
/// Narrowest and widest room-EQ band.
const EQ_Q_RANGE: (f64, f64) = (0.5, 10.0);

/// Why a measurement could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    /// The sweep parameters are outside `0 < start < end < Nyquist`, or the
    /// sweep is shorter than 100 ms.
    InvalidSweep,
    /// The recording started after the sweep was scheduled to play.
    CaptureStartedLate,
    /// The recording ends before the whole sweep could have arrived.
    CaptureTooShort,
    /// No impulse stands out of the noise: the speaker is silent, muted or
    /// out of range of the microphone.
    NoArrival,
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidSweep => "sweep parameters are out of range",
            Self::CaptureStartedLate => "recording started after the sweep was played",
            Self::CaptureTooShort => "recording ends before the sweep could arrive",
            Self::NoArrival => "no arrival stands out of the noise",
        })
    }
}

impl std::error::Error for CalibrationError {}

/// An exponential sine sweep and its inverse filter.
#[derive(Clone, Debug)]
pub struct LogSweep {
    sample_rate: u32,
    start_hz: f64,
    end_hz: f64,
    signal: Vec<f32>,
    inverse: Vec<f32>,
}

impl LogSweep {
    /// A sweep from `start_hz` to `end_hz` lasting `duration_s`.
    ///
    /// # Errors
    ///
    /// [`CalibrationError::InvalidSweep`] unless
    /// `0 < start_hz < end_hz < sample_rate / 2` and
    /// `0.1 <= duration_s <= 60`.
    pub fn new(
        sample_rate: u32,
        start_hz: f64,
        end_hz: f64,
        duration_s: f64,
    ) -> Result<Self, CalibrationError> {
        let rate = f64::from(sample_rate);
        let valid = start_hz > 0.0 && start_hz < end_hz && end_hz < rate / 2.0;
        if !valid || !(0.1..=60.0).contains(&duration_s) {
            return Err(CalibrationError::InvalidSweep);
        }
        let count = round_to_usize(duration_s * rate);
        let rate_constant = duration_s / (end_hz / start_hz).ln();
        let fade = SWEEP_FADE_S * rate;
        let last = usize_to_f64(count.saturating_sub(1));
        let signal: Vec<f64> = (0..count)
            .map(|i| {
                let at = usize_to_f64(i);
                let t = at / rate;
                let phase = 2.0 * PI * start_hz * rate_constant * ((t / rate_constant).exp() - 1.0);
                let edge = at.min(last - at);
                let envelope = if edge < fade {
                    0.5 - 0.5 * (PI * edge / fade).cos()
                } else {
                    1.0
                };
                SWEEP_AMPLITUDE * envelope * phase.sin()
            })
            .collect();
        // Time-reversed, weighted in proportion to the instantaneous
        // frequency: the sweep dwells on each octave equally long, so its
        // energy per hertz falls 3 dB per octave, and the weight restores a
        // flat product.
        let mut inverse: Vec<f64> = signal
            .iter()
            .enumerate()
            .rev()
            .map(|(i, &sample)| sample * ((usize_to_f64(i) - last) / rate / rate_constant).exp())
            .collect();
        let size = count.saturating_mul(2).next_power_of_two();
        let product = multiply(&spectrum(&signal, size), &spectrum(&inverse, size));
        let (low, high) = mid_band(start_hz, end_hz);
        let gain = mean_magnitude(&product, band_bins(size, rate, low, high));
        if gain > 0.0 {
            for sample in &mut inverse {
                *sample /= gain;
            }
        }
        Ok(Self {
            sample_rate,
            start_hz,
            end_hz,
            signal: signal.into_iter().map(f64_to_f32).collect(),
            inverse: inverse.into_iter().map(f64_to_f32).collect(),
        })
    }

    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The sweep to play.
    pub fn signal(&self) -> &[f32] {
        &self.signal
    }

    /// The impulse response contained in `capture`, starting `pre` samples
    /// before the whole sample at or before the scheduled play time, which
    /// lies `play_offset` samples into the capture. Also returns the fraction
    /// of a sample by which that whole sample precedes the play time.
    ///
    /// Samples before the play time come from the convolution's leading
    /// half, which holds only the (zero) response to silence and, much
    /// earlier, harmonic distortion; they let a peak at zero delay keep its
    /// leading side.
    fn impulse_response(&self, capture: &[f32], play_offset: f64, pre: usize) -> (Vec<f64>, f64) {
        let capture: Vec<f64> = capture.iter().map(|&s| f64::from(s)).collect();
        let inverse: Vec<f64> = self.inverse.iter().map(|&s| f64::from(s)).collect();
        let size = capture
            .len()
            .saturating_add(inverse.len())
            .next_power_of_two();
        let convolved = inverse_spectrum(multiply(
            &spectrum(&capture, size),
            &spectrum(&inverse, size),
        ));
        let whole = play_offset.floor();
        let start = self
            .signal
            .len()
            .saturating_sub(1)
            .saturating_add(round_to_usize(whole))
            .saturating_sub(pre);
        let rate = f64::from(self.sample_rate);
        let len = round_to_usize((MAX_ARRIVAL_S + RESPONSE_WINDOW_S) * rate).saturating_add(pre);
        let response = convolved.into_iter().skip(start).take(len).collect();
        (response, play_offset - whole)
    }

    /// Measures one speaker from a recording of its sweep.
    ///
    /// `capture` starts at local time `capture_start_ns`; the sweep was
    /// scheduled to leave the speaker at local time `play_at_ns`.
    ///
    /// # Errors
    ///
    /// When the recording does not cover the sweep, or no arrival stands out
    /// of its noise.
    pub fn measure(
        &self,
        capture: &[f32],
        capture_start_ns: i64,
        play_at_ns: i64,
    ) -> Result<SpeakerMeasurement, CalibrationError> {
        let rate = f64::from(self.sample_rate);
        let lead_ns = play_at_ns.saturating_sub(capture_start_ns);
        if lead_ns < 0 {
            return Err(CalibrationError::CaptureStartedLate);
        }
        let play_offset = i64_to_f64(lead_ns) * rate / 1e9;
        if usize_to_f64(capture.len()) < play_offset + usize_to_f64(self.signal.len()) {
            return Err(CalibrationError::CaptureTooShort);
        }
        let pre = round_to_usize(PRE_PEAK_S * rate);
        let (response, fraction) = self.impulse_response(capture, play_offset, pre);
        let search = round_to_usize(MAX_ARRIVAL_S * rate)
            .saturating_add(pre)
            .min(response.len());
        let (peak, peak_value) =
            response
                .iter()
                .take(search)
                .enumerate()
                .fold((0, 0.0_f64), |best, (i, &v)| {
                    if v.abs() > best.1 { (i, v.abs()) } else { best }
                });
        let rms = (response.iter().map(|v| v * v).sum::<f64>()
            / usize_to_f64(response.len().max(1)))
        .sqrt();
        if peak_value <= 0.0 || peak_value < MIN_PEAK_TO_RMS * rms {
            return Err(CalibrationError::NoArrival);
        }
        let arrival_samples =
            usize_to_f64(peak) + parabolic_offset(&response, peak) - usize_to_f64(pre) - fraction;

        let length = round_to_usize(RESPONSE_WINDOW_S * rate);
        let windowed: Vec<f64> = response
            .iter()
            .skip(peak.saturating_sub(pre))
            .take(length)
            .enumerate()
            .map(|(i, &v)| v * tail_fade(i, length))
            .collect();
        let magnitude =
            MagnitudeResponse::from_impulse(&windowed, rate, self.start_hz, self.end_hz);
        let (low, high) = mid_band(self.start_hz, self.end_hz);
        let level_db =
            magnitude.log_average_db(low.max(LEVEL_BAND_HZ.0), high.min(LEVEL_BAND_HZ.1));
        let eq = fit_room_eq(&magnitude, level_db, self.sample_rate);
        Ok(SpeakerMeasurement {
            arrival_samples,
            arrival_ns: round_to_i64(arrival_samples / rate * 1e9),
            level_db,
            snr_db: 20.0 * (peak_value / rms.max(f64::MIN_POSITIVE)).log10(),
            response: magnitude,
            eq,
        })
    }
}

/// What one speaker sounds like at the microphone.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeakerMeasurement {
    /// Delay from the scheduled play time to the direct sound, in samples.
    pub arrival_samples: f64,
    /// The same delay in nanoseconds.
    pub arrival_ns: i64,
    /// Level relative to a speaker that reproduces the sweep at unity gain.
    pub level_db: f64,
    /// How far the direct sound's peak stands above the impulse response's
    /// RMS, in dB: how much a fused result should trust this reading.
    pub snr_db: f64,
    pub response: MagnitudeResponse,
    /// Cuts that flatten this speaker's room modes toward `level_db`.
    pub eq: Vec<EqBand>,
}

/// A point of a smoothed magnitude response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponsePoint {
    pub hz: f64,
    pub db: f64,
}

/// A magnitude response smoothed to 1/6 octave, sampled every 1/24 octave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MagnitudeResponse {
    pub points: Vec<ResponsePoint>,
}

impl MagnitudeResponse {
    /// The smoothed response of `impulse` between `start_hz` and `end_hz`.
    fn from_impulse(impulse: &[f64], rate: f64, start_hz: f64, end_hz: f64) -> Self {
        let size = impulse.len().max(16_384).next_power_of_two();
        let power: Vec<f64> = spectrum(impulse, size)
            .iter()
            .take(size / 2)
            .map(Complex::norm_sqr)
            .collect();
        let bin_hz = rate / usize_to_f64(size);
        let half_width = 2.0_f64.powf(SMOOTHING_OCTAVES / 2.0);
        let steps = round_to_usize((end_hz / start_hz).log2() / RESPONSE_STEP_OCTAVES);
        let points = (0..=steps)
            .map(|step| {
                let hz = start_hz * 2.0_f64.powf(usize_to_f64(step) * RESPONSE_STEP_OCTAVES);
                let low = round_to_usize((hz / half_width / bin_hz).ceil());
                let high = round_to_usize((hz * half_width / bin_hz).floor());
                let band = power.get(low..=high.max(low)).filter(|_| high >= low);
                let mean = band.map_or_else(
                    || {
                        power
                            .get(round_to_usize(hz / bin_hz))
                            .copied()
                            .unwrap_or(0.0)
                    },
                    |band| band.iter().sum::<f64>() / usize_to_f64(band.len().max(1)),
                );
                ResponsePoint {
                    hz,
                    db: 10.0 * mean.max(1e-20).log10(),
                }
            })
            .collect();
        Self { points }
    }

    /// The response on the grid [`LogSweep::measure`] reports for a sweep
    /// from `start_hz` to `end_hz`, from its dB values alone. `None` when the
    /// count does not match that grid or a value is not finite.
    #[must_use]
    pub fn from_grid_db(start_hz: f64, end_hz: f64, db: &[f64]) -> Option<Self> {
        if !(start_hz > 0.0 && end_hz > start_hz) || db.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let steps = round_to_usize((end_hz / start_hz).log2() / RESPONSE_STEP_OCTAVES);
        (db.len() == steps.checked_add(1)?).then(|| Self {
            points: db
                .iter()
                .enumerate()
                .map(|(step, &db)| ResponsePoint {
                    hz: start_hz * 2.0_f64.powf(usize_to_f64(step) * RESPONSE_STEP_OCTAVES),
                    db,
                })
                .collect(),
        })
    }

    /// The dB-domain weighted mean of responses on one grid: a multi-position
    /// room response. `None` when there is nothing to average, the weights
    /// sum to zero, or the grids differ.
    #[must_use]
    pub fn weighted_mean(responses: &[(&Self, f64)]) -> Option<Self> {
        let (first, _) = responses.first()?;
        let total: f64 = responses.iter().map(|(_, weight)| weight.max(0.0)).sum();
        let same_grid = responses.iter().all(|(response, _)| {
            response.points.len() == first.points.len()
                && response
                    .points
                    .iter()
                    .zip(&first.points)
                    .all(|(a, b)| (a.hz - b.hz).abs() <= 1e-9 * b.hz.abs().max(1.0))
        });
        if total <= 0.0 || !total.is_finite() || !same_grid {
            return None;
        }
        let points = first
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| ResponsePoint {
                hz: point.hz,
                db: responses
                    .iter()
                    .filter_map(|(response, weight)| {
                        response.points.get(index).map(|p| p.db * weight.max(0.0))
                    })
                    .sum::<f64>()
                    / total,
            })
            .collect();
        Some(Self { points })
    }

    /// Mean of the dB values of the points between `low_hz` and `high_hz`.
    pub fn log_average_db(&self, low_hz: f64, high_hz: f64) -> f64 {
        let (sum, count) = self
            .points
            .iter()
            .filter(|p| (low_hz..=high_hz).contains(&p.hz))
            .fold((0.0, 0_u32), |(sum, count), p| {
                (sum + p.db, count.saturating_add(1))
            });
        if count == 0 {
            0.0
        } else {
            sum / f64::from(count)
        }
    }

    /// The smoothed level at `hz`, interpolated between points; `None` past
    /// the highest point.
    pub fn db_at(&self, hz: f64) -> Option<f64> {
        let after = self.points.iter().position(|p| p.hz >= hz)?;
        let high = self.points.get(after)?;
        let Some(low) = after.checked_sub(1).and_then(|i| self.points.get(i)) else {
            return Some(high.db);
        };
        let t = (hz / low.hz).log2() / (high.hz / low.hz).log2();
        Some(low.db + (high.db - low.db) * t)
    }
}

/// Normalized biquad coefficients (`a0 = 1`), for a transposed direct form II
/// filter: `y = b0·x + z1; z1 = b1·x − a1·y + z2; z2 = b2·x − a2·y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoefficients {
    /// Gain in dB at `hz`.
    pub fn magnitude_db(&self, hz: f64, sample_rate: u32) -> f64 {
        let w = 2.0 * PI * hz / f64::from(sample_rate);
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let numerator =
            (self.b0 + self.b1 * cos1 + self.b2 * cos2).hypot(self.b1 * sin1 + self.b2 * sin2);
        let denominator =
            (1.0 + self.a1 * cos1 + self.a2 * cos2).hypot(self.a1 * sin1 + self.a2 * sin2);
        20.0 * (numerator / denominator).log10()
    }
}

/// One peaking filter of a room EQ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub center_hz: f64,
    /// Zero or negative in a fitted room EQ: it only cuts.
    pub gain_db: f64,
    pub q: f64,
}

impl EqBand {
    /// Peaking-filter coefficients from the audio EQ cookbook.
    pub fn coefficients(&self, sample_rate: u32) -> BiquadCoefficients {
        let amplitude = 10.0_f64.powf(self.gain_db / 40.0);
        let w0 = 2.0 * PI * self.center_hz / f64::from(sample_rate);
        let alpha = w0.sin() / (2.0 * self.q);
        let cos = w0.cos();
        let a0 = 1.0 + alpha / amplitude;
        BiquadCoefficients {
            b0: (1.0 + alpha * amplitude) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * amplitude) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / amplitude) / a0,
        }
    }
}

/// The combined gain of `bands` at `hz`, in dB.
pub fn eq_db(bands: &[EqBand], hz: f64, sample_rate: u32) -> f64 {
    bands
        .iter()
        .map(|band| band.coefficients(sample_rate).magnitude_db(hz, sample_rate))
        .sum()
}

/// Fits up to [`EQ_MAX_BANDS`] peaking cuts below [`EQ_MAX_HZ`] that bring the
/// response down toward `target_db`. The octave above the response's first
/// point is left alone: it holds the sweep's fade-in, not the room.
///
/// Greedy: each band goes on the largest remaining excess, as deep as that
/// excess (at most [`EQ_MAX_CUT_DB`] including overlapping bands) and as wide
/// as the part of it above half its height. Dips are never filled: boosting
/// into a room null wastes amplifier power without raising what is heard.
#[must_use]
pub fn room_eq(response: &MagnitudeResponse, level_db: f64, sample_rate: u32) -> Vec<EqBand> {
    fit_room_eq(response, level_db, sample_rate)
}

/// The same fit as [`room_eq`].
pub fn fit_room_eq(response: &MagnitudeResponse, target_db: f64, sample_rate: u32) -> Vec<EqBand> {
    let lowest = response.points.first().map_or(0.0, |p| p.hz);
    let points: Vec<ResponsePoint> = response
        .points
        .iter()
        .copied()
        .filter(|p| p.hz <= EQ_MAX_HZ && p.hz >= lowest * 2.0)
        .collect();
    let mut bands: Vec<EqBand> = Vec::new();
    while bands.len() < EQ_MAX_BANDS {
        let excess: Vec<f64> = points
            .iter()
            .map(|p| p.db + eq_db(&bands, p.hz, sample_rate) - target_db)
            .collect();
        let Some((peak, &height)) = excess.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))
        else {
            break;
        };
        let Some(center) = points.get(peak) else {
            break;
        };
        let applied = eq_db(&bands, center.hz, sample_rate);
        let depth = height.min(EQ_MAX_CUT_DB + applied);
        if height <= EQ_TOLERANCE_DB || depth < EQ_TOLERANCE_DB / 2.0 {
            break;
        }
        let half = height / 2.0;
        let fallback = 2.0_f64.powf(SMOOTHING_OCTAVES);
        let lower = excess
            .iter()
            .take(peak)
            .rposition(|&e| e < half)
            .and_then(|i| points.get(i))
            .map_or(center.hz / fallback, |p| p.hz);
        let upper = excess
            .iter()
            .skip(peak)
            .position(|&e| e < half)
            .and_then(|i| points.get(peak.saturating_add(i)))
            .map_or(center.hz * fallback, |p| p.hz);
        let octaves = (upper / lower).log2().max(SMOOTHING_OCTAVES);
        let ratio = 2.0_f64.powf(octaves);
        let q = (ratio.sqrt() / (ratio - 1.0)).clamp(EQ_Q_RANGE.0, EQ_Q_RANGE.1);
        bands.push(EqBand {
            center_hz: center.hz,
            gain_db: -depth,
            q,
        });
    }
    bands
}

/// The trims one member applies.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberCalibration {
    /// Extra playout delay: how much earlier this member's sound arrives
    /// than the latest member's.
    pub delay_trim_ns: i64,
    /// Gain change in dB, zero or negative: how much louder this member is
    /// than the quietest.
    pub gain_trim_db: f64,
    pub eq: Vec<EqBand>,
}

/// Trims that make a group's members arrive together at the same level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationResult {
    /// One entry per measurement, in the order given.
    pub per_member: Vec<MemberCalibration>,
}

impl CalibrationResult {
    /// Aligns every member to the latest arrival and the quietest level.
    pub fn align(measurements: &[SpeakerMeasurement]) -> Self {
        let latest = measurements.iter().map(|m| m.arrival_ns).max().unwrap_or(0);
        let quietest = measurements
            .iter()
            .map(|m| m.level_db)
            .reduce(f64::min)
            .unwrap_or(0.0);
        Self {
            per_member: measurements
                .iter()
                .map(|m| MemberCalibration {
                    delay_trim_ns: latest.saturating_sub(m.arrival_ns),
                    gain_trim_db: quietest - m.level_db,
                    eq: m.eq.clone(),
                })
                .collect(),
        }
    }
}

/// Band well inside the sweep, where its spectrum is flat.
fn mid_band(start_hz: f64, end_hz: f64) -> (f64, f64) {
    (start_hz * 2.0, end_hz / 2.0)
}

/// Fraction of a sample by which the true peak lies after `peak`, from a
/// parabola through the magnitudes around it.
fn parabolic_offset(signal: &[f64], peak: usize) -> f64 {
    let at = |i: Option<usize>| i.and_then(|i| signal.get(i)).map_or(0.0, |v| v.abs());
    let (before, center, after) = (
        at(peak.checked_sub(1)),
        at(Some(peak)),
        at(peak.checked_add(1)),
    );
    let curvature = before - 2.0 * center + after;
    if curvature.abs() < f64::EPSILON {
        0.0
    } else {
        (0.5 * (before - after) / curvature).clamp(-0.5, 0.5)
    }
}

/// Half-Hann fade over the last quarter of a window of `len` samples.
fn tail_fade(index: usize, len: usize) -> f64 {
    let fade = len / 4;
    let from_end = len.saturating_sub(index);
    if from_end >= fade || fade == 0 {
        1.0
    } else {
        0.5 - 0.5 * (PI * usize_to_f64(from_end) / usize_to_f64(fade)).cos()
    }
}

fn spectrum(signal: &[f64], size: usize) -> Vec<Complex<f64>> {
    let mut buffer: Vec<Complex<f64>> = signal
        .iter()
        .take(size)
        .map(|&s| Complex::new(s, 0.0))
        .collect();
    buffer.resize(size, Complex::new(0.0, 0.0));
    FftPlanner::new()
        .plan_fft_forward(size)
        .process(&mut buffer);
    buffer
}

fn inverse_spectrum(mut spectrum: Vec<Complex<f64>>) -> Vec<f64> {
    let size = spectrum.len();
    FftPlanner::new()
        .plan_fft_inverse(size)
        .process(&mut spectrum);
    let scale = 1.0 / usize_to_f64(size.max(1));
    spectrum.into_iter().map(|c| c.re * scale).collect()
}

fn multiply(a: &[Complex<f64>], b: &[Complex<f64>]) -> Vec<Complex<f64>> {
    a.iter()
        .zip(b)
        .map(|(x, y)| Complex::new(x.re * y.re - x.im * y.im, x.re * y.im + x.im * y.re))
        .collect()
}

/// Indices of the bins of a `size`-point spectrum between `low` and `high` Hz.
fn band_bins(size: usize, rate: f64, low: f64, high: f64) -> std::ops::RangeInclusive<usize> {
    let bin_hz = rate / usize_to_f64(size);
    round_to_usize(low / bin_hz)..=round_to_usize(high / bin_hz)
}

fn mean_magnitude(spectrum: &[Complex<f64>], bins: std::ops::RangeInclusive<usize>) -> f64 {
    let band = spectrum.get(bins).unwrap_or(&[]);
    band.iter().map(|c| c.norm()).sum::<f64>() / usize_to_f64(band.len().max(1))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "sample counts stay far below 2^52"
)]
const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a recording's lead in nanoseconds stays far below 2^52"
)]
const fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "audio samples are narrowed to the output format on purpose"
)]
const fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a saturating float-to-int cast is the intended rounding"
)]
fn round_to_i64(value: f64) -> i64 {
    value.round() as i64
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a saturating float-to-int cast is the intended rounding; negatives clamp to 0"
)]
fn round_to_usize(value: f64) -> usize {
    value.round().max(0.0) as usize
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "test signal synthesis over small, known ranges"
)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// The recording starts this long before the sweep is scheduled.
    const LEAD_NS: i64 = 100_000_000;
    const CAPTURE_START_NS: i64 = 7_000_000_000;

    struct Rng(u64);

    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1_u64 << 53) as f64 * 2.0 - 1.0
        }
    }

    /// A synthetic room: the sweep delayed by `delay` samples (the fraction
    /// through a windowed-sinc interpolator), scaled by `gain_db`, with two
    /// early reflections, an optional resonance, and white noise at -60 dBFS.
    fn room(
        sweep: &LogSweep,
        delay: f64,
        gain_db: f64,
        mode: Option<EqBand>,
        seed: u64,
    ) -> Vec<f32> {
        let gain = 10.0_f64.powf(gain_db / 20.0);
        let mut fir = vec![0.0; 256];
        let whole = delay.floor();
        let fraction = delay - whole;
        for (reflection, amplitude) in [(0, 1.0), (37, 0.3), (113, -0.2)] {
            for tap in -32_i32..=32 {
                let t = f64::from(tap) - fraction;
                let sinc = if t.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * t).sin() / (PI * t)
                };
                let window = 0.5 + 0.5 * (PI * t / 33.0).cos();
                let index = 40 + reflection + tap;
                fir[index as usize] += amplitude * gain * sinc * window;
            }
        }
        let lead = (LEAD_NS as f64 * f64::from(RATE) / 1e9) as usize;
        let offset = lead + whole as usize - 40;
        let signal = sweep.signal();
        let length = lead + signal.len() + RATE as usize / 2;
        let mut out = vec![0.0_f64; length];
        let taps: Vec<(usize, f64)> = fir
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, h)| h.abs() > 0.0)
            .collect();
        for (i, &s) in signal.iter().enumerate() {
            for &(k, h) in &taps {
                out[offset + i + k] += f64::from(s) * h;
            }
        }
        if let Some(mode) = mode {
            let c = mode.coefficients(RATE);
            let (mut z1, mut z2) = (0.0, 0.0);
            for sample in &mut out {
                let x = *sample;
                let y = c.b0 * x + z1;
                z1 = c.b1 * x - c.a1 * y + z2;
                z2 = c.b2 * x - c.a2 * y;
                *sample = y;
            }
        }
        let mut rng = Rng(seed);
        out.iter()
            .map(|&s| (s + 0.001 * rng.unit()) as f32)
            .collect()
    }

    fn sweep() -> LogSweep {
        LogSweep::new(RATE, 20.0, 20_000.0, 1.0).unwrap()
    }

    fn measure(sweep: &LogSweep, capture: &[f32]) -> SpeakerMeasurement {
        sweep
            .measure(capture, CAPTURE_START_NS, CAPTURE_START_NS + LEAD_NS)
            .unwrap()
    }

    #[test]
    fn responses_rebuild_from_their_db_values_and_average_by_weight() {
        let sweep = sweep();
        let measured = measure(&sweep, &room(&sweep, 150.0, -4.0, None, 3));
        assert!(measured.snr_db > 20.0 * MIN_PEAK_TO_RMS.log10());
        let db: Vec<f64> = measured.response.points.iter().map(|p| p.db).collect();
        let rebuilt = MagnitudeResponse::from_grid_db(20.0, 20_000.0, &db).unwrap();
        for (a, b) in rebuilt.points.iter().zip(&measured.response.points) {
            assert!((a.hz - b.hz).abs() < 1e-9 && a.db.to_bits() == b.db.to_bits());
        }
        assert!(MagnitudeResponse::from_grid_db(20.0, 20_000.0, &db[1..]).is_none());
        assert!(MagnitudeResponse::from_grid_db(20.0, 20_000.0, &[f64::NAN; 1]).is_none());
        let louder = MagnitudeResponse::from_grid_db(
            20.0,
            20_000.0,
            &db.iter().map(|db| db + 6.0).collect::<Vec<_>>(),
        )
        .unwrap();
        let mean = MagnitudeResponse::weighted_mean(&[(&rebuilt, 2.0), (&louder, 1.0)]).unwrap();
        assert!((mean.points[10].db - (db[10] + 2.0)).abs() < 1e-9);
        assert!(MagnitudeResponse::weighted_mean(&[]).is_none());
        assert!(MagnitudeResponse::weighted_mean(&[(&rebuilt, 0.0)]).is_none());
        assert_eq!(
            room_eq(&rebuilt, measured.level_db, RATE),
            measured.eq,
            "the same fit as measuring"
        );
    }

    #[test]
    fn the_inverse_filter_turns_the_sweep_into_a_flat_impulse() {
        let sweep = sweep();
        let capture = [
            vec![0.0; 4_800].as_slice(),
            sweep.signal(),
            &vec![0.0; 24_000],
        ]
        .concat();
        let m = measure(&sweep, &capture);
        assert!(m.arrival_samples.abs() < 0.05, "{}", m.arrival_samples);
        assert!(m.level_db.abs() < 0.1, "{}", m.level_db);
        // Flat within half a decibel over the mid band; the analysis window
        // and the sweep's low edge allow a little more deep in the bass.
        for point in &m.response.points {
            let tolerance = match point.hz {
                hz if (200.0..10_000.0).contains(&hz) => 0.5,
                hz if (40.0..200.0).contains(&hz) => 1.0,
                _ => continue,
            };
            assert!(point.db.abs() < tolerance, "{point:?}");
        }
        assert!(m.eq.is_empty(), "{:?}", m.eq);
    }

    #[test]
    fn measures_delay_and_gain_through_a_room() {
        let sweep = sweep();
        for (delay, gain_db) in [(1_234.0, -6.0), (517.3, -17.5), (96.5, 3.0)] {
            let m = measure(&sweep, &room(&sweep, delay, gain_db, None, 1));
            assert!(
                (m.arrival_samples - delay).abs() < 0.25,
                "delay {delay}: {}",
                m.arrival_samples
            );
            let expected_ns = (delay / f64::from(RATE) * 1e9) as i64;
            assert!((m.arrival_ns - expected_ns).abs() < 21_000);
            assert!(
                (m.level_db - gain_db).abs() < 0.5,
                "gain {gain_db}: {}",
                m.level_db
            );
        }
    }

    #[test]
    fn room_eq_cuts_an_80_hz_mode() {
        let sweep = sweep();
        let mode = EqBand {
            center_hz: 80.0,
            gain_db: 10.0,
            q: 5.0,
        };
        let m = measure(&sweep, &room(&sweep, 700.0, -10.0, Some(mode), 2));
        assert!((m.level_db + 10.0).abs() < 0.5, "{}", m.level_db);
        assert!(!m.eq.is_empty() && m.eq.len() <= EQ_MAX_BANDS);
        for band in &m.eq {
            assert!(
                band.gain_db <= 0.0 && band.gain_db >= -EQ_MAX_CUT_DB,
                "{band:?}"
            );
            assert!(band.center_hz <= EQ_MAX_HZ, "{band:?}");
            assert!((EQ_Q_RANGE.0..=EQ_Q_RANGE.1).contains(&band.q), "{band:?}");
        }
        assert!(
            m.eq.iter().any(|b| (70.0..90.0).contains(&b.center_hz)),
            "{:?}",
            m.eq
        );
        let excess = |with_eq: bool| {
            m.response
                .points
                .iter()
                .filter(|p| (60.0..100.0).contains(&p.hz))
                .map(|p| {
                    let eq = if with_eq {
                        eq_db(&m.eq, p.hz, RATE)
                    } else {
                        0.0
                    };
                    p.db + eq - m.level_db
                })
                .fold(f64::MIN, f64::max)
        };
        let before = excess(false);
        let after = excess(true);
        assert!(before > 8.0, "mode not measured: {before} dB");
        assert!(after < 2.0, "mode left at {after} dB (was {before} dB)");
        assert!(m.response.db_at(80.0).unwrap() > m.level_db + 8.0);
    }

    #[test]
    fn alignment_delays_early_members_and_attenuates_loud_ones() {
        let sweep = sweep();
        let near = measure(&sweep, &room(&sweep, 150.0, -4.0, None, 3));
        let far = measure(&sweep, &room(&sweep, 900.0, -9.0, None, 4));
        let result = CalibrationResult::align(&[near.clone(), far.clone()]);
        let [near_trim, far_trim] = result.per_member.as_slice() else {
            panic!("{result:?}");
        };
        assert_eq!(far_trim.delay_trim_ns, 0);
        assert!(far_trim.gain_trim_db.abs() < f64::EPSILON);
        let sample_ns = 1e9 / f64::from(RATE);
        let expected_delay = (750.0 * sample_ns) as i64;
        assert!((near_trim.delay_trim_ns - expected_delay).abs() < sample_ns as i64);
        assert!((near_trim.gain_trim_db + 5.0).abs() < 0.5);
        assert_eq!(
            near.arrival_ns + near_trim.delay_trim_ns,
            far.arrival_ns + far_trim.delay_trim_ns
        );
    }

    #[test]
    fn rejects_unusable_input() {
        assert_eq!(
            LogSweep::new(RATE, 20.0, 30_000.0, 1.0).err(),
            Some(CalibrationError::InvalidSweep)
        );
        let sweep = LogSweep::new(RATE, 50.0, 16_000.0, 0.5).unwrap();
        let silence = vec![0.0; RATE as usize];
        assert_eq!(
            sweep.measure(&silence, 0, 1).err(),
            Some(CalibrationError::NoArrival)
        );
        assert_eq!(
            sweep.measure(&silence, 10, 0).err(),
            Some(CalibrationError::CaptureStartedLate)
        );
        assert_eq!(
            sweep.measure(&silence[..20_000], 0, 0).err(),
            Some(CalibrationError::CaptureTooShort)
        );
    }
}
