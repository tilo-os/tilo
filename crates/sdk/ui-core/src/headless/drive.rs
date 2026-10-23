//! `--headless-drive`: a prepared component driven by a spec's actions,
//! then audited, captured and written out. A spec that names a scenario
//! (`"scenario"`, or `--scenario`) has its app built from that scenario's
//! fixtures by the launcher; the scenario is staged here, before the
//! actions, as `--shots --scenario` stages it before its capture.

use super::{
    DEFAULT_VIEWPORT, FrameHarness, Spec, audit_spec, drive_spec_actions, write_spec_outputs,
};

/// Drive the same prepared component with existing actions, audit, and output formats.
/// Returns 0 for success, 1 for an action/output failure, and 3 for durability failure.
pub fn run_component(
    spec: &Spec,
    component: crate::application::AppComponent,
    inputs: crate::application::ComponentRunnerInputs,
) -> i32 {
    run_component_staged(spec, component, inputs, |_| Ok(()))
}

/// [`run_component`], with `stage` run on the mounted window before the
/// actions: the named scenario's selection, open menu or drag, as a capture
/// of it shows them. A stage that fails fails the drive (1).
pub fn run_component_staged(
    spec: &Spec,
    component: crate::application::AppComponent,
    inputs: crate::application::ComponentRunnerInputs,
    stage: impl FnOnce(&mut FrameHarness) -> Result<(), String>,
) -> i32 {
    tilo_startup_metrics::mark("headless-setup-start");
    let records = component.callback_binding().records;
    let mut driver = match FrameHarness::from_component(
        component,
        spec.viewport.unwrap_or(DEFAULT_VIEWPORT),
        inputs,
    ) {
        Ok(driver) => driver,
        Err(error) => {
            tilo_startup_metrics::mark("headless-setup-failed");
            eprintln!("headless: component setup failed: {error}");
            return 3;
        }
    };
    tilo_startup_metrics::mark("headless-setup-ready");
    if spec.window_bar {
        driver.present_in_window_bar();
    }
    let mut failure = stage(&mut driver)
        .map_err(|error| format!("scenario: {error}"))
        .err();
    if failure.is_none() {
        failure = drive_spec_actions(spec, &mut driver, |driver| {
            driver.persist_tick();
        });
    }
    driver.pump_component_wakes();
    if let Err(error) =
        audit_spec(spec, &driver, &mut failure).and_then(|()| write_spec_outputs(spec, &mut driver))
    {
        tilo_startup_metrics::mark("headless-output-failed");
        failure = Some(error);
    } else {
        tilo_startup_metrics::mark("headless-output-done");
    }
    tilo_startup_metrics::mark("headless-durability-start");
    driver.close_components();
    let reports = driver.component_close_reports();
    if reports.iter().any(|report| {
        report.view_error.is_some()
            || report.record_error.is_some()
            || report.teardown_error.is_some()
    }) {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: component close could not persist all state");
        return 3;
    }
    if let Err(error) = records.try_flush() {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: state write failed: {error}");
        return 3;
    }
    tilo_startup_metrics::mark("headless-durability-done");
    match failure {
        Some(error) => {
            eprintln!("headless: {error}");
            1
        }
        None => 0,
    }
}

/// Write one of the drive's outputs at `path`, making its directory.
pub(super) fn write_headless_output(
    path: impl AsRef<std::path::Path>,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}
