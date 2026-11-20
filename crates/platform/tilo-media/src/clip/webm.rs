//! The `WebM` container a clip recorded on the page is muxed into.
//!
//! The browser encodes the pictures (VP8 through `WebCodecs`); this module owns
//! the bytes around them, so what a page captures is a file every browser
//! plays and seeks: an EBML header, a `Segment` holding `Info` (timing and
//! duration), `Tracks` (one VP8 video track), one `Cluster` per second of
//! frames beginning on a key frame, and a `CuePoint` per cluster. The desktop
//! keeps its self-contained AVI encoder (`super::avi`).

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "the AVI and WebM writers do byte arithmetic on sizes the capture bounds (a 63 MiB clip cap, frame extents no wider than a u16) and index the buffers they build; every site is a container field, not a computed decision"
)]

/// Encoded frames past this bound are dropped, as the AVI recorder drops them
/// past its own: a forgotten recording cannot grow without bound, and the
/// clip stays below the portable media limit the content store verifies.
const MAX_BYTES: usize = 63 * 1024 * 1024;

/// One encoded frame from the browser's video encoder.
struct Chunk {
    /// Presentation time from the clip's first frame, in microseconds.
    timestamp_us: u64,
    key: bool,
    data: Vec<u8>,
}

/// Encoded VP8 frames, kept until the clip's bytes are built at
/// [`finish`](Muxer::bytes).
pub struct Muxer {
    width: u32,
    height: u32,
    fps: u32,
    chunks: Vec<Chunk>,
    encoded_bytes: usize,
}

impl Muxer {
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        Self {
            width,
            height,
            fps: fps.max(1),
            chunks: Vec::new(),
            encoded_bytes: 0,
        }
    }

    /// Hold one encoded frame. Frames that would push the clip past
    /// [`MAX_BYTES`] are dropped, so the file ends instead of growing.
    pub fn push(&mut self, timestamp_us: u64, key: bool, data: Vec<u8>) {
        if self.encoded_bytes.saturating_add(data.len()) > MAX_BYTES {
            return;
        }
        self.encoded_bytes = self.encoded_bytes.saturating_add(data.len());
        self.chunks.push(Chunk {
            timestamp_us,
            key,
            data,
        });
    }

    /// How long the captured frames last: from the first frame's timestamp to
    /// one frame past the last, so a single frame still names a duration.
    fn duration_us(&self) -> u64 {
        let (Some(first), Some(last)) = (self.chunks.first(), self.chunks.last()) else {
            return 0;
        };
        let span = last.timestamp_us.saturating_sub(first.timestamp_us);
        span.saturating_add(1_000_000 / u64::from(self.fps))
    }

    /// [`duration_us`](Self::duration_us) in whole milliseconds.
    pub fn duration_ms(&self) -> i64 {
        i64::try_from(self.duration_us() / 1000).unwrap_or(i64::MAX)
    }

    /// The complete `WebM` file, or `None` when no frame was recorded.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let first = self.chunks.first()?;
        let info = self.info_element();
        let tracks = self.tracks_element();
        // Cluster positions in Cues are relative to the first byte of the
        // Segment's body, which is where `Info` starts.
        let mut offset = info.len().saturating_add(tracks.len());
        let mut clusters = Vec::new();
        let mut cue_points = Vec::new();
        for cluster in self.clusters(first.timestamp_us) {
            let mut children = vec![uint_element(TIMECODE, cluster.timecode_ms)];
            for block in &cluster.blocks {
                children.push(element(SIMPLE_BLOCK, block));
            }
            let element = master(CLUSTER, &children);
            cue_points.push(master(
                CUE_POINT,
                &[
                    uint_element(CUE_TIME, cluster.timecode_ms),
                    master(
                        CUE_TRACK_POSITIONS,
                        &[
                            uint_element(CUE_TRACK, 1),
                            uint_element(CUE_CLUSTER_POSITION, offset as u64),
                        ],
                    ),
                ],
            ));
            offset = offset.saturating_add(element.len());
            clusters.push(element);
        }

        let mut body = Vec::new();
        body.extend_from_slice(&info);
        body.extend_from_slice(&tracks);
        for cluster in &clusters {
            body.extend_from_slice(cluster);
        }
        body.extend_from_slice(&master(CUES, &cue_points));

        let mut out = ebml_header();
        out.extend_from_slice(&element(SEGMENT, &body));
        Some(out)
    }

    fn info_element(&self) -> Vec<u8> {
        let duration = self.duration_us() as f64 / 1000.0;
        master(
            INFO,
            &[
                uint_element(TIMECODE_SCALE, 1_000_000),
                string_element(MUXING_APP, "tilo camera"),
                string_element(WRITING_APP, "tilo camera"),
                element(DURATION, &duration.to_bits().to_be_bytes()),
            ],
        )
    }

    fn tracks_element(&self) -> Vec<u8> {
        let default_duration = 1_000_000_000 / u64::from(self.fps);
        master(
            TRACKS,
            &[master(
                TRACK_ENTRY,
                &[
                    uint_element(TRACK_NUMBER, 1),
                    uint_element(TRACK_UID, 1),
                    uint_element(TRACK_TYPE, 1), // video
                    uint_element(FLAG_LACING, 0),
                    string_element(CODEC_ID, "V_VP8"),
                    uint_element(DEFAULT_DURATION, default_duration),
                    master(
                        VIDEO,
                        &[
                            uint_element(PIXEL_WIDTH, u64::from(self.width)),
                            uint_element(PIXEL_HEIGHT, u64::from(self.height)),
                        ],
                    ),
                ],
            )],
        )
    }

    /// The frames split into clusters: one starts at a key frame once the
    /// current cluster is half a second old, so every cluster decodes from
    /// its first block and a second of frames is a cluster. A cluster never
    /// lasts past five seconds even if the encoder stops producing key frames.
    fn clusters(&self, first_us: u64) -> Vec<Cluster> {
        let mut clusters: Vec<Cluster> = Vec::new();
        for chunk in &self.chunks {
            let offset_us = chunk.timestamp_us.saturating_sub(first_us);
            let starts = match clusters.last() {
                None => true,
                Some(cluster) => {
                    let since = offset_us.saturating_sub(cluster.timecode_ms.saturating_mul(1000));
                    (chunk.key && since >= 500_000) || since >= 5_000_000
                }
            };
            if starts {
                clusters.push(Cluster {
                    timecode_ms: offset_us / 1000,
                    blocks: Vec::new(),
                });
            }
            let Some(cluster) = clusters.last_mut() else {
                continue;
            };
            // A cluster is at most five seconds, which fits the signed
            // 16-bit relative timestamp a block carries.
            let relative =
                i16::try_from(offset_us / 1000 - cluster.timecode_ms).unwrap_or(i16::MAX);
            cluster.blocks.push(simple_block(relative, chunk));
        }
        clusters
    }
}

struct Cluster {
    timecode_ms: u64,
    /// One `SimpleBlock` payload per frame, in presentation order.
    blocks: Vec<Vec<u8>>,
}

fn simple_block(relative_ms: i16, chunk: &Chunk) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + chunk.data.len());
    out.push(0x81); // track number 1, one-byte vint
    out.extend_from_slice(&relative_ms.to_be_bytes());
    out.push(if chunk.key { 0x80 } else { 0x00 });
    out.extend_from_slice(&chunk.data);
    out
}

// EBML element ids, written as the bytes they are.
const EBML_HEADER: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
const EBML_VERSION: &[u8] = &[0x42, 0x86];
const EBML_READ_VERSION: &[u8] = &[0x42, 0xF7];
const EBML_MAX_ID_LENGTH: &[u8] = &[0x42, 0xF2];
const EBML_MAX_SIZE_LENGTH: &[u8] = &[0x42, 0xF3];
const DOC_TYPE: &[u8] = &[0x42, 0x82];
const DOC_TYPE_VERSION: &[u8] = &[0x42, 0x87];
const DOC_TYPE_READ_VERSION: &[u8] = &[0x42, 0x85];
const SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];
const INFO: &[u8] = &[0x15, 0x49, 0xA9, 0x66];
const TIMECODE_SCALE: &[u8] = &[0x2A, 0xD7, 0xB1];
const MUXING_APP: &[u8] = &[0x4D, 0x80];
const WRITING_APP: &[u8] = &[0x57, 0x41];
const DURATION: &[u8] = &[0x44, 0x89];
const TRACKS: &[u8] = &[0x16, 0x54, 0xAE, 0x6B];
const TRACK_ENTRY: &[u8] = &[0xAE];
const TRACK_NUMBER: &[u8] = &[0xD7];
const TRACK_UID: &[u8] = &[0x73, 0xC5];
const TRACK_TYPE: &[u8] = &[0x83];
const FLAG_LACING: &[u8] = &[0x9C];
const CODEC_ID: &[u8] = &[0x86];
const DEFAULT_DURATION: &[u8] = &[0x23, 0xE3, 0x83];
const VIDEO: &[u8] = &[0xE0];
const PIXEL_WIDTH: &[u8] = &[0xB0];
const PIXEL_HEIGHT: &[u8] = &[0xBA];
const CLUSTER: &[u8] = &[0x1F, 0x43, 0xB6, 0x75];
const TIMECODE: &[u8] = &[0xE7];
const SIMPLE_BLOCK: &[u8] = &[0xA3];
const CUES: &[u8] = &[0x1C, 0x53, 0xBB, 0x6B];
const CUE_POINT: &[u8] = &[0xBB];
const CUE_TIME: &[u8] = &[0xB3];
const CUE_TRACK_POSITIONS: &[u8] = &[0xB7];
const CUE_TRACK: &[u8] = &[0xF7];
const CUE_CLUSTER_POSITION: &[u8] = &[0xF1];

fn ebml_header() -> Vec<u8> {
    master(
        EBML_HEADER,
        &[
            uint_element(EBML_VERSION, 1),
            uint_element(EBML_READ_VERSION, 1),
            uint_element(EBML_MAX_ID_LENGTH, 4),
            uint_element(EBML_MAX_SIZE_LENGTH, 8),
            string_element(DOC_TYPE, "webm"),
            uint_element(DOC_TYPE_VERSION, 2),
            uint_element(DOC_TYPE_READ_VERSION, 2),
        ],
    )
}

/// A master element: its id, its body's size, its children.
fn master(id: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for child in children {
        body.extend_from_slice(child);
    }
    element(id, &body)
}

fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len().saturating_add(body.len()).saturating_add(8));
    out.extend_from_slice(id);
    out.extend_from_slice(&vint_size(body.len() as u64));
    out.extend_from_slice(body);
    out
}

fn uint_element(id: &[u8], value: u64) -> Vec<u8> {
    element(id, &uint(value))
}

/// An unsigned integer as EBML writes one: big-endian, zero bytes trimmed, at
/// least one byte.
fn uint(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let leading = bytes.iter().take_while(|byte| **byte == 0).count().min(7);
    bytes.iter().skip(leading).copied().collect()
}

fn string_element(id: &[u8], value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

/// An EBML size: a variable-length integer whose leading one bits say its
/// length.
fn vint_size(value: u64) -> Vec<u8> {
    let mut length = 1usize;
    while length < 8 && value >= (1u64 << (7 * length)) - 1 {
        length += 1;
    }
    let mut out = Vec::with_capacity(length);
    for index in (0..length).rev() {
        out.push(((value >> (8 * index)) & 0xFF) as u8);
    }
    if let Some(first) = out.first_mut() {
        *first |= 1u8 << (8 - length);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One element as the tests read it back: id, body.
    struct Parsed {
        id: Vec<u8>,
        body: Vec<u8>,
    }

    fn id_length(first: u8) -> usize {
        (1..=4)
            .find(|length| first & (0x80 >> (length - 1)) != 0)
            .unwrap_or(0)
    }

    fn size_length(first: u8) -> usize {
        (1..=8)
            .find(|length| first & (0x80 >> (length - 1)) != 0)
            .unwrap_or(0)
    }

    fn read_id(data: &[u8]) -> Option<(Vec<u8>, usize)> {
        let length = id_length(*data.first()?);
        (length > 0 && data.len() >= length).then(|| (data[..length].to_vec(), length))
    }

    fn read_size(data: &[u8]) -> Option<(usize, usize)> {
        let length = size_length(*data.first()?);
        if length == 0 || data.len() < length {
            return None;
        }
        let mut value = u64::from(data[0]) & (0xFFu64 >> length);
        for byte in &data[1..length] {
            value = value * 256 + u64::from(*byte);
        }
        Some((usize::try_from(value).ok()?, length))
    }

    /// The children of a master element's body, parsed exactly: a size that
    /// reaches past the body or leaves bytes over is a bug, not leniency.
    fn children(body: &[u8]) -> Vec<Parsed> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < body.len() {
            let (id, id_len) = read_id(&body[at..]).expect("element id");
            let (size, size_len) = read_size(&body[at + id_len..]).expect("element size");
            let start = at + id_len + size_len;
            let end = start + size;
            assert!(end <= body.len(), "element size reaches past its parent");
            out.push(Parsed {
                id,
                body: body[start..end].to_vec(),
            });
            at = end;
        }
        assert_eq!(at, body.len(), "children fill their parent exactly");
        out
    }

    fn find<'a>(nodes: &'a [Parsed], id: &[u8]) -> Option<&'a Parsed> {
        nodes.iter().find(|node| node.id == id)
    }

    fn uint_of(node: &Parsed) -> u64 {
        node.body
            .iter()
            .fold(0u64, |value, byte| value * 256 + u64::from(*byte))
    }

    fn text_of(node: &Parsed) -> String {
        String::from_utf8(node.body.clone()).expect("utf-8")
    }

    /// Five frames of three bytes at 30fps: the smallest clip carrying
    /// more than one frame, its timing, its key frames and a cluster.
    fn five_frames() -> Muxer {
        let mut muxer = Muxer::new(64, 48, 30);
        for index in 0..5u64 {
            muxer.push(index * 33_333, index == 0, vec![0x10, index as u8, 0xAA]);
        }
        muxer
    }

    /// The Segment's body of a well-formed clip: an EBML header naming `WebM`,
    /// then a segment that holds the whole file.
    fn segment_body(webm: &[u8]) -> &[u8] {
        let (header_id, header_len) = read_id(webm).expect("header id");
        assert_eq!(header_id, EBML_HEADER);
        let (header_size, header_size_len) = read_size(&webm[header_len..]).expect("header size");
        let header = children(&webm[header_len + header_size_len..][..header_size]);
        assert_eq!(
            text_of(find(&header, DOC_TYPE).expect("doctype")),
            "webm",
            "the document type is WebM"
        );
        let segment_at = header_len + header_size_len + header_size;
        let (segment_id, segment_len) = read_id(&webm[segment_at..]).expect("segment id");
        assert_eq!(segment_id, SEGMENT);
        let (segment_size, segment_size_len) =
            read_size(&webm[segment_at + segment_len..]).expect("segment size");
        assert_eq!(
            segment_at + segment_len + segment_size_len + segment_size,
            webm.len(),
            "the segment holds the whole file"
        );
        &webm[segment_at + segment_len + segment_size_len..][..segment_size]
    }

    #[test]
    fn encoded_frames_mux_into_webm_with_their_timing() {
        assert!(
            Muxer::new(64, 48, 30).bytes().is_none(),
            "no frames, no clip"
        );
        let muxer = five_frames();
        assert_eq!(muxer.duration_ms(), 166, "five frames at 30fps");
        let webm = muxer.bytes().expect("a clip");
        let segment = children(segment_body(&webm));

        let info = children(&find(&segment, INFO).expect("info").body);
        assert_eq!(
            uint_of(find(&info, TIMECODE_SCALE).expect("scale")),
            1_000_000
        );
        let duration = f64::from_bits(u64::from_be_bytes(
            find(&info, DURATION).expect("duration").body[..8]
                .try_into()
                .expect("eight bytes"),
        ));
        assert!((duration - 166.666).abs() < 0.01, "duration {duration}");

        let tracks = children(&find(&segment, TRACKS).expect("tracks").body);
        let track = find(&tracks, TRACK_ENTRY).expect("track entry");
        let track = children(&track.body);
        assert_eq!(
            uint_of(find(&track, TRACK_NUMBER).expect("track number")),
            1
        );
        assert_eq!(
            uint_of(find(&track, TRACK_TYPE).expect("track type")),
            1,
            "a video track"
        );
        assert_eq!(text_of(find(&track, CODEC_ID).expect("codec")), "V_VP8");
        assert_eq!(
            uint_of(find(&track, DEFAULT_DURATION).expect("default duration")),
            33_333_333
        );
        let video = children(&find(&track, VIDEO).expect("video").body);
        assert_eq!(uint_of(find(&video, PIXEL_WIDTH).expect("width")), 64);
        assert_eq!(uint_of(find(&video, PIXEL_HEIGHT).expect("height")), 48);

        let clusters: Vec<&Parsed> = segment.iter().filter(|node| node.id == CLUSTER).collect();
        assert_eq!(clusters.len(), 1, "five frames make one cluster");
        let cluster = children(&clusters[0].body);
        assert_eq!(uint_of(find(&cluster, TIMECODE).expect("cluster time")), 0);
        let mut blocks = cluster.iter().filter(|node| node.id == SIMPLE_BLOCK);
        for index in 0..5u64 {
            let block = blocks.next().expect("one block per frame");
            assert_eq!(block.body[0], 0x81, "track number 1");
            let relative = i16::from_be_bytes([block.body[1], block.body[2]]);
            assert_eq!(
                relative,
                i16::try_from((index * 33_333) / 1000).expect("relative ms")
            );
            assert_eq!(
                block.body[3],
                if index == 0 { 0x80 } else { 0x00 },
                "the first frame is a key frame and the rest are not"
            );
            assert_eq!(
                &block.body[4..],
                &[0x10, index as u8, 0xAA],
                "frame bytes are carried whole"
            );
        }
        assert!(blocks.next().is_none(), "no more blocks");
    }

    #[test]
    fn a_clip_is_webm_by_its_bytes_and_its_cues_point_at_its_clusters() {
        let webm = five_frames().bytes().expect("a clip");

        let body = segment_body(&webm);
        let segment = children(body);
        let cues = children(&find(&segment, CUES).expect("cues").body);
        let cue_points: Vec<&Parsed> = cues.iter().filter(|node| node.id == CUE_POINT).collect();
        assert_eq!(cue_points.len(), 1, "one cue per cluster");
        let cue = children(&cue_points[0].body);
        assert_eq!(uint_of(find(&cue, CUE_TIME).expect("cue time")), 0);
        let positions = children(
            &find(&cue, CUE_TRACK_POSITIONS)
                .expect("cue track positions")
                .body,
        );
        assert_eq!(uint_of(find(&positions, CUE_TRACK).expect("cue track")), 1);
        let position = uint_of(find(&positions, CUE_CLUSTER_POSITION).expect("position"));
        let position = usize::try_from(position).expect("in range");
        let (at, _) = read_id(&body[position..]).expect("a cluster at the cue's position");
        assert_eq!(at, CLUSTER, "the cue points at its cluster");
    }

    #[test]
    fn a_second_of_frames_starts_a_new_cluster_and_the_cues_follow_them() {
        let mut muxer = Muxer::new(320, 240, 30);
        // Three seconds of frames, a key frame every 30, as the page's
        // encoder is asked for.
        for index in 0..90u64 {
            muxer.push(index * 33_333, index % 30 == 0, vec![index as u8; 4]);
        }
        let webm = muxer.bytes().expect("a clip");
        let (_, header_len) = read_id(&webm).expect("header id");
        let (header_size, header_size_len) = read_size(&webm[header_len..]).expect("header size");
        let segment_at = header_len + header_size_len + header_size;
        let (segment_id, segment_len) = read_id(&webm[segment_at..]).expect("segment id");
        assert_eq!(segment_id, SEGMENT);
        let (size, size_len) = read_size(&webm[segment_at + segment_len..]).expect("segment size");
        let body = &webm[segment_at + segment_len + size_len..][..size];
        let segment = children(body);
        let clusters: Vec<&Parsed> = segment.iter().filter(|node| node.id == CLUSTER).collect();
        assert_eq!(clusters.len(), 3, "one cluster per second of key frames");
        let cues = children(&find(&segment, CUES).expect("cues").body);
        let cue_points: Vec<&Parsed> = cues.iter().filter(|node| node.id == CUE_POINT).collect();
        assert_eq!(cue_points.len(), 3);
        for (index, cue) in cue_points.iter().enumerate() {
            let cue = children(&cue.body);
            let positions = children(&find(&cue, CUE_TRACK_POSITIONS).expect("positions").body);
            let at = usize::try_from(uint_of(
                find(&positions, CUE_CLUSTER_POSITION).expect("position"),
            ))
            .expect("in range");
            let (id, _) = read_id(&body[at..]).expect("a cluster at the cue's position");
            assert_eq!(id, CLUSTER, "cue {index} points at a cluster");
            let time = uint_of(find(&cue, CUE_TIME).expect("cue time"));
            let expected = u64::try_from(index).expect("small") * 1000;
            assert!(
                time.abs_diff(expected) <= 1,
                "cue {index} names its cluster's second ({time}ms)"
            );
            assert_eq!(
                uint_of(find(&children(&clusters[index].body), TIMECODE).expect("cluster time")),
                time,
                "the cue and its cluster agree on the time"
            );
        }
    }
}
