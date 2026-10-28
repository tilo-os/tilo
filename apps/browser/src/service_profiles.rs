//! Host root admission for the website-session backend.

mod native;
pub use browser_continuity::service_profiles::supported;
pub use native::Profile;
/// The session engine runs and cleans up in a profile through this.
impl browser_continuity::session_engine::Profile for Profile {
    fn path(&self) -> std::io::Result<&std::path::Path> {
        self.path()
    }
    fn cleanup_after_engine_exit(&mut self) -> std::io::Result<()> {
        self.cleanup_after_engine_exit()
    }
}
