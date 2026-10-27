//! The image writer's view of a prepared installer medium, in a build without
//! the installation (`installation`, apps/imager/Cargo.toml).
//!
//! Only the installer journey prepares a medium that carries a person's
//! identity, belongings and original files, and only it hands the writer a
//! [`prepared_media::PreparedMedia`](crate::setup::journey::prepared_media::PreparedMedia) or a [`portable_source::OriginalSource`](crate::setup::journey::portable_source::OriginalSource).
//! Without it neither can exist: both are uninhabited, so every path of the
//! writer that would deliver, refresh or complete a prepared medium is
//! statically unreachable, and none of the native preparation, capture or
//! delivery code is linked. [`prepared_media::Delivery`](crate::setup::journey::prepared_media::Delivery) answers what the
//! journey's own delivery answers when no native preparation is available
//! (`journey/prepared_media/presentation.rs`): nothing is pending, and a
//! request to deliver is refused.

/// The original system's files a person granted to the installer journey.
pub mod portable_source {
    /// Never constructed without the installation.
    pub struct OriginalSource(pub(crate) std::convert::Infallible);

    impl OriginalSource {
        /// Whether only the original applications are carried over.
        pub fn applications_only(&self) -> bool {
            match self.0 {}
        }
    }
}

/// A prepared installer medium and its delivery after the write.
pub mod prepared_media {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use imager_backend::imager_model::{InstallProfile, PreparedSetupExpectation, TargetDrive};

    pub use imager_backend::journey_media::ReturnReason;

    use super::portable_source::OriginalSource;
    use crate::setup::original_admission::ReviewedOriginalImage;

    /// Never constructed without the installation.
    pub struct PreparedMedia(pub(crate) std::convert::Infallible);

    impl PreparedMedia {
        pub fn is_other_device(&self) -> bool {
            false
        }
        pub fn expectation(&self) -> &PreparedSetupExpectation {
            match self.0 {}
        }
        pub fn account_custody_protocol(&self) -> Option<u32> {
            match self.0 {}
        }
        pub fn service_session_protocol(&self) -> Option<u32> {
            match self.0 {}
        }
        pub fn applications_protocol(&self) -> Option<u32> {
            match self.0 {}
        }
        pub fn home_archive_protocol(&self) -> Option<u32> {
            match self.0 {}
        }
        pub fn install_profile_draft(&self) -> io::Result<Option<InstallProfile>> {
            match self.0 {}
        }
        pub(crate) fn prepare_original_on_media(
            self: &Arc<Self>,
            _image_review: &ReviewedOriginalImage,
            _target: &TargetDrive,
            _original: &OriginalSource,
            _cancel: &AtomicBool,
        ) -> io::Result<PreparedOriginalMedia> {
            match self.0 {}
        }
    }

    /// A medium prepared to carry the original system's files.
    pub struct PreparedOriginalMedia {
        pub artifact: Arc<PreparedMedia>,
        pub review: Arc<offline::OfflineMediaReview>,
    }

    /// Refreshing a prepared medium before its write.
    pub fn refresh_for_write(
        prepared: &Arc<PreparedMedia>,
        _cancel: &AtomicBool,
    ) -> io::Result<Arc<PreparedMedia>> {
        match prepared.0 {}
    }

    /// No native source review exists to have failed.
    pub fn source_review_reason(_error: &io::Error) -> Option<ReturnReason> {
        None
    }

    /// No drive carries a pending prepared medium.
    pub fn pending_target(_stable_id: &str) -> bool {
        false
    }

    /// Nothing to retire: without native preparation no drive carries a
    /// prepared medium ([`pending_target`] is always false), and closing the
    /// window retires each target it touched, which must not fail here.
    pub fn finish_target(_stable_id: &str) -> io::Result<()> {
        Ok(())
    }

    /// The journey's navigation back to a prepared medium.
    #[derive(Clone)]
    pub struct Navigation {
        pub artifact: ui::LocalState<Option<Arc<PreparedMedia>>>,
        pub showing_media: ui::LocalState<bool>,
        pub reason: ui::LocalState<Option<ReturnReason>>,
    }

    impl Navigation {
        pub fn review_home(&self, reason: ReturnReason) {
            self.reason.set(Some(reason));
            self.showing_media.set(false);
        }
    }

    /// The delivery of a prepared medium after its write: never one here.
    #[derive(Default)]
    pub struct Delivery {
        _none: (),
    }

    impl Delivery {
        pub fn is_other_device(&self) -> bool {
            false
        }
        pub fn with_artifact(artifact: Arc<PreparedMedia>) -> Self {
            match artifact.0 {}
        }
        pub fn start_awaiting_offline_export_automatic(
            &mut self,
            review: Arc<offline::OfflineMediaReview>,
        ) -> Result<(), &'static str> {
            match review.0 {}
        }
        pub fn finish_close(&mut self) -> io::Result<()> {
            Ok(())
        }
        pub fn has_artifact(&self, _artifact: &Arc<PreparedMedia>) -> bool {
            false
        }
        pub fn artifact(&self) -> Option<Arc<PreparedMedia>> {
            None
        }
        pub fn mark_source_changed(&mut self) {}
        pub fn mark_source_unavailable(&mut self) {}
        pub fn review_reason(&self) -> Option<ReturnReason> {
            None
        }
        pub fn reserved_data_bytes(&self) -> u64 {
            0
        }
        pub fn expectation(&self) -> Option<&PreparedSetupExpectation> {
            None
        }
        pub fn requires_data_partition(&self) -> bool {
            false
        }
        pub fn writing(&mut self) {}
        pub fn written(&mut self, _target: TargetDrive) {}
        pub fn awaiting_original_boot(&self) -> bool {
            false
        }
        pub fn original_boot_drive(&self) -> Option<String> {
            None
        }
        pub fn pending(&self) -> bool {
            false
        }
        pub fn busy(&self) -> bool {
            false
        }
        pub fn start(&mut self, _root: std::path::PathBuf) -> Result<(), &'static str> {
            Err("installer-media-delivery-failed")
        }
        pub fn requires_offline_export(&self) -> bool {
            false
        }
        pub fn start_automatic(&mut self) -> Result<(), &'static str> {
            Err("installer-media-delivery-failed")
        }
        pub fn progress_label(&self) -> Option<String> {
            None
        }
        pub fn cancel(&self) -> Result<(), &'static str> {
            Err("installer-media-delivery-not-running")
        }
        pub fn poll(&mut self) -> bool {
            false
        }
        pub fn needs_source_review(&self) -> bool {
            false
        }
        pub fn failed(&self) -> bool {
            false
        }
        pub fn projection(&self) -> serde_json::Value {
            serde_json::json!({"state":"none","next_step":null,"receipt":null,"awaiting_setup_receipt":null,"complete":false,"home_receipt":null,"phase":null,"completed":null,"total":null,"cancelling":false})
        }
    }

    /// The native review of a medium that carries the original files.
    pub mod offline {
        /// Never constructed without the installation.
        pub struct OfflineMediaReview(pub(crate) std::convert::Infallible);

        impl OfflineMediaReview {
            #[cfg(target_os = "linux")]
            pub fn attach_request(
                &self,
                _artifact: &super::PreparedMedia,
                _request: &mut crate::write::WriteRequest,
            ) -> std::io::Result<()> {
                match self.0 {}
            }
        }
    }
}
