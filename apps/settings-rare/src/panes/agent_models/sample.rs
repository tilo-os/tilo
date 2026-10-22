//! The model manager a screenshot shows, standing in for the device's.

use super::{
    HardwareFit, HardwareRecommendation, LocalModelControl, LocalModelPackage, LocalModelSnapshot,
};
use crate::i18n::t;

/// The model manager a screenshot and the crate's tests show: a good fit,
/// one recommended model, nothing installed. Every request succeeds.
pub(super) struct SampleManager;

impl LocalModelControl for SampleManager {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        crate::device_read::off_ui_thread();
        Ok(LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 32,
                accelerator: "GPU with 16 GiB".into(),
                accelerator_memory_gib: Some(16),
                free_storage_gib: Some(120),
                reason: t("agent-reason-good"),
            },
            recommendation: Some(LocalModelPackage {
                id: "sample/helpful-7b".into(),
                name: "Helpful 7B".into(),
                description: "A capable assistant model for this device".into(),
                source: "huggingface.co/sample/helpful-7b".into(),
                revision: "0123456789abcdef".into(),
                license: "Apache 2.0".into(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".into(),
                download_bytes: 5_368_709_120,
                installed_bytes: 10_737_418_240,
                vision: false,
                face_recognition: false,
                in_process: false,
                segmentation: false,
                no_runtime: false,
            }),
            models: Vec::new(),
            in_process: Vec::new(),
            accelerator_setup: None,
        })
    }
    fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
}
