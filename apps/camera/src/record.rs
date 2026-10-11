//! The camera's record: the one recent capture it hands off and the codes
//! it scanned.
//!
//! The photo and video *pixels* live as verified bytes in the app's content
//! store (see [`crate::media`]); a local capture file is merely an optional
//! projection. The record holds portable capture metadata naming those bytes
//! by their content-addressed media id, never a path, plus the scan history.
//! Capture preferences are settings ([`crate::model::CapturePreferences`]),
//! not record state. Camera deliberately keeps one recent handoff, not a
//! parallel photo/video library: Photos and Video Player own collections.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, ActionCx, JsonSchema, Model, Refusal, resource};

use crate::model::{CaptureKind, Facing, ScanEntry};

/// How many scanned codes the history keeps.
pub const MAX_SCANS: usize = 24;
/// How long a scanned code may be, in bytes.
const MAX_SCAN_BYTES: usize = 16 * 1024;

/// Portable capture metadata. The pixels live under `media_id` in the app's
/// content store, verified on read; the record holds no path.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[model(check = SharedCapture::valid)]
pub struct SharedCapture {
    /// The capture's name: its file name on the device that took it.
    pub id: String,
    /// The content-addressed id of its bytes.
    pub media_id: String,
    /// Photo or video
    pub kind: CaptureKind,
    /// When it was taken, in Unix seconds.
    pub ts: i64,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// How long a video runs, in milliseconds.
    pub dur_ms: i64,
    /// Name
    #[model(label)]
    pub label: String,
    /// The camera that took it.
    pub facing: Facing,
}

impl SharedCapture {
    fn valid(&self) -> Result<(), Refusal> {
        if self.id.is_empty()
            || self.id.len() > 160
            || !crate::media::is_media_id(&self.media_id)
            || self.w == 0
            || self.h == 0
            || u64::from(self.w).saturating_mul(u64::from(self.h)) > 33_554_432
            || self.label.len() > 256
            || self.ts < 0
            || self.dur_ms < 0
        {
            return Err(Refusal::new("capture metadata is invalid"));
        }
        Ok(())
    }
}

/// The camera's record.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// Scanned codes, newest first.
    #[model(readonly)]
    pub scans: Vec<ScanEntry>,
    /// The capture taken last, to hand off to Photos or Video Player.
    #[model(readonly)]
    pub recent: Option<SharedCapture>,
}

#[resource(kind = "camera", label = "Camera", search = Record::search)]
impl Record {
    /// Hand off a capture.
    ///
    /// The capture taken last, whose verified bytes are in the content
    /// store, becomes the one Camera offers to open in Photos or Video
    /// Player.
    #[action]
    pub fn register_capture(&mut self, #[param("The capture")] capture: SharedCapture) {
        self.recent = Some(capture);
    }

    /// Record a scanned code.
    ///
    /// It goes to the front of the history; the same code scanned again
    /// moves there rather than repeating.
    #[action]
    pub fn record_scan(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the code said")] raw: String,
    ) -> Result<(), Refusal> {
        if raw.is_empty() || raw.len() > MAX_SCAN_BYTES {
            return Err(Refusal::new("scan payload is invalid or too large"));
        }
        self.scans.retain(|scan| scan.raw != raw);
        self.scans.insert(
            0,
            ScanEntry {
                raw,
                ts: cx.now_ms().div_euclid(1000),
            },
        );
        self.scans.truncate(MAX_SCANS);
        Ok(())
    }
}

impl Record {
    /// The recent capture and every scanned code, found by what they say.
    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        let recent = self.recent.iter().map(|capture| {
            let item = crate::actions::capture_item_from_shared(capture);
            tilo_resource::SearchEntry::new(capture.label.clone(), "recent capture")
                .anchored(crate::actions::capture_resource_id(&item))
        });
        let scans = self
            .scans
            .iter()
            .map(|scan| tilo_resource::SearchEntry::new(scan.raw.clone(), "scan"));
        recent.chain(scans).collect()
    }
}

#[cfg(test)]
mod tests;
