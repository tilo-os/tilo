//! Provider boundary for `qos-compatd`; no runner commands are constructed in UI code.
use crate::model::{AppStatus, ApplianceState, Catalogue, Confidence, Grant, Route, WindowsApp};

pub trait CompatibilityProvider: Send {
    fn catalogue(&self) -> Catalogue;
    /// Sets a user route override.
    ///
    /// # Errors
    ///
    /// Returns an error when the service rejects the app or requested route.
    fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String>;
    /// Removes a user route override.
    ///
    /// # Errors
    ///
    /// Returns an error when the service does not know the app.
    fn reset_route_override(&mut self, app_id: &str) -> Result<(), String>;
}

/// Predictable fixture. It deliberately looks like the service response, not a
/// mock of Wine: code depending on it cannot accidentally gain host execution.
pub struct SampleProvider {
    catalogue: Catalogue,
}
impl SampleProvider {
    pub fn new() -> Self {
        let grants = |items: &[(&str, &str, bool)]| {
            items
                .iter()
                .map(|(label, detail, enabled)| Grant {
                    label: (*label).to_string(),
                    detail: (*detail).to_string(),
                    enabled: *enabled,
                })
                .collect()
        };
        Self { catalogue: Catalogue {
            apps: vec![
                WindowsApp { id: "adobe-reader".into(), name: "Adobe Acrobat Reader".into(), publisher: "Adobe".into(), route: Route::Wine, recommended_route: Route::Wine, runner: "Wine 11.0 (qOS pinned)".into(), capsule: "Adobe Reader".into(), prefix: "capsules/adobe-reader".into(), catalogue_source: "qOS compatibility catalogue".into(), confidence: Confidence::Verified, reason: "Verified desktop-app recipe; isolated Wine capsule is the smallest compatible layer.".into(), status: AppStatus::Ready, grants: grants(&[("Downloads", "Read and write", true), ("Clipboard", "On demand", true), ("Camera", "Not granted", false)]), fallback_detail: "If it stops working, repair this capsule or open it in the managed Windows VM.".into() },
                WindowsApp { id: "cyberpunk-2077".into(), name: "Cyberpunk 2077".into(), publisher: "CD PROJEKT RED".into(), route: Route::Proton, recommended_route: Route::Proton, runner: "UMU-Proton 10.0-3".into(), capsule: "Steam · Cyberpunk 2077".into(), prefix: "games/cyberpunk-2077".into(), catalogue_source: "Steam + ProtonDB recipe".into(), confidence: Confidence::Community, reason: "Game route uses Steam Runtime and UMU-Proton, including the game's known compatibility fixups.".into(), status: AppStatus::Running, grants: grants(&[("Games library", "Read and write", true), ("Controller", "Connected devices", true), ("Microphone", "Not granted", false)]), fallback_detail: "The Windows VM is offered only after Proton repair options are exhausted.".into() },
                WindowsApp { id: "visual-studio".into(), name: "Visual Studio".into(), publisher: "Microsoft".into(), route: Route::Vm, recommended_route: Route::Vm, runner: "Windows 11 managed VM".into(), capsule: "Windows workspace".into(), prefix: "vm/windows-workspace".into(), catalogue_source: "qOS fallback policy".into(), confidence: Confidence::Fallback, reason: "Requires Windows services not safely reproduced in a Wine capsule; it opens as an integrated guest app.".into(), status: AppStatus::Ready, grants: grants(&[("Workspace exchange", "Selected files only", true), ("Clipboard", "On demand", true), ("USB devices", "Not granted", false)]), fallback_detail: "One shared VM is suspended when idle. Its desktop is used only for setup or recovery.".into() },
            ], vm: ApplianceState { ready: true, detail: "Managed Windows workspace · suspended when idle · 2 guest apps available".into(), running_apps: 0 } } }
    }
}
impl Default for SampleProvider {
    fn default() -> Self {
        Self::new()
    }
}
impl CompatibilityProvider for SampleProvider {
    fn catalogue(&self) -> Catalogue {
        self.catalogue.clone()
    }
    fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.route = route;
        app.runner = match route {
            Route::Wine => "Wine 11.0 (qOS pinned)",
            Route::Proton => "UMU-Proton 10.0-3",
            Route::Vm => "Windows 11 managed VM",
        }
        .into();
        Ok(())
    }
    fn reset_route_override(&mut self, app_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.route = app.recommended_route;
        Ok(())
    }
}
