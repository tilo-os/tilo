//! Checking and completing a calibration with more than one microphone.
//!
//! What a speaker group needs is each speaker's delay and level *where the
//! listener sits*. The microphone at the listening position measures exactly
//! that: its reading of a speaker is the whole delay from the scheduled start
//! on the fabric clock (the member's output pipeline plus the acoustic path
//! to the seat), and the level there. That reading is the answer.
//!
//! Other microphones only help around it:
//!
//! * **Checking.** Every usable reading (SNR at least [`MIN_SNR_DB`]) is fit
//!   by a small additive model, `t[s,m] = a[s] + b[m] + r[s,m]`: a per-speaker
//!   term (output latency, member clock error), a per-microphone term (its
//!   clock offset, capture latency) and a residual (geometry, noise). The fit
//!   is a weighted median polish, so one bad reading cannot drag it. To check
//!   the listening microphone's readings, the worst seat reading further
//!   than [`ARRIVAL_OUTLIER_NS`] (15 ms) from the fit's `a[s] + b[L]` is set
//!   aside and the fit repeated: gross errors (a clock step, a missed direct
//!   sound) are tens of milliseconds, room geometry is a few. A seat reading
//!   is judged only when at least two other microphones heard its speaker
//!   and the seat kept two other speakers to anchor `b[L]`; a pair's two
//!   seat readings anchor only each other, so they always stand. Levels are
//!   checked the same way with [`LEVEL_OUTLIER_DB`].
//! * **Filling in.** A listening reading that is missing, too noisy or set
//!   aside is replaced by that prediction, which carries the other
//!   microphones' average geometry instead of the seat's own: accurate to the
//!   room's path differences, a few milliseconds at most. Without a
//!   prediction (no other microphone heard the speaker, or the listening
//!   microphone heard nothing else to anchor `b[L]`) the speaker is unknown.
//! * **Splitting.** A microphone on the speaker's own device hears it
//!   centimetres away, recording and playing on one clock, so fabric-clock
//!   error cancels: its reading is that device's output latency. The rest
//!   of the listening delay is acoustic time of flight.
//!
//! A reading's weight is its linear SNR, capped at [`MAX_SNR_DB`], discounted
//! by the error bound of its device's clock estimate. With only the listening
//! microphone the result is exactly its raw readings.

/// Readings with less SNR than this are ignored.
pub const MIN_SNR_DB: f64 = 10.0;
/// SNR beyond this adds no weight: other errors dominate by then.
pub const MAX_SNR_DB: f64 = 40.0;
/// A clock error bound of this much halves a reading's weight.
pub const CLOCK_SCALE_NS: f64 = 250_000.0;
/// A listening reading this far from the other microphones' prediction is a
/// gross error rather than geometry.
pub const ARRIVAL_OUTLIER_NS: f64 = 15_000_000.0;
/// The same for levels.
pub const LEVEL_OUTLIER_DB: f64 = 10.0;
/// Median polish passes; it settles in a handful.
const POLISH_PASSES: usize = 32;

/// What one microphone heard of one speaker's sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hearing {
    pub speaker: usize,
    pub microphone: usize,
    /// Arrival after the sweep's scheduled start, measured on the
    /// microphone's device against its fabric-clock mapping of the leader's
    /// clock.
    pub arrival_ns: f64,
    pub level_db: f64,
    pub snr_db: f64,
    /// Error bound of that device's leader-clock estimate at the sweep.
    pub clock_error_ns: f64,
}

/// One run's readings.
#[derive(Clone, Copy, Debug)]
pub struct FusionInput<'a> {
    pub speakers: usize,
    pub microphones: usize,
    pub hearings: &'a [Hearing],
    /// The microphone at the listening position.
    pub listening: usize,
    /// Per speaker: the microphone on that speaker's own device, if it takes
    /// part.
    pub own_microphone: &'a [Option<usize>],
}

/// One speaker as heard at the listening position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FusedSpeaker {
    pub arrival_ns: f64,
    pub level_db: f64,
    /// Taken from the listening microphone itself; false when that reading
    /// was missing, below [`MIN_SNR_DB`], or inconsistent with the other
    /// microphones and was estimated from them instead.
    pub at_listening: bool,
    /// The speaker device's output latency, heard by its own microphone
    /// (play and record share that device's clock, so fabric-clock error
    /// cancels).
    pub output_latency_ns: Option<f64>,
    /// `arrival_ns - output_latency_ns`: time of flight to the listening
    /// position.
    pub acoustic_ns: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fusion {
    /// One per speaker; `None` when it could not be placed at the listening
    /// position.
    pub speakers: Vec<Option<FusedSpeaker>>,
    /// `(speaker, microphone)` readings set aside.
    pub rejected: Vec<(usize, usize)>,
}

/// How much a reading counts: its linear SNR, capped, discounted by the
/// clock error bound. Zero below [`MIN_SNR_DB`] or for a non-finite input.
#[must_use]
pub fn weight(snr_db: f64, clock_error_ns: f64) -> f64 {
    if snr_db.is_nan() || snr_db < MIN_SNR_DB || !clock_error_ns.is_finite() {
        return 0.0;
    }
    let clock = clock_error_ns.max(0.0) / CLOCK_SCALE_NS;
    10.0_f64.powf(snr_db.min(MAX_SNR_DB) / 10.0) / clock.mul_add(clock, 1.0)
}

/// One usable reading of one quantity.
#[derive(Clone, Copy, Debug)]
struct Point {
    speaker: usize,
    microphone: usize,
    value: f64,
    weight: f64,
}

/// What the other microphones say the listening microphone should have
/// read for `speaker`: `a[s] + b[L]` from a median polish without that
/// reading. `None` without both terms.
fn predict(
    points: &[Point],
    speakers: usize,
    microphones: usize,
    speaker: usize,
    listening: usize,
) -> Option<f64> {
    let others = points
        .iter()
        .filter(|point| !(point.speaker == speaker && point.microphone == listening))
        .copied()
        .collect::<Vec<_>>();
    let (rows, columns) = polish(&others, speakers, microphones);
    Some(rows.get(speaker).copied().flatten()? + columns.get(listening).copied().flatten()?)
}

/// Weighted median polish of `value = row[speaker] + column[microphone]`.
fn polish(
    points: &[Point],
    speakers: usize,
    microphones: usize,
) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let mut rows = vec![None; speakers];
    let mut columns: Vec<Option<f64>> = (0..microphones)
        .map(|microphone| {
            points
                .iter()
                .any(|p| p.microphone == microphone)
                .then_some(0.0)
        })
        .collect();
    for _ in 0..POLISH_PASSES {
        for (speaker, row) in rows.iter_mut().enumerate() {
            let mut values = points
                .iter()
                .filter(|point| point.speaker == speaker)
                .filter_map(|point| {
                    let column = columns.get(point.microphone).copied().flatten()?;
                    Some((point.value - column, point.weight))
                })
                .collect::<Vec<_>>();
            *row = weighted_median(&mut values);
        }
        for (microphone, column) in columns.iter_mut().enumerate() {
            let mut values = points
                .iter()
                .filter(|point| point.microphone == microphone)
                .filter_map(|point| {
                    let row = rows.get(point.speaker).copied().flatten()?;
                    Some((point.value - row, point.weight))
                })
                .collect::<Vec<_>>();
            *column = weighted_median(&mut values);
        }
    }
    (rows, columns)
}

/// The value at half the total weight; the mean of the two middle values on
/// an exact split, so two equal-weight readings meet halfway.
fn weighted_median(values: &mut [(f64, f64)]) -> Option<f64> {
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total = values.iter().map(|value| value.1).sum::<f64>();
    if total <= 0.0 {
        return None;
    }
    let mut seen = 0.0;
    for (index, (value, weight)) in values.iter().enumerate() {
        seen += weight;
        if (seen - total / 2.0).abs() <= total * 1e-12 {
            let next = index.checked_add(1).and_then(|next| values.get(next));
            return Some(next.map_or(*value, |next| (value + next.0) / 2.0));
        }
        if seen > total / 2.0 {
            return Some(*value);
        }
    }
    values.last().map(|value| value.0)
}

/// Check one quantity's listening readings against the other microphones
/// and fill in the unusable ones. Per speaker: the value and whether it came
/// from the listening microphone.
///
/// Seat readings are set aside one at a time, worst first, from a fit of
/// every reading still kept. Only a reading the fit can judge is a
/// candidate: at least two other microphones heard its speaker, and the
/// listening microphone kept at least two other speakers to anchor its own
/// term. With a pair, the two seat readings anchor each other and neither
/// can be told wrong, so both stand.
fn resolve(
    input: &FusionInput<'_>,
    points: &[Point],
    threshold: f64,
    rejected: &mut Vec<(usize, usize)>,
) -> Vec<Option<(f64, bool)>> {
    let is_seat = |point: &Point| point.microphone == input.listening;
    let mut kept = points.to_vec();
    loop {
        let (rows, columns) = polish(&kept, input.speakers, input.microphones);
        let judgeable = |seat: &Point| {
            let others = kept
                .iter()
                .filter(|point| point.speaker == seat.speaker && !is_seat(point))
                .count();
            let anchors = kept
                .iter()
                .filter(|point| is_seat(point) && point.speaker != seat.speaker)
                .count();
            others >= 2 && anchors >= 2
        };
        let worst = kept
            .iter()
            .enumerate()
            .filter(|(_, point)| is_seat(point) && judgeable(point))
            .filter_map(|(index, point)| {
                let row = rows.get(point.speaker).copied().flatten()?;
                let column = columns.get(input.listening).copied().flatten()?;
                Some((index, (point.value - row - column).abs()))
            })
            .filter(|(_, residual)| *residual > threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((index, _)) = worst else {
            break;
        };
        let point = kept.remove(index);
        if !rejected.contains(&(point.speaker, point.microphone)) {
            rejected.push((point.speaker, point.microphone));
        }
    }
    (0..input.speakers)
        .map(|speaker| {
            if let Some(reading) = kept
                .iter()
                .find(|point| point.speaker == speaker && is_seat(point))
            {
                return Some((reading.value, true));
            }
            predict(
                &kept,
                input.speakers,
                input.microphones,
                speaker,
                input.listening,
            )
            .map(|predicted| (predicted, false))
        })
        .collect()
}

/// The listening microphone's reading of every speaker, checked and
/// completed by the others. See the module documentation for the model.
#[must_use]
pub fn fuse(input: &FusionInput<'_>) -> Fusion {
    let usable = input
        .hearings
        .iter()
        .filter(|hearing| {
            hearing.speaker < input.speakers
                && hearing.microphone < input.microphones
                && hearing.arrival_ns.is_finite()
                && hearing.level_db.is_finite()
        })
        .filter_map(|hearing| {
            let weight = weight(hearing.snr_db, hearing.clock_error_ns);
            (weight > 0.0).then_some((hearing, weight))
        })
        .collect::<Vec<_>>();
    let points = |value: fn(&Hearing) -> f64| {
        let mut seen = Vec::new();
        usable
            .iter()
            .filter(|(hearing, _)| {
                let key = (hearing.speaker, hearing.microphone);
                let first = !seen.contains(&key);
                seen.push(key);
                first
            })
            .map(|(hearing, weight)| Point {
                speaker: hearing.speaker,
                microphone: hearing.microphone,
                value: value(hearing),
                weight: *weight,
            })
            .collect::<Vec<_>>()
    };
    let arrivals = points(|hearing| hearing.arrival_ns);
    let levels = points(|hearing| hearing.level_db);
    let mut rejected = Vec::new();
    let arrival = resolve(input, &arrivals, ARRIVAL_OUTLIER_NS, &mut rejected);
    let level = resolve(input, &levels, LEVEL_OUTLIER_DB, &mut rejected);
    let speakers = arrival
        .iter()
        .zip(&level)
        .enumerate()
        .map(|(speaker, (arrival, level))| {
            let (arrival_ns, at_listening) = (*arrival)?;
            let (level_db, _) = (*level)?;
            let output_latency_ns = input
                .own_microphone
                .get(speaker)
                .copied()
                .flatten()
                .and_then(|own| {
                    arrivals
                        .iter()
                        .find(|point| point.speaker == speaker && point.microphone == own)
                })
                .map(|point| point.value);
            Some(FusedSpeaker {
                arrival_ns,
                level_db,
                at_listening,
                output_latency_ns,
                acoustic_ns: output_latency_ns.map(|latency| arrival_ns - latency),
            })
        })
        .collect();
    Fusion { speakers, rejected }
}

#[cfg(test)]
#[expect(
    clippy::cast_precision_loss,
    clippy::float_cmp,
    reason = "synthetic rooms over small, known ranges; zero weights are exact"
)]
mod tests {
    use super::*;

    const SOUND_M_PER_S: f64 = 343.0;
    /// The listening microphone in every room below.
    const SEAT: usize = 0;

    struct Room {
        speakers: Vec<[f64; 2]>,
        /// Output latency of each speaker's device.
        latency_ns: Vec<f64>,
        gain_db: Vec<f64>,
        microphones: Vec<[f64; 2]>,
        sensitivity_db: Vec<f64>,
        clock_offset_ns: Vec<f64>,
        /// Residual clock drift per microphone, over sweeps 5 s apart.
        drift_ppm: Vec<f64>,
    }

    fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
        (a[0] - b[0]).hypot(a[1] - b[1])
    }

    impl Room {
        /// Four speakers in a 5 × 4 m room; the seat is microphone 0 on the
        /// sofa, the others are phones and laptops within a metre of it.
        fn living_room() -> Self {
            let microphones = vec![[2.5, 2.5], [2.9, 2.7], [2.0, 2.8], [2.7, 1.9]];
            let count = microphones.len();
            Self {
                speakers: vec![[0.3, 0.3], [4.7, 0.3], [0.5, 3.7], [4.5, 3.6]],
                latency_ns: vec![2e6, 14e6, 7e6, 31e6],
                gain_db: vec![-3.0, -8.0, -5.0, -1.0],
                microphones,
                sensitivity_db: (0..count).map(|m| -2.0 * m as f64).collect(),
                clock_offset_ns: vec![0.0; count],
                drift_ppm: vec![0.0; count],
            }
        }

        fn hearings(&self) -> Vec<Hearing> {
            let mut out = Vec::new();
            for (s, speaker) in self.speakers.iter().enumerate() {
                let since_first_ns = s as f64 * 5e9;
                for (m, microphone) in self.microphones.iter().enumerate() {
                    let d = distance(*speaker, *microphone);
                    out.push(Hearing {
                        speaker: s,
                        microphone: m,
                        arrival_ns: self.latency_ns[s]
                            + d / SOUND_M_PER_S * 1e9
                            + self.clock_offset_ns[m]
                            + self.drift_ppm[m] * 1e-6 * since_first_ns,
                        level_db: self.gain_db[s] + self.sensitivity_db[m] - 20.0 * d.log10(),
                        snr_db: 35.0,
                        clock_error_ns: 50_000.0,
                    });
                }
            }
            out
        }

        /// What the seat's microphone truly reads of each speaker.
        fn truth(&self) -> Vec<(f64, f64)> {
            self.speakers
                .iter()
                .enumerate()
                .map(|(s, speaker)| {
                    let d = distance(*speaker, self.microphones[SEAT]);
                    (
                        self.latency_ns[s] + d / SOUND_M_PER_S * 1e9,
                        self.gain_db[s] + self.sensitivity_db[SEAT] - 20.0 * d.log10(),
                    )
                })
                .collect()
        }

        fn fuse_with(&self, hearings: &[Hearing], own: &[Option<usize>]) -> Fusion {
            fuse(&FusionInput {
                speakers: self.speakers.len(),
                microphones: self.microphones.len(),
                hearings,
                listening: SEAT,
                own_microphone: own,
            })
        }

        fn fuse(&self, hearings: &[Hearing]) -> Fusion {
            self.fuse_with(hearings, &vec![None; self.speakers.len()])
        }
    }

    fn assert_raw(fusion: &Fusion, room: &Room) {
        for (fused, (arrival, level)) in fusion.speakers.iter().zip(room.truth()) {
            let fused = fused.unwrap();
            assert!(fused.at_listening);
            assert!((fused.arrival_ns - arrival).abs() < 1e-3, "{fused:?}");
            assert!((fused.level_db - level).abs() < 1e-9, "{fused:?}");
        }
    }

    #[test]
    fn the_seat_alone_is_its_raw_readings() {
        let room = Room::living_room();
        let seat_only = room
            .hearings()
            .into_iter()
            .filter(|h| h.microphone == SEAT)
            .collect::<Vec<_>>();
        let fusion = room.fuse(&seat_only);
        assert!(fusion.rejected.is_empty());
        assert_raw(&fusion, &room);
    }

    #[test]
    fn consistent_extra_microphones_change_nothing() {
        let room = Room::living_room();
        let fusion = room.fuse(&room.hearings());
        assert!(fusion.rejected.is_empty());
        assert_raw(&fusion, &room);
    }

    #[test]
    fn clock_offsets_and_small_drift_elsewhere_cause_no_false_rejection() {
        let mut room = Room::living_room();
        room.clock_offset_ns = vec![0.0, -80e6, 45e6, 12e6];
        room.drift_ppm = vec![0.0, 20.0, -15.0, 50.0];
        let fusion = room.fuse(&room.hearings());
        assert!(fusion.rejected.is_empty(), "{:?}", fusion.rejected);
        assert_raw(&fusion, &room);
    }

    #[test]
    fn a_gross_error_at_the_seat_is_replaced_by_the_others() {
        let mut room = Room::living_room();
        room.clock_offset_ns = vec![0.0, -80e6, 45e6, 12e6];
        let mut hearings = room.hearings();
        hearings
            .iter_mut()
            .find(|h| h.speaker == 2 && h.microphone == SEAT)
            .unwrap()
            .arrival_ns += 40e6;
        let fusion = room.fuse(&hearings);
        assert_eq!(fusion.rejected, vec![(2, SEAT)]);
        let fused = fusion.speakers[2].unwrap();
        assert!(!fused.at_listening);
        // The stand-in carries the other microphones' geometry, all within a
        // metre of the seat: a metre of path is under 3 ms.
        let truth = room.truth()[2];
        assert!(
            (fused.arrival_ns - truth.0).abs() < 3e6,
            "{fused:?} vs {truth:?}"
        );
        assert!(
            (fused.level_db - truth.1).abs() < 1e-9,
            "its level was fine"
        );
        assert!(fusion.speakers[1].unwrap().at_listening);
    }

    #[test]
    fn a_pair_s_seat_readings_always_stand() {
        let mut room = Room::living_room();
        room.speakers.truncate(2);
        room.latency_ns.truncate(2);
        room.gain_db.truncate(2);
        let mut hearings = room.hearings();
        hearings
            .iter_mut()
            .find(|h| h.speaker == 1 && h.microphone == SEAT)
            .unwrap()
            .arrival_ns += 40e6;
        let fusion = room.fuse(&hearings);
        assert!(
            fusion.rejected.is_empty(),
            "two seat readings anchor only each other: {:?}",
            fusion.rejected
        );
        assert!(
            fusion
                .speakers
                .iter()
                .all(|fused| fused.unwrap().at_listening)
        );
    }

    #[test]
    fn a_gross_error_needs_two_other_witnesses() {
        let room = Room::living_room();
        let mut hearings = room
            .hearings()
            .into_iter()
            .filter(|h| h.microphone <= 1)
            .collect::<Vec<_>>();
        hearings
            .iter_mut()
            .find(|h| h.speaker == 2 && h.microphone == SEAT)
            .unwrap()
            .arrival_ns += 40e6;
        let fusion = room.fuse(&hearings);
        assert!(fusion.rejected.is_empty());
        assert!(fusion.speakers[2].unwrap().at_listening);
    }

    #[test]
    fn a_noisy_or_missing_seat_reading_is_filled_in() {
        let room = Room::living_room();
        let mut hearings = room.hearings();
        hearings
            .iter_mut()
            .find(|h| h.speaker == 1 && h.microphone == SEAT)
            .unwrap()
            .snr_db = MIN_SNR_DB - 1.0;
        hearings.retain(|h| !(h.speaker == 3 && h.microphone == SEAT));
        let fusion = room.fuse(&hearings);
        assert!(fusion.rejected.is_empty(), "ignored, not rejected");
        let truth = room.truth();
        for speaker in [1, 3] {
            let fused = fusion.speakers[speaker].unwrap();
            assert!(!fused.at_listening);
            assert!(
                (fused.arrival_ns - truth[speaker].0).abs() < 3e6,
                "{fused:?}"
            );
            assert!((fused.level_db - truth[speaker].1).abs() < 3.0, "{fused:?}");
        }
        assert!(fusion.speakers[0].unwrap().at_listening);
    }

    #[test]
    fn a_speaker_only_the_seat_missed_and_nobody_else_heard_is_unknown() {
        let room = Room::living_room();
        let hearings = room
            .hearings()
            .into_iter()
            .filter(|h| h.speaker != 2)
            .collect::<Vec<_>>();
        let fusion = room.fuse(&hearings);
        assert!(fusion.speakers[2].is_none());
        assert!(fusion.speakers[3].is_some());
    }

    #[test]
    fn the_own_microphone_splits_output_latency_from_flight_time() {
        let mut room = Room::living_room();
        // Speaker 1's device has a microphone 5 cm from its driver.
        room.microphones.push([4.65, 0.3]);
        room.sensitivity_db.push(0.0);
        room.clock_offset_ns.push(0.0);
        room.drift_ppm.push(0.0);
        let own = [None, Some(4), None, None];
        let fusion = room.fuse_with(&room.hearings(), &own);
        let fused = fusion.speakers[1].unwrap();
        let latency = fused.output_latency_ns.unwrap();
        assert!((latency - 14e6).abs() < 200_000.0, "{latency}");
        let flight = distance(room.speakers[1], room.microphones[SEAT]) / SOUND_M_PER_S * 1e9;
        assert!((fused.acoustic_ns.unwrap() - flight).abs() < 200_000.0);
        assert!(fusion.speakers[0].unwrap().output_latency_ns.is_none());
        assert!(fusion.rejected.is_empty());
    }

    #[test]
    fn a_noisy_extra_microphone_does_not_disturb_the_seat() {
        let room = Room::living_room();
        let mut hearings = room.hearings();
        for (index, hearing) in hearings
            .iter_mut()
            .filter(|h| h.microphone == 3)
            .enumerate()
        {
            hearing.arrival_ns += [9e6, -12e6, 30e6, -45e6][index];
            hearing.level_db += [6.0, -8.0, 15.0, -20.0][index];
            hearing.snr_db = 12.0;
        }
        let fusion = room.fuse(&hearings);
        assert!(fusion.rejected.is_empty(), "{:?}", fusion.rejected);
        assert_raw(&fusion, &room);
    }

    #[test]
    fn weights_follow_snr_and_clock_error() {
        assert_eq!(weight(MIN_SNR_DB - 0.1, 0.0), 0.0);
        assert_eq!(weight(f64::NAN, 0.0), 0.0);
        assert_eq!(weight(30.0, f64::INFINITY), 0.0);
        assert!(weight(30.0, 0.0) > weight(20.0, 0.0));
        assert!(weight(30.0, 0.0) > weight(30.0, 1e6));
        assert!((weight(30.0, CLOCK_SCALE_NS) - weight(30.0, 0.0) / 2.0).abs() < 1e-9);
        assert_eq!(weight(60.0, 0.0), weight(MAX_SNR_DB, 0.0));
    }
}
