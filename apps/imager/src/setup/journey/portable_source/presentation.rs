//! Source-consent presentation. The backend owns matching directory grants;
//! choices, capture intents and native worker state still belong to the frontend.
use super::native_ownership::{FrontendNativeActor as Actor, FrontendNativeCustody};
use std::sync::Arc;

pub(crate) struct OriginalSource {
    pub(super) native_payload: FrontendNativeCustody,
    pub(super) applications_only: fn(&OriginalSource) -> bool,
}
impl OriginalSource {
    pub fn applications_only(&self) -> bool {
        (self.applications_only)(self)
    }
}
pub(super) trait SelectionState: std::any::Any {
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn busy(&self) -> bool;
    fn choosing(&self) -> bool;
    fn refresh(&mut self) -> Result<(), &'static str>;
    fn poll(&mut self) -> Option<Result<bool, &'static str>>;
    fn reviewed_original(&self) -> Option<Arc<OriginalSource>>;
    fn invalidate(&mut self);
    fn mounted_permission_active(&self) -> bool;
    fn mounted_needs_restart(&self) -> bool;
    fn mounted_choices(&self) -> Vec<(String, String, bool)>;
    fn request_mounted(&mut self, location: String) -> Result<(), &'static str>;
    fn finish_mounted(&mut self) -> Result<(), &'static str>;
    fn cancel_mounted(&mut self);
    fn mounted_locations(&self) -> Vec<String>;
    fn projection(&self) -> serde_json::Value;
}
pub(super) struct Selection {
    pub(super) native: Actor<dyn SelectionState>,
}
impl Default for Selection {
    fn default() -> Self {
        #[cfg(not(target_os = "linux"))]
        let native = Actor::default();
        #[cfg(target_os = "linux")]
        let native = Actor::new(
            Box::new(super::portable_source_native::Selection::default())
                as Box<dyn SelectionState>,
        );
        Self { native }
    }
}
impl Selection {
    #[cfg(target_os = "linux")]
    fn native_selection(
        &mut self,
    ) -> Result<&mut super::portable_source_native::Selection, &'static str> {
        self.native
            .get_mut()
            .map_err(|_| "installer-portable-source-failed")?
            .as_any_mut()
            .downcast_mut()
            .ok_or("installer-portable-source-failed")
    }

    pub fn request_with_choices(
        &mut self,
        home: std::path::PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_selection()?.request_with_choices(home, choices)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (home, choices);
            Err("installer-portable-source-failed")
        }
    }

    pub fn start_with_choices(
        &mut self,
        selected: std::path::PathBuf,
        expected: std::path::PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_selection()?
                .start_with_choices(selected, expected, choices)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (selected, expected, choices);
            Err("installer-portable-source-failed")
        }
    }

    pub fn refresh_mounted(
        &mut self,
        expected: std::path::PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            self.native_selection()?.refresh_mounted(expected, choices)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (expected, choices);
            Err("installer-portable-source-failed")
        }
    }

    pub fn reviewed_handle(
        &self,
    ) -> Option<Arc<imager_backend::setup::portable_home_media::HomeSource>> {
        #[cfg(target_os = "linux")]
        {
            self.native
                .get()
                .ok()?
                .as_any()
                .downcast_ref::<super::portable_source_native::Selection>()?
                .reviewed_handle()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    pub fn reviewed_applications(
        &self,
    ) -> Option<Arc<imager_backend::setup::flatpak_transfer::portable::PortableFlatpakSource>> {
        #[cfg(target_os = "linux")]
        {
            self.native
                .get()
                .ok()?
                .as_any()
                .downcast_ref::<super::portable_source_native::Selection>()?
                .reviewed_applications()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
    pub fn busy(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.busy(),
            Err(_unsupported) => false,
        }
    }
    pub fn choosing(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.choosing(),
            Err(_unsupported) => false,
        }
    }
    pub fn refresh(&mut self) -> Result<(), &'static str> {
        match self.native.get_mut() {
            Ok(native) => native.refresh(),
            Err(_unsupported) => Err("installer-portable-source-failed"),
        }
    }
    pub fn poll(&mut self) -> Option<Result<bool, &'static str>> {
        match self.native.get_mut() {
            Ok(native) => native.poll(),
            Err(_unsupported) => None,
        }
    }
    pub fn reviewed_original(&self) -> Option<Arc<OriginalSource>> {
        match self.native.get() {
            Ok(native) => native.reviewed_original(),
            Err(_unsupported) => None,
        }
    }
    pub fn invalidate(&mut self) {
        match self.native.get_mut() {
            Ok(native) => native.invalidate(),
            Err(_unsupported) => (),
        }
    }
    pub fn mounted_permission_active(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.mounted_permission_active(),
            Err(_unsupported) => false,
        }
    }
    pub fn mounted_needs_restart(&self) -> bool {
        match self.native.get() {
            Ok(native) => native.mounted_needs_restart(),
            Err(_unsupported) => false,
        }
    }
    pub fn mounted_choices(&self) -> Vec<(String, String, bool)> {
        match self.native.get() {
            Ok(native) => native.mounted_choices(),
            Err(_unsupported) => Vec::new(),
        }
    }
    pub fn request_mounted(&mut self, location: String) -> Result<(), &'static str> {
        match self.native.get_mut() {
            Ok(native) => native.request_mounted(location),
            Err(_unsupported) => Err("installer-portable-source-failed"),
        }
    }
    pub fn finish_mounted(&mut self) -> Result<(), &'static str> {
        match self.native.get_mut() {
            Ok(native) => native.finish_mounted(),
            Err(_unsupported) => Err("installer-portable-source-failed"),
        }
    }
    pub fn cancel_mounted(&mut self) {
        match self.native.get_mut() {
            Ok(native) => native.cancel_mounted(),
            Err(_unsupported) => (),
        }
    }
    pub fn mounted_locations(&self) -> Vec<String> {
        match self.native.get() {
            Ok(native) => native.mounted_locations(),
            Err(_unsupported) => Vec::new(),
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        match self.native.get() {
            Ok(native) => native.projection(),
            Err(_unsupported) => {
                serde_json::json!({"state":"idle","capture":null,"mounted_locations":[],"mounted_permissions":null,"prepared":false,"applications":false,"files":null,"bytes":null})
            }
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct Choices {
    pub home: bool,
    pub applications: bool,
    pub accounts: bool,
    pub system_refs: Vec<imager_backend::setup::flatpak_transfer::InstalledRef>,
    pub preparation: Option<Arc<imager_backend::prepared_identity::PreparedIdentity>>,
    /// Root-relative Home subtrees the person excluded. The review applies them
    /// to the inventory, so the sealed archive and its verification never carry
    /// an excluded folder.
    pub exclusions: Vec<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_actor_cannot_grant_or_refresh_source_consent() {
        let mut selection = Selection {
            native: Actor::default(),
        };
        let expected = std::path::PathBuf::from("reviewed-home");
        assert!(
            selection
                .request_with_choices(expected.clone(), Choices::default())
                .is_err()
        );
        assert!(
            selection
                .start_with_choices(expected.clone(), expected.clone(), Choices::default())
                .is_err()
        );
        assert!(
            selection
                .refresh_mounted(expected, Choices::default())
                .is_err()
        );
        assert!(selection.reviewed_handle().is_none());
        assert!(selection.reviewed_applications().is_none());
        assert!(selection.reviewed_original().is_none());
        assert!(!selection.busy());
        assert!(!selection.choosing());
    }
}
