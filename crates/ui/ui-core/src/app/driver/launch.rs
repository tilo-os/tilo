//! What a launch writes of an app shown by the driver: `--headless-drive`'s run of a spec's steps,
//! its audit and its output files, and the frame `--shots` and the capture steps photograph.
//!
//! A spec that names a scenario (`"scenario"`, or `--scenario`) has its app built from that
//! scenario's fixtures by the launcher; the scenario is staged here, before the steps, as
//! `--shots --scenario` stages it before its capture.

use tilo_sort::CompactSort as _;

use super::Driver;
use crate::headless::{Capture, DEFAULT_VIEWPORT, Spec};

impl Driver {
    #[cfg(feature = "model")]
    /// Drive the prepared `component` with `spec`'s steps, audit it and write the outputs the spec
    /// names. Returns 0 for success, 1 for a step or output failure, and 3 when the component
    /// could not be mounted or what it holds could not be kept.
    pub fn run_component(
        spec: &Spec,
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
    ) -> i32 {
        Self::run_component_staged(spec, component, inputs, |_| Ok(()))
    }

    #[cfg(feature = "model")]
    /// [`Self::run_component`], with `stage` run on the mounted window before the steps: the
    /// named scenario's selection, open menu or drag, as a capture of it shows them.
    pub fn run_component_staged(
        spec: &Spec,
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
        stage: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> i32 {
        tilo_startup_metrics::mark("headless-setup-start");
        let records = component.callback_binding().records;
        let mut driver = match Self::from_component(
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
        // Kept before anything runs, so a scenario that fails still writes the results it asked
        // for, and the failure reported is its own.
        if spec.results.is_some() {
            driver.retain_invocation_results();
        }
        let mut failure = stage(&mut driver)
            .map_err(|error| format!("scenario: {error}"))
            .err();
        if failure.is_none() {
            failure = driver.drive_spec_steps(spec);
        }
        driver.idle();
        if let Err(error) = driver
            .audit_spec(spec, &mut failure)
            .and_then(|()| driver.write_spec_outputs(spec))
        {
            tilo_startup_metrics::mark("headless-output-failed");
            // A failure already met is the cause; what could not be written after it follows it.
            failure = Some(match failure.take() {
                Some(cause) => format!("{cause}; then {error}"),
                None => error,
            });
        } else {
            tilo_startup_metrics::mark("headless-output-done");
        }
        tilo_startup_metrics::mark("headless-durability-start");
        driver.close_components();
        if driver.component_close_reports().iter().any(|report| {
            report.view_error.is_some()
                || report.resource_error.is_some()
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

    /// Wait for the controls the app pressed on its own to run, then run `spec`'s steps in order,
    /// offering the record's writer what each changed. Why the run stopped, if it did.
    fn drive_spec_steps(&mut self, spec: &Spec) -> Option<String> {
        tilo_startup_metrics::mark("headless-actions-start");
        if !self.wait_until(crate::time::Duration::from_secs(10), Self::controls_settled) {
            return Some("startup control invocation did not become ready".into());
        }
        if let Some(error) = self.control_failures().first() {
            return Some(format!("startup control invocation failed: {error}"));
        }
        self.persist_tick();
        let mut bound = crate::app::driver::drive::Bindings::new();
        // The same steps, run by the same loop, an in-process caller drives: this path only adds
        // the metrics and the wording a CLI failure prints.
        let steps = crate::app::driver::drive::steps_of(spec);
        for (index, step) in steps.iter().enumerate() {
            if let Err(error) = self.perform_bound(step, &mut bound) {
                tilo_startup_metrics::mark("headless-action-failed");
                tilo_startup_metrics::mark("headless-actions-done");
                return Some(format!(
                    "action {} of {} ({}): {error}",
                    index.saturating_add(1),
                    steps.len(),
                    step.name()
                ));
            }
            self.persist_tick();
        }
        tilo_startup_metrics::mark("headless-actions-done");
        None
    }

    /// The accessibility audit `spec` asks for: its issues printed, its report written, and the
    /// run failed when it is not clean and nothing failed before.
    fn audit_spec(&self, spec: &Spec, failure: &mut Option<String>) -> Result<(), String> {
        if !spec.audit && spec.audit_report.is_none() {
            return Ok(());
        }
        let audit = self.accessibility_audit();
        if spec.audit {
            for issue in &audit.issues {
                eprintln!(
                    "headless accessibility [{}] {}: {}",
                    issue.code, issue.path, issue.message
                );
            }
        }
        if let Some(path) = &spec.audit_report {
            let json = serde_json::to_string_pretty(&audit)
                .map_err(|error| format!("serializing accessibility audit: {error}"))?;
            write_output(path, json)
                .map_err(|error| format!("writing accessibility audit to {path}: {error}"))?;
        }
        if spec.audit && !audit.is_clean() && failure.is_none() {
            *failure = Some(format!(
                "accessibility audit found {} issue(s)",
                audit.issues.len()
            ));
        }
        Ok(())
    }

    /// Write the files `spec` names: the invocations' results, the addressable widgets, the
    /// document, the inspection, the published resources and the captures.
    ///
    /// # Errors
    /// When one cannot be made or written.
    pub fn write_spec_outputs(&mut self, spec: &Spec) -> Result<(), String> {
        if let Some(path) = &spec.results {
            write_output(path, self.invocation_results_json()?)
                .map_err(|error| format!("writing invocation results to {path}: {error}"))?;
        }
        if let Some(path) = &spec.targets {
            let mut names: Vec<(String, [f64; 4])> = self
                .targets()
                .into_iter()
                .map(|(name, rect)| (name, [rect.x0, rect.y0, rect.width(), rect.height()]))
                .collect();
            names.compact_sort_by(|a, b| a.0.cmp(&b.0));
            let json = serde_json::to_string_pretty(&names)
                .map_err(|error| format!("serializing targets: {error}"))?;
            write_output(path, json)
                .map_err(|error| format!("writing targets to {path}: {error}"))?;
        }
        if let Some(path) = &spec.dump {
            write_output(path, self.dump_json())
                .map_err(|error| format!("writing dump to {path}: {error}"))?;
        }
        if let Some(path) = &spec.inspect {
            let json = self
                .inspection_json()
                .map_err(|error| format!("serializing inspection: {error}"))?;
            write_output(path, json)
                .map_err(|error| format!("writing inspection to {path}: {error}"))?;
        }
        if let Some(path) = &spec.resources {
            let tree: Vec<serde_json::Value> = self
                .resources()
                .iter()
                .map(crate::app::driver::drive::resource_json)
                .collect();
            let json = serde_json::to_string_pretty(&tree)
                .map_err(|error| format!("serializing resources: {error}"))?;
            write_output(path, json)
                .map_err(|error| format!("writing resources to {path}: {error}"))?;
        }
        for capture in &spec.captures {
            self.write_capture(capture)?;
        }
        Ok(())
    }

    /// Take one [`Capture`]: switch theme and size when it names them, let the window settle as a
    /// person would see it, and write the frame as a PNG.
    fn write_capture(&mut self, capture: &Capture) -> Result<(), String> {
        if let Some(name) = &capture.theme {
            let theme = crate::theme::Theme::named(name)
                .ok_or_else(|| format!("capture {}: unknown theme {name:?}", capture.path))?;
            self.set_theme(theme.with_reduce_motion(true));
        }
        if let Some(viewport) = capture.viewport {
            self.resize(viewport)
                .map_err(|error| format!("capture {}: layout failed: {error}", capture.path))?;
        }
        self.settle_for_capture(crate::launch::CAPTURE_SETTLE_BOUND);
        let path = std::path::Path::new(&capture.path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tilo_files::native::create_dir_all(parent)
                .map_err(|error| format!("capture {}: {error}", capture.path))?;
        }
        self.render_png(path);
        Ok(())
    }

    /// Write the `--shots` capture at `out`: the app's frame alone, or under the window's bar when
    /// `window_bar` is set.
    ///
    /// The frame is the window a person sees a moment after it opened: the capture settles first,
    /// as a named capture does, so an entrance still running, a scroll bar still fading and a page
    /// whose sharp bitmap has not arrived are all waited out.
    #[cfg(feature = "native")]
    pub(crate) fn write_shot(&mut self, out: &std::path::Path, window_bar: bool) {
        if window_bar {
            self.present_in_window_bar();
        }
        self.settle_for_capture(crate::launch::CAPTURE_SETTLE_BOUND);
        // What the window shows under `protect capture`, or a secret field holding a value, is
        // left out of a capture: it is the ground the capture stands on, as a capture of a
        // protected window shows a placeholder where it is.
        if self.capture_protected() {
            self.render_ground_png(out);
            return;
        }
        self.prepare_component_output();
        self.render_png(out);
    }

    /// The frame a capture or an audit reads last: everything the app's components changed since
    /// the frame before is taken up ([`Self::refresh_model`]) and laid out.
    #[cfg(feature = "native")]
    pub(crate) fn prepare_component_output(&mut self) {
        self.refresh_model();
        self.idle();
    }
}

/// Write one of the drive's outputs at `path`, making its directory.
fn write_output(
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
