//! Small in-process MP4 sink for compositor recordings.
//!
//! `OpenH264` gives the capture path Annex-B access units. MP4 carries the same
//! compressed pictures with length-prefixed NAL units and an `avcC` codec
//! configuration record, so this module only repackages bytes; it does not run
//! a second encoder or spawn an external media process.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

pub struct Mp4Recording {
    path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    output: Option<ffmpeg::format::context::Output>,
    frame_index: i64,
    audio_track_names: Vec<String>,
    audio_sample_indices: Vec<i64>,
}

impl Mp4Recording {
    pub fn new(
        path: PathBuf,
        width: u32,
        height: u32,
        fps: u32,
        audio_track_names: Vec<String>,
    ) -> Self {
        let audio_sample_indices = vec![0; audio_track_names.len()];
        Self {
            path,
            width,
            height,
            fps: fps.max(1),
            output: None,
            frame_index: 0,
            audio_track_names,
            audio_sample_indices,
        }
    }

    pub fn write_access_unit(&mut self, annex_b: &[u8]) -> Result<(), String> {
        let nals = annex_b_nals(annex_b);
        if nals.is_empty() {
            return Ok(());
        }
        if self.output.is_none() {
            let sps = nals
                .iter()
                .copied()
                .find(|nal| nal_type(nal) == 7)
                .ok_or_else(|| {
                    "first H.264 access unit did not contain SPS metadata".to_string()
                })?;
            let pps = nals
                .iter()
                .copied()
                .find(|nal| nal_type(nal) == 8)
                .ok_or_else(|| {
                    "first H.264 access unit did not contain PPS metadata".to_string()
                })?;
            self.output = Some(open_output(
                &self.path,
                self.width,
                self.height,
                self.fps,
                &avc_configuration(sps, pps)?,
                &self.audio_track_names,
            )?);
        }

        let keyframe = nals.iter().any(|nal| nal_type(nal) == 5);
        let sample = mp4_sample(&nals)?;
        if sample.is_empty() {
            return Ok(());
        }
        let mut packet = ffmpeg::Packet::copy(&sample);
        packet.set_stream(0);
        packet.set_pts(Some(self.frame_index));
        packet.set_dts(Some(self.frame_index));
        packet.set_duration(1);
        packet.set_position(-1);
        if keyframe {
            packet.set_flags(ffmpeg::packet::Flags::KEY);
        }
        let output = self
            .output
            .as_mut()
            .ok_or_else(|| "MP4 output was not initialized".to_string())?;
        let mux_time_base = output
            .stream(0)
            .ok_or_else(|| "MP4 video stream disappeared".to_string())?
            .time_base();
        // MP4 commonly replaces the requested 1/fps clock with a finer track
        // time base while writing its header. Rescale every packet into that
        // final clock or a five-second recording can play in milliseconds.
        let fps = i32::try_from(self.fps).map_err(|_| "recording FPS is too large".to_string())?;
        packet.rescale_ts((1, fps), mux_time_base);
        packet
            .write_interleaved(output)
            .map_err(|error| format!("mux MP4 sample: {error}"))?;
        self.frame_index = self.frame_index.saturating_add(1);
        Ok(())
    }

    /// Append interleaved stereo S16LE samples to a named MOV audio track.
    /// Partial sample frames stay with the caller; accepting only aligned
    /// buffers keeps timestamps exact and the muxer contract unambiguous.
    pub fn write_pcm_audio(&mut self, track_index: usize, pcm: &[u8]) -> Result<(), String> {
        let sample_index = self
            .audio_sample_indices
            .get_mut(track_index)
            .ok_or_else(|| format!("audio track {track_index} was not configured"))?;
        if pcm.is_empty() {
            return Ok(());
        }
        if !pcm.len().is_multiple_of(4) {
            return Err("stereo S16 audio buffer is not sample-frame aligned".into());
        }
        let sample_frames =
            i64::try_from(pcm.len() / 4).map_err(|_| "audio buffer is too large".to_string())?;
        let mut packet = ffmpeg::Packet::copy(pcm);
        let stream_index = track_index.saturating_add(1);
        packet.set_stream(stream_index);
        packet.set_pts(Some(*sample_index));
        packet.set_dts(Some(*sample_index));
        packet.set_duration(sample_frames);
        packet.set_position(-1);
        packet.set_flags(ffmpeg::packet::Flags::KEY);
        let output = self
            .output
            .as_mut()
            .ok_or_else(|| "video track must start before audio is muxed".to_string())?;
        let mux_time_base = output
            .stream(stream_index)
            .ok_or_else(|| "MOV audio stream disappeared".to_string())?
            .time_base();
        packet.rescale_ts((1, 48_000), mux_time_base);
        packet
            .write_interleaved(output)
            .map_err(|error| format!("mux MOV audio: {error}"))?;
        *sample_index = sample_index.saturating_add(sample_frames);
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), String> {
        let Some(mut output) = self.output.take() else {
            return Err("recording ended before its first encoded frame".into());
        };
        output
            .write_trailer()
            .map_err(|error| format!("finalize MP4 {}: {error}", self.path.display()))
    }
}

fn open_output(
    path: &Path,
    width: u32,
    height: u32,
    fps: u32,
    codec_private: &[u8],
    audio_track_names: &[String],
) -> Result<ffmpeg::format::context::Output, String> {
    let width = i32::try_from(width).map_err(|_| "video width is too large".to_string())?;
    let height = i32::try_from(height).map_err(|_| "video height is too large".to_string())?;
    let fps = i32::try_from(fps).map_err(|_| "video FPS is too large".to_string())?;
    let extradata_size = i32::try_from(codec_private.len())
        .map_err(|_| "MP4 codec metadata is too large".to_string())?;
    ffmpeg::init().map_err(|error| format!("initialize media muxer: {error}"))?;
    let mut output = ffmpeg::format::output(path)
        .map_err(|error| format!("create MP4 {}: {error}", path.display()))?;
    let mut parameters = ffmpeg::codec::Parameters::new();
    unsafe {
        let raw = parameters.as_mut_ptr();
        (*raw).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
        (*raw).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_H264;
        (*raw).width = width;
        (*raw).height = height;
        (*raw).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
        let padding = ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        let allocation = ffmpeg::ffi::av_mallocz(codec_private.len().saturating_add(padding));
        if allocation.is_null() {
            return Err("allocate MP4 codec metadata".into());
        }
        let extradata = allocation.cast::<u8>();
        std::ptr::copy_nonoverlapping(codec_private.as_ptr(), extradata, codec_private.len());
        (*raw).extradata = extradata;
        (*raw).extradata_size = extradata_size;
    }
    {
        let mut stream = output
            .add_stream(Option::<ffmpeg::Codec>::None)
            .map_err(|error| format!("add MP4 video stream: {error}"))?;
        stream.set_parameters(parameters);
        stream.set_time_base((1, fps));
        stream.set_rate((fps, 1));
        stream.set_avg_frame_rate((fps, 1));
    }
    for track_name in audio_track_names {
        let mut parameters = ffmpeg::codec::Parameters::new();
        unsafe {
            let raw = parameters.as_mut_ptr();
            (*raw).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO;
            (*raw).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_PCM_S16LE;
            (*raw).format = ffmpeg::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32;
            (*raw).sample_rate = 48_000;
            (*raw).bit_rate = 48_000 * 2 * 16;
            (*raw).bits_per_coded_sample = 16;
            (*raw).bits_per_raw_sample = 16;
            (*raw).block_align = 4;
            (*raw).ch_layout = ffmpeg::ffi::AVChannelLayout {
                order: ffmpeg::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
                nb_channels: 2,
                u: ffmpeg::ffi::AVChannelLayout__bindgen_ty_1 {
                    mask: ffmpeg::ffi::AV_CH_LAYOUT_STEREO,
                },
                opaque: std::ptr::null_mut(),
            };
        }
        let mut stream = output
            .add_stream(Option::<ffmpeg::Codec>::None)
            .map_err(|error| format!("add MOV audio stream: {error}"))?;
        stream.set_parameters(parameters);
        stream.set_time_base((1, 48_000));
        let mut metadata = ffmpeg::Dictionary::new();
        metadata.set("title", track_name);
        metadata.set("handler_name", track_name);
        stream.set_metadata(metadata);
    }
    output
        .write_header()
        .map_err(|error| format!("write MP4 header {}: {error}", path.display()))?;
    Ok(output)
}

fn avc_configuration(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, String> {
    if sps.len() < 4
        || pps.is_empty()
        || sps.len() > u16::MAX as usize
        || pps.len() > u16::MAX as usize
    {
        return Err("invalid H.264 SPS/PPS metadata".into());
    }
    let profile = sps
        .get(1..4)
        .ok_or_else(|| "invalid H.264 SPS metadata".to_string())?;
    let [profile_idc, constraints, level] = profile else {
        return Err("invalid H.264 SPS metadata".into());
    };
    let sps_len = u16::try_from(sps.len()).map_err(|_| "H.264 SPS is too large".to_string())?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| "H.264 PPS is too large".to_string())?;
    let mut out = Vec::with_capacity(11usize.saturating_add(sps.len()).saturating_add(pps.len()));
    out.extend_from_slice(&[1, *profile_idc, *constraints, *level, 0xff, 0xe1]);
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    Ok(out)
}

fn mp4_sample(nals: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for nal in nals {
        // Parameter sets live once in `avcC`; repeating them in every sample
        // wastes space and confuses stricter importers.
        if matches!(nal_type(nal), 7..=9) {
            continue;
        }
        let len =
            u32::try_from(nal.len()).map_err(|_| "H.264 NAL unit is too large".to_string())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    Ok(out)
}

fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |byte| byte & 0x1f)
}

// Indices advance monotonically within `bytes` and every resulting range is
// validated by `get`; keeping the parser's offsets explicit mirrors Annex-B.
#[allow(clippy::arithmetic_side_effects)]
fn annex_b_nals(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let prefix = if bytes.get(i..i + 4) == Some(&[0, 0, 0, 1]) {
            4
        } else if bytes.get(i..i + 3) == Some(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        starts.push((i, i + prefix));
        i += prefix;
    }
    let mut out = Vec::with_capacity(starts.len());
    for (index, &(_, payload_start)) in starts.iter().enumerate() {
        let payload_end = starts
            .get(index + 1)
            .map_or(bytes.len(), |(start, _)| *start);
        if payload_start < payload_end
            && let Some(nal) = bytes.get(payload_start..payload_end)
        {
            out.push(nal);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_precision_loss,
        reason = "test media durations are tiny and exactly representable in f64"
    )]

    use super::*;

    #[test]
    fn annex_b_boundaries_and_mp4_sample_are_exact() {
        let access_unit = [
            0, 0, 0, 1, 0x67, 0x64, 0, 31, 0xaa, 0, 0, 1, 0x68, 0xbb, 0, 0, 0, 1, 0x65, 0xcc, 0xdd,
        ];
        let nals = annex_b_nals(&access_unit);
        assert_eq!(nals.len(), 3);
        assert_eq!(
            nals.iter().map(|nal| nal_type(nal)).collect::<Vec<_>>(),
            [7, 8, 5]
        );
        let sample = mp4_sample(&nals).unwrap();
        assert_eq!(&sample[..4], &3u32.to_be_bytes());
        assert_eq!(&sample[4..], &[0x65, 0xcc, 0xdd]);
    }

    #[test]
    fn avcc_carries_profile_level_and_parameter_sets() {
        let sps = [0x67, 0x64, 0, 31, 0xaa];
        let pps = [0x68, 0xbb];
        let avcc = avc_configuration(&sps, &pps).unwrap();
        assert_eq!(&avcc[..6], &[1, 0x64, 0, 31, 0xff, 0xe1]);
        assert!(avcc.windows(sps.len()).any(|bytes| bytes == sps));
        assert!(avcc.windows(pps.len()).any(|bytes| bytes == pps));
    }

    #[test]
    fn openh264_access_units_finish_as_a_demuxable_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.mp4");
        let mut encoder = crate::network::encoder::H264Encoder::new(64, 48, 30).unwrap();
        let mut recording = Mp4Recording::new(path.clone(), 64, 48, 30, Vec::new());
        for frame in 0..5u8 {
            let mut rgba = vec![0u8; 64 * 48 * 4];
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[
                    frame.saturating_mul(35),
                    110,
                    210u8.saturating_sub(frame.saturating_mul(20)),
                    255,
                ]);
            }
            let access_unit = encoder.encode(&rgba).unwrap();
            recording.write_access_unit(&access_unit).unwrap();
        }
        recording.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");
        assert!(bytes.windows(4).any(|window| window == b"mdat"));
        assert!(bytes.windows(4).any(|window| window == b"moov"));
        let input = ffmpeg::format::input(&path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        assert_eq!(stream.frames(), 5);
        let time_base = stream.time_base();
        let duration_seconds = stream.duration() as f64 * f64::from(time_base.numerator())
            / f64::from(time_base.denominator());
        assert!(
            (0.16..0.18).contains(&duration_seconds),
            "{duration_seconds}"
        );
    }

    #[test]
    fn periodic_idr_starts_a_standalone_replay_clip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent.mp4");
        let mut encoder =
            crate::network::encoder::H264Encoder::new_with_bitrate_and_keyframe_interval(
                64, 48, 30, 2_000_000, 60,
            )
            .unwrap();
        let mut recent = Vec::new();
        for frame in 0..65u8 {
            let mut rgba = vec![0u8; 64 * 48 * 4];
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[
                    frame.saturating_mul(3),
                    80,
                    210u8.saturating_sub(frame.saturating_mul(2)),
                    255,
                ]);
            }
            let access_unit = encoder.encode(&rgba).unwrap();
            if frame >= 60 {
                recent.push(access_unit);
            }
        }

        let first_nals = annex_b_nals(&recent[0]);
        assert!(first_nals.iter().any(|nal| nal_type(nal) == 5));
        assert!(first_nals.iter().any(|nal| nal_type(nal) == 7));
        assert!(first_nals.iter().any(|nal| nal_type(nal) == 8));

        let mut recording = Mp4Recording::new(path.clone(), 64, 48, 30, Vec::new());
        for access_unit in &recent {
            recording.write_access_unit(access_unit).unwrap();
        }
        recording.finish().unwrap();

        let input = ffmpeg::format::input(&path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        assert_eq!(stream.frames(), 5);
    }

    #[test]
    fn mov_keeps_lossless_sources_as_named_synchronized_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.mov");
        let mut encoder = crate::network::encoder::H264Encoder::new(64, 48, 30).unwrap();
        let mut recording = Mp4Recording::new(
            path.clone(),
            64,
            48,
            30,
            vec!["Desktop sound".into(), "Microphone".into()],
        );
        for frame in 0..5u8 {
            let mut rgba = vec![0u8; 64 * 48 * 4];
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[frame.saturating_mul(35), 90, 180, 255]);
            }
            recording
                .write_access_unit(&encoder.encode(&rgba).unwrap())
                .unwrap();
            // 1/30 second of 48 kHz stereo S16LE.
            recording
                .write_pcm_audio(0, &vec![0x11; 1_600 * 4])
                .unwrap();
            recording
                .write_pcm_audio(1, &vec![0x22; 1_600 * 4])
                .unwrap();
        }
        recording.finish().unwrap();

        let input = ffmpeg::format::input(&path).unwrap();
        let video = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let audio: Vec<_> = input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio)
            .collect();
        assert_eq!(video.parameters().id(), ffmpeg::codec::Id::H264);
        assert_eq!(audio.len(), 2);
        assert_eq!(
            audio[0].metadata().get("handler_name"),
            Some("Desktop sound")
        );
        assert_eq!(audio[1].metadata().get("handler_name"), Some("Microphone"));
        for stream in audio {
            assert_eq!(stream.parameters().id(), ffmpeg::codec::Id::PCM_S16LE);
            let time_base = stream.time_base();
            let duration_seconds = stream.duration() as f64 * f64::from(time_base.numerator())
                / f64::from(time_base.denominator());
            assert!(
                (0.16..0.18).contains(&duration_seconds),
                "{duration_seconds}"
            );
        }
    }
}
