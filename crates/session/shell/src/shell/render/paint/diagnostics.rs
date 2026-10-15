//! Opt-in renderer diagnostics: GPU dispatch-stage records, the shared coarse/fine
//! comparisons, and segment-order replays.

use std::time::{Duration, Instant};

use ui::gpu::RenderContext;
use vello::{AaConfig, RenderParams, Renderer, Scene, wgpu};

use super::{
    BridgeError, MappedXrgbSource, RESILIENT_RENDER_ATTEMPTS, ReplayOutputDestinations,
    RetainedGpuTarget, SegmentOrderReplayDiagnostic, SharedCoarseDiagnosticTarget,
    SharedCoarseGeometryDiagnostic, VelloDrmBridge,
};

/// Opt-in diagnostic coverage, independent of input or presentation causality.
#[derive(Default)]
pub(super) struct DispatchStageObserver {
    attempts: u64,
    matched_current: u64,
    stale_samples: u64,
    missing_current: u64,
    pub(super) last_record: Option<serde_json::Value>,
}

impl DispatchStageObserver {
    #[expect(
        clippy::print_stderr,
        reason = "explicit GPU diagnostics must reach the native compositor process log"
    )]
    pub(super) fn record(
        &mut self,
        target: &mut RetainedGpuTarget,
        revision: u64,
        rendered: bool,
        previous_submission: Option<u64>,
        renderer_generation: Option<u64>,
    ) {
        self.attempts = self.attempts.saturating_add(1);
        let latest = target
            .renderer
            .gpu_dispatch_timestamp_latest_submission_id();
        let sample = target.renderer.take_gpu_dispatch_timestamp_sample();
        let sample_id = sample.as_ref().map(|sample| sample.submission_id);
        let expected = latest.filter(|_| latest != previous_submission && rendered);
        let matched = sample_id.is_some() && sample_id == expected;
        self.matched_current = self.matched_current.saturating_add(u64::from(matched));
        self.stale_samples = self
            .stale_samples
            .saturating_add(u64::from(sample_id.is_some() && !matched));
        self.missing_current = self.missing_current.saturating_add(u64::from(!matched));
        let stages: Vec<_> = sample.as_ref().map(|sample| {
            sample.stages.iter().take(usize::try_from(sample.stage_count).unwrap_or(0).min(sample.stages.len()))
                .map(|stage| serde_json::json!({
                    "stage_id": stage.stage_id,
                    "stage": vello::gpu_dispatch_timestamp_stage_name(stage.stage_id).unwrap_or("unknown"),
                    "dispatch_count": stage.dispatch_count,
                    "duration_ns": stage.duration_ns.to_string(),
                })).collect()
        }).unwrap_or_default();
        let row = serde_json::json!({
            "process_id": std::process::id(), "renderer_generation": renderer_generation,
            "revision": revision, "render_succeeded": rendered,
            "latest_attempted_submission_id": latest, "expected_current_submission_id": expected,
            "sample_submission_id": sample_id,
            "matched_current": matched, "missing_current": !matched,
            "stale_sample": sample_id.is_some() && !matched,
            "status": format!("{:?}", target.renderer.gpu_dispatch_timestamp_status()),
            "timestamp_buffer_bytes": target.renderer.gpu_dispatch_timestamp_buffer_bytes(),
            "coverage": {"attempts": self.attempts, "matched_current": self.matched_current,
                "stale_samples": self.stale_samples, "missing_current": self.missing_current},
            "stages": stages,
        });
        eprintln!("VELLO-STAGES {row}");
        self.last_record = Some(row);
    }
}

impl VelloDrmBridge {
    /// Diagnostic-only exact comparison of dense and sparse Area fine work sharing one coarse
    /// recording. This invalidates retained state before rendering and never publishes a
    /// revision, so a subsequent ordinary paint must conservatively render densely.
    ///
    /// This intentionally allocates one additional target and staging buffer only for this
    /// explicit call. It is not part of the production paint path or benchmark workload.
    ///
    /// # Errors
    /// Returns [`BridgeError`] if Vello cannot submit or settle either paired fine result, or if
    /// either complete diagnostic readback cannot map before the ordinary one-second fence.
    #[doc(hidden)]
    pub fn shared_coarse_fine_diagnostic_into_xrgb8888(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<vello::SparseRenderStats>, BridgeError> {
        self.shared_coarse_diagnostic(
            scene,
            damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            Self::render_shared_coarse_diagnostic_attempts,
        )
    }

    /// Like [`Self::shared_coarse_fine_diagnostic_into_xrgb8888`], while also returning the
    /// bounded CPU geometry capture from that exact settled submission.
    #[doc(hidden)]
    pub fn shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        self.shared_coarse_diagnostic(
            scene,
            damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            Self::render_shared_coarse_geometry_diagnostic_attempts,
        )
    }

    /// Validate `damage`, allocate the sparse diagnostic target and run
    /// `attempts` against it on a freshly invalidated retained state.
    fn shared_coarse_diagnostic<T>(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        attempts: impl FnOnce(
            &mut Self,
            &Scene,
            &[vello::FineDamageRect],
            &mut [u8],
            &mut [u8],
            usize,
            SharedCoarseDiagnosticTarget<'_>,
        ) -> Result<Option<T>, BridgeError>,
    ) -> Result<Option<T>, BridgeError> {
        self.target.check_health()?;
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let Some(damage) = Self::validate_diagnostic_damage(damage, self.target.size) else {
            return Ok(None);
        };

        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let (sparse_texture, sparse_view, sparse_readback, sparse_stride) =
            Self::allocate_targets(&device, width, height);
        let sparse_target = SharedCoarseDiagnosticTarget {
            texture: &sparse_texture,
            view: &sparse_view,
            readback: &sparse_readback,
            padded_bytes_per_row: sparse_stride,
        };

        attempts(
            self,
            scene,
            &damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            sparse_target,
        )
    }

    /// Replay two fixed segment orders through ordinary Area fine while holding one live coarse
    /// result. This is an explicit failure diagnostic and is not used by paint or benchmarks.
    #[doc(hidden)]
    pub fn segment_order_replay_into_xrgb8888(
        &mut self,
        scene: &Scene,
        input: &vello::SegmentOrderReplayInput,
        left_dst: &mut [u8],
        right_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<SegmentOrderReplayDiagnostic>, BridgeError> {
        self.target.check_health()?;
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let (width, height) = self.target.size;
        Self::validate_replay_destination(left_dst, dst_pitch, width, height, "left")?;
        Self::validate_replay_destination(right_dst, dst_pitch, width, height, "right")?;
        Renderer::segment_order_replay_preflight(width, height, input)
            .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        let (right_texture, right_view, right_readback, right_stride) =
            Self::allocate_targets(&device, width, height);
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let replay = self
                .target
                .renderer
                .render_to_textures_segment_order_replay_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    &right_view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    input,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            &right_texture,
                            &right_readback,
                            right_stride,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some(replay) = replay else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            let buffers = [
                (&self.readback_buf, self.readback_buf.size()),
                (&right_readback, right_readback.size()),
                (&replay.left, replay.bytes_per_witness),
                (&replay.right, replay.bytes_per_witness),
            ];
            let mapped = Self::map_replay_buffers(&self.context, self.dev_id, &buffers)?;
            if self.target.renderer.settle_bump_budget(&device) {
                drop(mapped);
                Self::unmap_replay_buffers(&buffers);
                continue;
            }
            let decoded = Self::swizzle_and_decode_replay(
                &mapped,
                &mut ReplayOutputDestinations {
                    left: left_dst,
                    right: right_dst,
                    dst_pitch,
                    width: width as usize,
                    height: height as usize,
                    left_padded_bytes_per_row: self.padded_bytes_per_row as usize,
                    right_padded_bytes_per_row: right_stride as usize,
                },
            );
            drop(mapped);
            Self::unmap_replay_buffers(&buffers);
            let (left_witness, right_witness) = decoded?;
            return Ok(Some(SegmentOrderReplayDiagnostic {
                left_witness,
                right_witness,
            }));
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn render_shared_coarse_geometry_diagnostic_attempts(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .target
                .renderer
                .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            sparse_target.texture,
                            sparse_target.readback,
                            sparse_target.padded_bytes_per_row,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some((stats, geometry)) = sparse else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            self.target.last_sparse_render_attempts =
                self.target.last_sparse_render_attempts.saturating_add(1);
            self.target.last_sparse_render_stats = Some(stats);

            if let Some(capture) = self.read_shared_coarse_geometry_diagnostic_outputs(
                &device,
                sparse_target,
                full_dst,
                sparse_dst,
                dst_pitch,
                stats,
                geometry,
            )? {
                return Ok(Some(capture));
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn render_shared_coarse_diagnostic_attempts(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
    ) -> Result<Option<vello::SparseRenderStats>, BridgeError> {
        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .target
                .renderer
                .render_to_textures_shared_coarse_area_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            sparse_target.texture,
                            sparse_target.readback,
                            sparse_target.padded_bytes_per_row,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some(stats) = sparse else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            self.target.last_sparse_render_attempts =
                self.target.last_sparse_render_attempts.saturating_add(1);
            self.target.last_sparse_render_stats = Some(stats);

            if self.read_shared_coarse_diagnostic_outputs(
                &device,
                sparse_target,
                full_dst,
                sparse_dst,
                dst_pitch,
            )? {
                return Ok(Some(stats));
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn read_shared_coarse_diagnostic_outputs(
        &mut self,
        device: &wgpu::Device,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<bool, BridgeError> {
        let (width, height) = self.target.size;
        let full_mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
        let sparse_mapped =
            match Self::map_readback(&self.context, self.dev_id, sparse_target.readback) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(full_mapped);
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        if self.target.renderer.settle_bump_budget(device) {
            drop(sparse_mapped);
            drop(full_mapped);
            sparse_target.readback.unmap();
            self.readback_buf.unmap();
            return Ok(false);
        }
        let full_source = MappedXrgbSource {
            bytes: &full_mapped,
            padded_bytes_per_row: self.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        let sparse_source = MappedXrgbSource {
            bytes: &sparse_mapped,
            padded_bytes_per_row: sparse_target.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        Self::swizzle_mapped_xrgb8888(full_source, full_dst, dst_pitch, false);
        Self::swizzle_mapped_xrgb8888(sparse_source, sparse_dst, dst_pitch, false);
        drop(sparse_mapped);
        drop(full_mapped);
        sparse_target.readback.unmap();
        self.readback_buf.unmap();
        Ok(true)
    }

    fn read_shared_coarse_geometry_diagnostic_outputs(
        &mut self,
        device: &wgpu::Device,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        stats: vello::SparseRenderStats,
        geometry: vello::SharedCoarseGeometryDownload,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        let (width, height) = self.target.size;
        let vello::SharedCoarseGeometryDownload {
            config,
            ptcl,
            ptcl_bytes,
            segments,
            segments_bytes,
        } = geometry;
        let full_mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
        let sparse_mapped =
            match Self::map_readback(&self.context, self.dev_id, sparse_target.readback) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(full_mapped);
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        let ptcl_mapped =
            match Self::map_buffer_range(&self.context, self.dev_id, &ptcl, ptcl_bytes) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(sparse_mapped);
                    drop(full_mapped);
                    sparse_target.readback.unmap();
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        let segments_mapped =
            match Self::map_buffer_range(&self.context, self.dev_id, &segments, segments_bytes) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(ptcl_mapped);
                    drop(sparse_mapped);
                    drop(full_mapped);
                    ptcl.unmap();
                    sparse_target.readback.unmap();
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        if self.target.renderer.settle_bump_budget(device) {
            drop(segments_mapped);
            drop(ptcl_mapped);
            drop(sparse_mapped);
            drop(full_mapped);
            segments.unmap();
            ptcl.unmap();
            sparse_target.readback.unmap();
            self.readback_buf.unmap();
            return Ok(None);
        }
        let full_source = MappedXrgbSource {
            bytes: &full_mapped,
            padded_bytes_per_row: self.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        let sparse_source = MappedXrgbSource {
            bytes: &sparse_mapped,
            padded_bytes_per_row: sparse_target.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        Self::swizzle_mapped_xrgb8888(full_source, full_dst, dst_pitch, false);
        Self::swizzle_mapped_xrgb8888(sparse_source, sparse_dst, dst_pitch, false);
        let ptcl_bytes = ptcl_mapped.to_vec();
        let segments_bytes = segments_mapped.to_vec();
        drop(segments_mapped);
        drop(ptcl_mapped);
        drop(sparse_mapped);
        drop(full_mapped);
        segments.unmap();
        ptcl.unmap();
        sparse_target.readback.unmap();
        self.readback_buf.unmap();
        Ok(Some(SharedCoarseGeometryDiagnostic {
            stats,
            config,
            ptcl: ptcl_bytes,
            segments: segments_bytes,
        }))
    }

    /// Diagnostic paired rendering needs valid native rectangles, but intentionally does not
    /// apply the production sparse profitability threshold: a complete frame is a meaningful
    /// dense-versus-sparse oracle input.
    pub(super) fn validate_diagnostic_damage(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        const MAX_DAMAGE_RECTS: usize = 64;
        if damage.is_empty() || damage.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        let (width, height) = size;
        if damage.iter().any(|rect| {
            rect.width == 0
                || rect.height == 0
                || rect.x >= width
                || rect.y >= height
                || rect.x.checked_add(rect.width).is_none_or(|x1| x1 > width)
                || rect.y.checked_add(rect.height).is_none_or(|y1| y1 > height)
        }) {
            return None;
        }
        Some(damage.to_vec())
    }

    /// Map every replay result before one bounded device poll. Unlike the older geometry helper,
    /// this has one absolute deadline for both full-frame outputs and both tiny GPU witnesses.
    fn map_replay_buffers(
        context: &RenderContext,
        dev_id: usize,
        buffers: &[(&wgpu::Buffer, u64)],
    ) -> Result<Vec<wgpu::BufferView>, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .ok_or_else(|| BridgeError::Map("replay mapping deadline overflowed".to_owned()))?;
        let mut receivers = Vec::with_capacity(buffers.len());
        for (buffer, bytes) in buffers {
            let slice = buffer.slice(..*bytes);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).ok();
            });
            receivers.push(rx);
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if let Err(error) = dev_handle.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        }) {
            Self::unmap_replay_buffers(buffers);
            return Err(BridgeError::Map(format!("replay fence wait: {error:?}")));
        }
        for receiver in receivers {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    Self::unmap_replay_buffers(buffers);
                    return Err(BridgeError::Map(format!("replay map: {error:?}")));
                }
                Err(error) => {
                    Self::unmap_replay_buffers(buffers);
                    return Err(BridgeError::Map(format!("replay map callback: {error}")));
                }
            }
        }
        Ok(buffers
            .iter()
            .map(|(buffer, bytes)| buffer.slice(..*bytes).get_mapped_range())
            .collect())
    }

    fn swizzle_and_decode_replay(
        mapped: &[wgpu::BufferView],
        destinations: &mut ReplayOutputDestinations<'_>,
    ) -> Result<(Vec<u32>, Vec<u32>), BridgeError> {
        match mapped {
            [left, right, left_witness, right_witness] => {
                let left_source = MappedXrgbSource {
                    bytes: left,
                    padded_bytes_per_row: destinations.left_padded_bytes_per_row,
                    width: destinations.width,
                    height: destinations.height,
                };
                let right_source = MappedXrgbSource {
                    bytes: right,
                    padded_bytes_per_row: destinations.right_padded_bytes_per_row,
                    width: destinations.width,
                    height: destinations.height,
                };
                Self::swizzle_mapped_xrgb8888(
                    left_source,
                    destinations.left,
                    destinations.dst_pitch,
                    false,
                );
                Self::swizzle_mapped_xrgb8888(
                    right_source,
                    destinations.right,
                    destinations.dst_pitch,
                    false,
                );
                Self::decode_replay_witness(left_witness).and_then(|left| {
                    Self::decode_replay_witness(right_witness).map(|right| (left, right))
                })
            }
            _ => Err(BridgeError::Map(
                "replay mapped an unexpected number of buffers".to_owned(),
            )),
        }
    }

    fn decode_replay_witness(payload: &[u8]) -> Result<Vec<u32>, BridgeError> {
        let mut words = Vec::with_capacity(payload.len() / 4);
        let chunks = payload.chunks_exact(4);
        if !chunks.remainder().is_empty() {
            return Err(BridgeError::Map(
                "replay witness length is not a whole number of u32 words".to_owned(),
            ));
        }
        for chunk in chunks {
            let [byte0, byte1, byte2, byte3] = chunk else {
                return Err(BridgeError::Map(
                    "replay witness contained a partial u32 word".to_owned(),
                ));
            };
            words.push(u32::from_le_bytes([*byte0, *byte1, *byte2, *byte3]));
        }
        Ok(words)
    }

    fn unmap_replay_buffers(buffers: &[(&wgpu::Buffer, u64)]) {
        for (buffer, _) in buffers {
            buffer.unmap();
        }
    }

    fn validate_replay_destination(
        destination: &[u8],
        pitch: usize,
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<(), BridgeError> {
        let tight = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| BridgeError::InvalidSize(format!("{label} width overflow")))?;
        let required = pitch
            .checked_mul(usize::try_from(height).map_err(|_| {
                BridgeError::InvalidSize(format!("{label} height does not fit usize"))
            })?)
            .ok_or_else(|| BridgeError::InvalidSize(format!("{label} destination overflow")))?;
        if pitch < tight || destination.len() < required {
            return Err(BridgeError::InvalidSize(format!(
                "{label} replay destination is {} bytes at pitch {pitch}; need at least {required} bytes and pitch {tight}",
                destination.len(),
            )));
        }
        Ok(())
    }
}
