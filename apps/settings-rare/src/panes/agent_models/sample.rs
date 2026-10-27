//! The model manager a screenshot shows, standing in for the device's.

use agent_provider_config::local::hub::{HubFormat, HubSearchResult};

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

impl super::hub::HubControl for SampleManager {
    /// Two sample models on Hugging Face, one per file family, found by
    /// any part of their name.
    fn hub_search(&self, query: &str, format: &str) -> Result<Vec<HubSearchResult>, String> {
        crate::device_read::off_ui_thread();
        let sample = |repo: &str, downloads, likes, format| HubSearchResult {
            repo: repo.into(),
            downloads,
            likes,
            gated: false,
            pipeline: Some("text-generation".into()),
            license: Some("apache-2.0".into()),
            formats: vec![format],
        };
        let query = query.to_lowercase();
        Ok([
            sample("sample/helpful-7b-gguf", 48_210, 1, HubFormat::Gguf),
            sample("sample/helpful-7b", 1, 312, HubFormat::Safetensors),
        ]
        .into_iter()
        .filter(|result| {
            result.repo.contains(&query)
                && result
                    .formats
                    .iter()
                    .any(|family| super::hub::format_name(*family) == format)
        })
        .collect())
    }
}
