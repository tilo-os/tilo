//! How a frame reaches the outputs and is proven on the panel: the
//! presenter that submits it, the pending page flips it retires, and the
//! phase window it reports its timing through.

use std::time::{Duration, Instant};

use super::paint;
use super::paint::BridgePhaseTiming;
use super::runtime::ScanoutCommand;
use super::scanout;
use super::{
    DirectOutcome, DirectRender, DrmPresenter, FlipVblank, FlipWait, OutputId, OwedFlip,
    PaintScanoutSample, ScanoutFrame, demote_direct, direct_failures, direct_reason, direct_status,
    fastest_direct_refresh, flip_wait, paint_scanouts, physical_flip_proof, publish_client_planes,
    quiesce_presentations, refresh_of, report_signal_status, route_flip_completions,
};
use tilo_lock::MutexRecover as _;

impl DrmPresenter {
    /// The only way a frame reaches the scanouts from the render thread: every [`Presenter`]
    /// entry point that presents lands here.
    ///
    /// The present gate is held for the whole frame, flip retirement included, so dropping
    /// DRM master for a VT switch (`DrmDispatchState::perform(SessionAction::DropMaster)`)
    /// waits until no output's page flip is queued or in flight. A `Pause` queued before
    /// that drop is drained here under the same gate, so the frame after it paints nothing.
    fn submit(
        &mut self,
        frame: ScanoutFrame<'_>,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        self.frame_phases = None;
        self.display_timing = crate::shell::present::DisplayTiming::default();
        self.flip_vblanks.clear();
        if let Some(bridge) = &mut self.paint {
            let _ = bridge.take_phase_timing();
        }
        self.pointer = *self.shared_pointer.lock_recover();
        self.frame_owes = false;
        let present_gate = self.present_gate.clone();
        let _quiesced = quiesce_presentations(present_gate.as_ref());
        self.drain_commands();
        self.service_links();
        let overlap = !self.blanked && !self.paused && self.overlap_eligible();
        if self.owed.is_some() && (!overlap || frame.scanout.is_some()) {
            // Leaving the overlapped mode, or a client buffer may take the CRTC (nothing
            // renders that could overlap the wait): the owed flip retires first, as every
            // flip did before frames overlapped.
            self.collect_owed(DIRECT_RETIREMENT_TIMEOUT);
        }
        self.overlapping = overlap;
        if self.blanked || self.paused {
            return None;
        }
        if overlap {
            return self.submit_overlapping(frame);
        }
        let before = self
            .scanouts
            .first()
            .map(|entry| entry.scanout.flip_submission());
        let sample = paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            &mut self.cpu,
            self.pointer,
            frame,
            Some(&mut self.phase_window),
        );
        let flip_queued_at = sample.direct_queued.then(Instant::now);
        let retirement_started = self.profile_phases.then(Instant::now);
        let retirement_attempted = sample.direct_queued
            || (self.scanouts.len() == 1
                && self
                    .scanouts
                    .first()
                    .map(|entry| entry.scanout.flip_submission())
                    != before);
        // A slower output's flip from an earlier frame is checked as well: one that is overdue
        // is demoted even on a frame that queued nothing.
        let physical = if sample.direct_queued || self.direct_flip_pending() {
            self.retire_direct_flips()
        } else if self.scanouts.len() == 1
            && self
                .scanouts
                .first()
                .map(|entry| entry.scanout.flip_submission())
                != before
        {
            self.retire_physical_flip(0, DIRECT_RETIREMENT_TIMEOUT)
        } else {
            None
        };
        if let Some(started) = retirement_started {
            self.frame_phases = self
                .paint
                .as_ref()
                .and_then(paint::VelloDrmBridge::last_phase_timing)
                .map(|paint| crate::shell::present::FramePhaseTiming {
                    paint,
                    kms_retirement_us: if retirement_attempted {
                        started.elapsed().as_micros()
                    } else {
                        0
                    },
                });
        }
        self.display_timing = self.frame_display_timing(flip_queued_at);
        self.emit_phase_profile(sample);
        if let Some(bridge) = &mut self.paint {
            let _ = bridge.take_phase_timing();
        }
        report_signal_status(&mut self.scanouts, self.signal_reports.as_ref());
        publish_client_planes(&mut self.scanouts, &self.client_planes);
        physical
    }

    /// [`Self::submit`] for one vblank-paced output whose rendering overlaps the previous
    /// frame's flip: render into the spare slot while that flip is pending, queue this frame
    /// once it retired, and return without waiting for this frame's own flip, which the pipe
    /// collects through [`Presenter::retire_owed`]. The GPU-completion wait stays: vello's
    /// bump-allocator counters must be read back before a frame may be shown, and a legacy
    /// page flip takes no in-fence (an atomic `IN_FENCE_FD` would need the atomic API and a
    /// sync-file export wgpu does not offer).
    fn submit_overlapping(
        &mut self,
        frame: ScanoutFrame<'_>,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        // The vblank may already have passed: retire the owed flip from its event, before
        // the frame's own non-blocking drain consumes it without its timestamp.
        self.collect_owed(Duration::ZERO);
        for entry in &mut self.scanouts {
            if let Some(targets) = entry.direct_targets.as_mut() {
                targets.set_overlap(true);
            }
        }
        let mut sample = paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            &mut self.cpu,
            self.pointer,
            frame,
            Some(&mut self.phase_window),
        );
        let mut kms_wait_us = 0;
        if let Some((output, render)) = sample.overlapped.take() {
            let (queued, waited) = self.submit_overlapped(output, render);
            sample.direct_queued |= queued;
            kms_wait_us = waited;
        }
        for entry in &mut self.scanouts {
            if let Some(targets) = entry.direct_targets.as_mut() {
                targets.set_overlap(false);
            }
        }
        if sample.direct_queued {
            // An owed flip still unresolved here retired unseen: a newer one is queued.
            if let Some(owed) = self.owed {
                self.resolve_owed(owed, None);
            }
            self.owed = self.scanouts.first().map(|entry| OwedFlip {
                device_id: entry.device_id,
                crtc: entry.scanout.crtc(),
                queued_at: Instant::now(),
                submission: entry.scanout.flip_submission(),
                interval: refresh_of(&self.outputs, entry.id)
                    .and_then(|rate| 1_000_000_000_000_u64.checked_div(u64::from(rate)))
                    .map(Duration::from_nanos),
            });
            self.frame_owes = self.owed.is_some();
        }
        if self.profile_phases {
            self.frame_phases = self
                .paint
                .as_ref()
                .and_then(paint::VelloDrmBridge::last_phase_timing)
                .map(|paint| crate::shell::present::FramePhaseTiming {
                    paint,
                    // The render thread's wait for the previous flip, the part of KMS
                    // retirement still on the critical path.
                    kms_retirement_us: kms_wait_us,
                });
        }
        // A frame that queued nothing (an unchanged frame) retires at once; the pipe
        // holds it behind the owed flip it is shown with.
        self.flip_vblanks.clear();
        self.display_timing = self.frame_display_timing(None);
        self.emit_phase_profile(sample);
        if let Some(bridge) = &mut self.paint {
            let _ = bridge.take_phase_timing();
        }
        report_signal_status(&mut self.scanouts, self.signal_reports.as_ref());
        None
    }

    /// Tighten `Presenter::present_frame`'s completion boundary to the KMS page-flip events of
    /// the outputs that pace the loop: those at the fastest refresh (`flip_wait`).
    /// `PresentPipe` only frees producer capacity after this method returns, so the loop
    /// renders at the fastest output's rate. A slower output's flip is left pending and
    /// retires through the next frame's non-blocking drain; until it has, that output skips
    /// the frames it could not show (`DirectOutcome::Deferred`), so no output renders more
    /// than once per its own vblank and no target KMS owns is rendered into. Waiting for
    /// every pacing output, rather than the first, keeps one whose vblank trails another's
    /// from dropping every other frame. Each gets the whole timeout, so one wedged output
    /// cannot spend a healthy sibling's budget and demote it; a slow output whose flip is
    /// overdue is demoted without a wait.
    fn retire_direct_flips(&mut self) -> Option<crate::shell::present::PhysicalPresentation> {
        let fastest = fastest_direct_refresh(&self.scanouts, &self.outputs);
        let mut proof = None;
        // Each pass retires or demotes one output; the bound is a backstop.
        for _ in 0..self.scanouts.len() {
            let now = Instant::now();
            let Some((index, timeout)) =
                self.scanouts.iter().enumerate().find_map(|(index, entry)| {
                    let targets = entry.direct_targets.as_ref()?;
                    let queued_at = targets.flip_queued_at()?;
                    let wait = flip_wait(
                        refresh_of(&self.outputs, entry.id),
                        fastest,
                        now.saturating_duration_since(queued_at),
                    );
                    match wait {
                        FlipWait::Await => Some((index, DIRECT_RETIREMENT_TIMEOUT)),
                        FlipWait::Overdue => Some((index, Duration::ZERO)),
                        FlipWait::Leave => None,
                    }
                })
            else {
                break;
            };
            let retired = self.retire_physical_flip(index, timeout);
            proof = proof.or(retired);
        }
        proof
    }

    /// Some direct output still waits for a page flip queued by an earlier frame.
    fn direct_flip_pending(&self) -> bool {
        self.scanouts.iter().any(|entry| {
            entry
                .direct_targets
                .as_ref()
                .is_some_and(scanout::DirectScanoutTargets::flip_pending)
        })
    }

    /// Wait up to `timeout` for the page flip queued on `scanouts[index]`.
    /// Every completion read on the way goes to the scanout it belongs to: the
    /// card's sibling outputs share this event stream.
    fn retire_physical_flip(
        &mut self,
        index: usize,
        timeout: Duration,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        let (retirement, proof) = self.await_flip(index, timeout);
        if retirement == scanout::DirectRetirement::TimedOut {
            self.retire_timed_out(index);
        }
        proof
    }

    /// A flip on `scanouts[index]` did not retire in time: its completion can no
    /// longer prove a frame, and a direct output goes to the readback presenter.
    fn retire_timed_out(&mut self, index: usize) {
        let Some(entry) = self.scanouts.get_mut(index) else {
            return;
        };
        entry.scanout.invalidate_physical_feedback();
        if entry.direct_targets.is_some() {
            let hdr = entry
                .direct_targets
                .as_ref()
                .is_some_and(|targets| targets.format().is_hdr());
            demote_direct(
                entry,
                hdr,
                "retire",
                &format!(
                    "page flip did not retire within {} ms",
                    DIRECT_RETIREMENT_TIMEOUT.as_millis()
                ),
            );
        }
    }

    /// Wait up to `timeout` for the page flip queued on `scanouts[index]`, recording
    /// the vblank of every completion read on the way and routing each to the scanout
    /// it belongs to. A timeout here is only an answer: [`Self::retire_physical_flip`]
    /// decides what an overdue flip costs.
    fn await_flip(
        &mut self,
        index: usize,
        timeout: Duration,
    ) -> (
        scanout::DirectRetirement,
        Option<crate::shell::present::PhysicalPresentation>,
    ) {
        let single_output = self.scanouts.len() == 1;
        let Some(entry) = self.scanouts.get_mut(index) else {
            return (scanout::DirectRetirement::TimedOut, None);
        };
        let (retirement, completions) = entry.scanout.wait_flip_completions(timeout);
        // Every completion read here comes from this card's event stream. A
        // driver without monotonic event stamps leaves the wakeup as the vblank.
        let woke = Instant::now();
        let monotonic = entry.scanout.monotonic_flip_timestamps();
        let device_id = entry.device_id;
        self.flip_vblanks
            .extend(completions.iter().map(|event| FlipVblank {
                device_id,
                crtc: event.crtc,
                at: if monotonic {
                    scanout::instant_from_monotonic(event.duration)
                } else {
                    woke
                },
            }));
        self.phase_window.record_flip_completions(completions.len());
        let proof = physical_flip_proof(
            retirement,
            &completions,
            entry.scanout.crtc(),
            single_output && entry.scanout.can_prove_physical_flip(),
        )
        .map(|mut proof| {
            proof.refresh = if entry.scanout.adaptive_sync_active() {
                None
            } else {
                self.outputs
                    .first()
                    .and_then(|output| output.refresh_mhz)
                    .and_then(|rate| 1_000_000_000_000_u64.checked_div(u64::from(rate)))
                    .map(Duration::from_nanos)
            };
            proof
        });
        let completed: Vec<_> = completions
            .iter()
            .map(|event| (device_id, event.crtc, event.frame))
            .collect();
        route_flip_completions(&mut self.scanouts, &completed);
        (retirement, proof)
    }

    fn emit_phase_profile(&mut self, sample: PaintScanoutSample) {
        self.phase_window.cpu_pixel_passes = self
            .phase_window
            .cpu_pixel_passes
            .saturating_add(sample.cpu_pixel_passes);
        if sample.legacy_attempted {
            let timing = self
                .paint
                .as_ref()
                .and_then(paint::VelloDrmBridge::last_phase_timing);
            self.phase_window
                .record_legacy_attempt(timing, sample.bridge_us, sample.scanout_us);
        }
        let now = Instant::now();
        // A production session reports bounded aggregates. Explicit profiling
        // instead flushes every non-empty window immediately: if rendering
        // stalls or becomes idle after one expensive frame, there may never be
        // a later frame available to flush that evidence.
        if self.phase_window.should_emit(now, self.profile_phases) {
            let frames = self.phase_window.frames.max(1);
            let summary = self.phase_window.summary_fields(now);
            // The session's default subscriber passes only errors unless
            // RUST_LOG says otherwise, so an info event never reaches the
            // flight recorder. The runner captures stderr into that fsyncing
            // record; keep the line machine-parseable.
            eprintln!(
                "frames={} direct_frames={} direct_dropped={} flip_completions={} \
                 unprofiled_frames={} window_us={} avg_submit_us={} \
                 avg_gpu_readback_map_us={} avg_settle_us={} avg_cpu_swizzle_us={} \
                 avg_backdrop_us={} readback_bytes={} output_bytes={} cpu_pixel_passes={} \
                 bridge_us={} scanout_us={} \
                 avg_direct_us={} max_direct_us={} direct_sparse_frames={} \
                 client_scanout_frames={} direct_failures={} \
                 direct_scanout_status={} direct_scanout_reason={:?} \
                 DRM render phase summary",
                summary.frames,
                summary.direct_frames,
                summary.direct_dropped,
                summary.flip_completions,
                summary.unprofiled_frames,
                summary.window_us,
                self.phase_window
                    .submit_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .gpu_readback_map_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .settle_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .cpu_swizzle_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .backdrop_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                summary.readback_bytes,
                summary.output_bytes,
                summary.cpu_pixel_passes,
                summary.bridge_us,
                summary.scanout_us,
                self.phase_window
                    .direct_us
                    .checked_div(u128::from(summary.direct_frames))
                    .unwrap_or(0),
                self.phase_window.max_direct_us,
                self.phase_window.direct_sparse_frames,
                self.phase_window.client_scanout_frames,
                direct_failures(&self.scanouts),
                direct_status(&self.scanouts),
                direct_reason(&self.scanouts),
            );
            self.phase_window.reset(now);
        }
        if !self.profile_phases {
            return;
        }
        let Some(timing) = self
            .paint
            .as_ref()
            .and_then(paint::VelloDrmBridge::last_phase_timing)
        else {
            return;
        };
        self.profile_seq = self.profile_seq.saturating_add(1);
        eprintln!(
            "seq={} submit_us={} gpu_readback_map_us={} settle_us={} cpu_swizzle_us={} \
             backdrop_us={} readback_bytes={} output_bytes={} DRM render frame phases",
            self.profile_seq,
            timing.submit_us,
            timing.completed_readback_map_us,
            timing.settle_us,
            timing.cpu_copy_swizzle_us,
            timing.backdrop_us,
            timing.readback_bytes,
            timing.cpu_output_bytes,
        );
    }

    /// Apply every reconfiguration the loop thread has queued.
    /// Move every output's link along ([`service_links`]).
    fn service_links(&mut self) {
        let completions = service_links(
            &mut self.scanouts,
            !self.paused,
            &self.link_busy,
            self.signal_reports.as_ref(),
        );
        self.phase_window.record_flip_completions(completions);
    }

    fn drain_commands(&mut self) {
        let Some(rx) = self.commands.as_ref() else {
            return;
        };
        let pending: Vec<ScanoutCommand> = rx.try_iter().collect();
        for cmd in pending {
            apply_scanout_command(
                cmd,
                &mut self.scanouts,
                &mut self.outputs,
                &mut self.paused,
                &mut self.blanked,
            );
        }
    }
}
