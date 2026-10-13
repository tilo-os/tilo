//! Goal-oriented UX scenarios driven through the real widget host.

mod checks;
#[cfg(not(target_arch = "wasm32"))]
mod runner;
pub use checks::{gap, overlap_area, pixel_contrast};
#[cfg(not(target_arch = "wasm32"))]
pub use runner::{Report, run, run_with_state};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    X,
    Y,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub clicks: u32,
    pub keys: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub goal: String,
    #[serde(default = "default_fixture")]
    pub fixture: String,
    pub viewport: [u32; 2],
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default = "default_scale")]
    pub text_scale: f64,
    #[serde(default = "default_theme")]
    pub theme: String,
    pub budget: Budget,
    pub steps: Vec<Step>,
}
fn default_scale() -> f64 {
    1.0
}
fn default_theme() -> String {
    "dark".into()
}
fn default_fixture() -> String {
    "desktop".into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    WaitState {
        pointer: String,
        equals: Value,
        timeout_ms: u64,
    },
    Input {
        action: Value,
    },
    Wheel {
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        target: Option<String>,
        delta: [f64; 2],
        unit: WheelUnit,
        phase: WheelPhase,
    },
    Checkpoint {
        name: String,
        checks: Vec<Check>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WheelUnit {
    Lines,
    PhysicalPixels,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WheelPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// A fixed screen region or a viewport-relative inset from a real target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ImageRegion {
    Pixels([u32; 4]),
    Target { target: String, inset: [u32; 4] },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    Visible {
        target: String,
    },
    SemanticValue {
        target: String,
        equals: String,
    },
    Selected {
        target: String,
    },
    Absent {
        target: String,
    },
    Gap {
        first: String,
        second: String,
        axis: Axis,
        min: f64,
        max: f64,
    },
    Aligned {
        first: String,
        second: String,
        axis: Axis,
        tolerance: f64,
    },
    NoOverlap {
        first: String,
        second: String,
    },
    TargetSize {
        target: String,
        min: [f64; 2],
    },
    Contrast {
        label: String,
        foreground: [u32; 2],
        background: [u32; 2],
        min: f64,
    },
    Moved {
        target: String,
        from: String,
        axis: Axis,
        min: f64,
        max: f64,
    },
    ImageUnchanged {
        from: String,
        region: ImageRegion,
    },
    ImageShift {
        from: String,
        region: ImageRegion,
        axis: Axis,
        min: f64,
        max: f64,
    },
    Resource {
        id: String,
    },
    Semantic {
        expectation: Value,
    },
    State {
        pointer: String,
        equals: Value,
    },
}

impl Scenario {
    /// Parse a bounded journey. The final checkpoint must verify an outcome.
    /// # Errors
    /// Rejects malformed, unbounded, or outcome-free scenarios.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if s.name.trim().is_empty()
            || s.goal.trim().is_empty()
            || s.viewport.iter().any(|v| !(200..=8192).contains(v))
            || !s.scale.is_finite()
            || !(0.5..=4.0).contains(&s.scale)
            || !s.text_scale.is_finite()
            || !(1.0..=2.0).contains(&s.text_scale)
            || !matches!(s.theme.as_str(), "dark" | "light" | "high-contrast")
            || !matches!(s.fixture.as_str(), "desktop" | "files")
            || s.steps.len() > 1000
        {
            return Err("invalid name, goal, viewport, scale, theme, or step count".into());
        }
        if !matches!(s.steps.last(), Some(Step::Checkpoint { checks, .. }) if checks.iter().any(|c| matches!(c, Check::Visible {..} | Check::Resource {..} | Check::Semantic {..} | Check::Moved {..} | Check::ImageShift {..} | Check::State {..})))
        {
            return Err(
                "final checkpoint must assert a visible, semantic, resource, or movement outcome"
                    .into(),
            );
        }
        let mut names =
            std::collections::HashSet::from(["initial".to_string(), "input-failure".to_string()]);
        for step in &s.steps {
            match step {
                Step::WaitState {
                    timeout_ms,
                    pointer,
                    ..
                } => {
                    if *timeout_ms > 30_000 || !pointer.starts_with('/') {
                        return Err("invalid state wait or timeout over 30 seconds".into());
                    }
                }
                Step::Checkpoint { name, checks } => {
                    if name.is_empty()
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                        || !names.insert(name.clone())
                    {
                        return Err("checkpoint names must be unique safe filenames".into());
                    }
                    for check in checks {
                        checks::validate(check, s.viewport)?;
                    }
                }
                Step::Input { action } => {
                    let spec = crate::headless::parse_spec(
                        &serde_json::json!({"actions":[action]}).to_string(),
                    )?;
                    if spec.actions.len() != 1 {
                        return Err("one input per step required".into());
                    }
                    if let Some(crate::headless::Action::WaitForNamed { timeout_ms, .. }) =
                        spec.actions.first()
                        && *timeout_ms > 30_000
                    {
                        return Err("wait timeout exceeds 30 seconds".into());
                    }
                }
                Step::Wheel {
                    at, target, delta, ..
                } => {
                    if at.is_some() == target.is_some()
                        || target.as_ref().is_some_and(|s| s.trim().is_empty())
                    {
                        return Err("wheel requires exactly one coordinate or target".into());
                    }
                    if at
                        .iter()
                        .flatten()
                        .chain(delta)
                        .any(|v| !v.is_finite() || v.abs() > 100_000.0)
                    {
                        return Err("invalid wheel coordinates or delta".into());
                    }
                }
            }
        }
        Ok(s)
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Counts {
    pub clicks: u32,
    pub keys: u32,
    pub semantic_actions: u32,
    pub wheel_events: u32,
}
impl Counts {
    pub fn record(&mut self, action: &Value) {
        if action.get("click_named").is_some()
            || action.get("click_at").is_some()
            || action.get("tap_named").is_some()
            || action.get("tap_at").is_some()
        {
            self.clicks = self.clicks.saturating_add(1);
        }
        if action.get("key").is_some() {
            self.keys = self.keys.saturating_add(1);
        }
        if let Some(text) = action.get("type").and_then(Value::as_str) {
            self.keys = self
                .keys
                .saturating_add(u32::try_from(text.chars().count()).unwrap_or(u32::MAX));
        }
        if action.get("perform").is_some() {
            self.semantic_actions = self.semantic_actions.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests;
