//! The Chip Design app's engine, as a package reaches it: `engine.call`, `engine.start`,
//! `engine.status` and `engine.cancel` answered for `so.tilo.chip-design` by
//! [`tilo_chip_design_engine`] on this device.
//!
//! The app runs sandboxed, so it asks by name. An imported engineering document is checked here
//! before any of it reaches the workspace, and the structural operator design is resolved here.
//! Both answer now: a document is checked in one breath, so no ask of this engine is a job.
//! No other app is answered by this engine.

use std::sync::{Arc, OnceLock};

use tilolang_tilo::engine::Provider;

/// Register the engine once, at the host's first ask: every later one finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilolang_tilo::engine::register(tilo_chip_design_engine::APP, Arc::new(ChipDesign));
    });
}

/// The registry's entry for the Chip Design app.
struct ChipDesign;

impl Provider for ChipDesign {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_chip_design_engine::call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_chip_design_engine::start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        tilo_chip_design_engine::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_chip_design_engine::cancel(job)
    }
}
