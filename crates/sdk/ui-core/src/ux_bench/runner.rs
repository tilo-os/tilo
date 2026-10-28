use super::{
    Axis, Check, Counts, ImageRegion, Scenario, Step, WheelPhase, WheelUnit, gap, overlap_area,
    pixel_contrast,
};
use crate::headless::{FrameHarness, InspectionNode};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

use winit::{
    dpi::PhysicalPosition,
    event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent},
};

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub check: Value,
    pub passed: bool,
    pub observed: Value,
}
#[derive(Debug, Serialize)]
pub struct Frame {
    pub name: String,
    pub screenshot: String,
    pub targets: BTreeMap<String, [f64; 4]>,
    pub visible_targets: BTreeMap<String, [f64; 4]>,
    pub checks: Vec<Measurement>,
    pub state: Value,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub scenario: Value,
    pub passed: bool,
    pub counts: Counts,
    pub trace: Vec<Value>,
    pub frames: Vec<Frame>,
    pub failures: Vec<String>,
    pub coverage: Vec<String>,
}

fn semantic_targets(
    node: &crate::a11y::AccessNode,
    fallback: Option<[f64; 4]>,
    targets: &mut BTreeMap<String, Vec<[f64; 4]>>,
) {
    if node.state.hidden {
        return;
    }
    let rect = node
        .bounds
        .map(|r| [r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0])
        .or(fallback);
    if let Some(rect) = rect {
        for name in [&node.name, &node.semantic_key, &node.action_target]
            .into_iter()
            .flatten()
        {
            let values = targets.entry(format!("semantic:{name}")).or_default();
            if !values.contains(&rect) {
                values.push(rect);
            }
        }
    }
    for child in &node.children {
        semantic_targets(child, None, targets);
    }
}
fn collect(node: &InspectionNode, targets: &mut BTreeMap<String, Vec<[f64; 4]>>) {
    if node.semantics.as_ref().is_some_and(|s| s.state.hidden) {
        return;
    }
    if let Some(semantics) = &node.semantics {
        semantic_targets(semantics, Some(node.rect), targets);
    }
    for child in &node.children {
        collect(child, targets);
    }
}
fn remove_hidden(node: &InspectionNode, hidden: bool, targets: &mut BTreeMap<String, [f64; 4]>) {
    let hidden = hidden || node.semantics.as_ref().is_some_and(|s| s.state.hidden);
    if hidden {
        targets.remove(&node.path);
        if let Some(id) = &node.test_id {
            targets.remove(id);
        }
    }
    for child in &node.children {
        remove_hidden(child, hidden, targets);
    }
}

fn semantic_exists(node: &crate::a11y::AccessNode, name: &str) -> bool {
    !node.state.hidden
        && ([&node.name, &node.semantic_key, &node.action_target]
            .into_iter()
            .flatten()
            .any(|n| n == name)
            || node
                .children
                .iter()
                .any(|child| semantic_exists(child, name)))
}

/// Whether `node` answers to `name`, with or without the `semantic:` prefix
/// a journey writes to say it means a semantic name rather than a path.
fn names_node(node: &crate::a11y::AccessNode, name: &str) -> bool {
    semantic_exists(node, name.strip_prefix("semantic:").unwrap_or(name))
}

fn semantic_node(driver: &FrameHarness, target: &str) -> Result<crate::a11y::AccessNode, String> {
    fn access(
        node: &crate::a11y::AccessNode,
        target: &str,
        found: &mut Vec<crate::a11y::AccessNode>,
    ) {
        if node.state.hidden {
            return;
        }
        if [&node.name, &node.semantic_key, &node.action_target]
            .into_iter()
            .flatten()
            .any(|name| name == target)
        {
            found.push(node.clone());
        }
        for child in &node.children {
            access(child, target, found);
        }
    }
    fn walk(node: &InspectionNode, target: &str, found: &mut Vec<crate::a11y::AccessNode>) {
        if node
            .semantics
            .as_ref()
            .is_some_and(|node| node.state.hidden)
        {
            return;
        }
        if let Some(node) = &node.semantics {
            access(node, target, found);
        }
        for child in &node.children {
            walk(child, target, found);
        }
    }
    let mut found = vec![];
    walk(
        &driver.inspection().root,
        target.strip_prefix("semantic:").unwrap_or(target),
        &mut found,
    );
    if found.len() != 1 {
        return Err(format!("missing or ambiguous semantic target: {target}"));
    }
    found.pop().ok_or_else(|| "missing semantic target".into())
}

fn target_exists(node: &InspectionNode, name: &str) -> bool {
    if node.semantics.as_ref().is_some_and(|s| s.state.hidden) {
        return false;
    }
    if let Some(semantic) = name.strip_prefix("semantic:") {
        if node
            .semantics
            .as_ref()
            .is_some_and(|s| semantic_exists(s, semantic))
        {
            return true;
        }
    } else if node.path == name || node.test_id.as_deref() == Some(name) {
        return true;
    }
    node.children.iter().any(|child| target_exists(child, name))
}

/// Whether `target` holds the keyboard focus, with the evidence behind the
/// answer.
///
/// A surface can keep drawing the same values at every width while the
/// keyboard stops reaching a control, so this asks the inspection rather than
/// the pixels: the target's own widget is the runner's focus owner, or the
/// target names a widget or semantic control inside that owner which reports
/// it is focused.
fn focused_target(driver: &FrameHarness, target: &str) -> (bool, Value) {
    fn node_is_focused(node: &crate::a11y::AccessNode) -> bool {
        !node.state.hidden && (node.state.focused || node.children.iter().any(node_is_focused))
    }
    fn holds_focus(node: &InspectionNode, name: &str) -> bool {
        if node.semantics.as_ref().is_some_and(|s| s.state.hidden) {
            return false;
        }
        let named = node.semantics.as_ref().is_some_and(|s| names_node(s, name));
        if named && node.semantics.as_ref().is_some_and(node_is_focused) {
            return true;
        }
        node.children.iter().any(|child| holds_focus(child, name))
    }
    fn inside<'a>(node: &'a InspectionNode, path: &str) -> Option<&'a InspectionNode> {
        if node.path == path {
            return Some(node);
        }
        node.children.iter().find_map(|child| inside(child, path))
    }
    let inspection = driver.inspection();
    let owner = inspection.keyboard_focus_owner.as_deref();
    let focused = owner.is_some_and(|path| {
        inside(&inspection.root, path).is_some_and(|owner| {
            // The widget holding the runner's focus is the target however it
            // is titled: the keyboard is there, whatever the control publishes
            // about its own state.
            owner.path == target
                || owner.test_id.as_deref() == Some(target)
                || owner
                    .semantics
                    .as_ref()
                    .is_some_and(|semantics| names_node(semantics, target))
                || holds_focus(owner, target)
        })
    });
    (
        focused,
        json!({"target": target, "focused": focused, "focus_owner": owner}),
    )
}

fn targets(driver: &FrameHarness) -> BTreeMap<String, [f64; 4]> {
    let mut all = BTreeMap::new();
    let inspection = driver.inspection();
    collect(&inspection.root, &mut all);
    let mut unique: BTreeMap<_, _> = all
        .into_iter()
        .filter_map(|(k, v)| {
            if v.len() == 1 {
                v.first().map(|r| (k, *r))
            } else {
                None
            }
        })
        .collect();
    for (name, r) in driver.targets() {
        unique.insert(name, [r.x0, r.y0, r.width(), r.height()]);
    }
    remove_hidden(&inspection.root, false, &mut unique);
    unique
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    [
        x,
        y,
        ((a[0] + a[2]).min(b[0] + b[2]) - x).max(0.0),
        ((a[1] + a[3]).min(b[1] + b[3]) - y).max(0.0),
    ]
}

fn visible_targets(driver: &FrameHarness) -> BTreeMap<String, [f64; 4]> {
    fn semantics(
        node: &crate::a11y::AccessNode,
        fallback: Option<[f64; 4]>,
        clip: [f64; 4],
        out: &mut BTreeMap<String, [f64; 4]>,
    ) {
        if node.state.hidden {
            return;
        }
        if let Some(rect) = node
            .bounds
            .map(|r| [r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0])
            .or(fallback)
        {
            for name in [&node.name, &node.semantic_key, &node.action_target]
                .into_iter()
                .flatten()
            {
                out.insert(format!("semantic:{name}"), intersect(rect, clip));
            }
        }
        for child in &node.children {
            semantics(child, None, clip, out);
        }
    }
    fn walk(node: &InspectionNode, clip: [f64; 4], out: &mut BTreeMap<String, [f64; 4]>) {
        if node.semantics.as_ref().is_some_and(|s| s.state.hidden) {
            return;
        }
        out.insert(node.path.clone(), intersect(node.rect, clip));
        if let Some(id) = &node.test_id {
            out.insert(id.clone(), intersect(node.rect, clip));
        }
        if let Some(s) = &node.semantics {
            semantics(s, Some(node.rect), clip, out);
        }
        let child_clip = node.child_clip.map_or(clip, |c| intersect(c, clip));
        for child in &node.children {
            walk(child, child_clip, out);
        }
    }
    let snapshot = driver.inspection();
    let mut out = BTreeMap::new();
    walk(
        &snapshot.root,
        [
            0.0,
            0.0,
            f64::from(snapshot.viewport[0]),
            f64::from(snapshot.viewport[1]),
        ],
        &mut out,
    );
    out
}
fn resources(items: Vec<tilo_actions::Resource>) -> Value {
    Value::Array(items.into_iter().map(|r| json!({"id":r.id.0,"kind":r.kind,"label":r.label,
        "actions":r.actions.iter().map(|a| json!({"id":a.id,"label":a.label,"kind":a.kind})).collect::<Vec<_>>(),
        "children":resources(r.children)})).collect())
}
fn has_resource(v: &Value, id: &str) -> bool {
    v.as_array().is_some_and(|a| {
        a.iter().any(|r| {
            r.get("id").and_then(Value::as_str) == Some(id) || has_resource(&r["children"], id)
        })
    })
}
fn coordinate(r: [f64; 4], axis: Axis) -> f64 {
    match axis {
        Axis::X => r[0],
        Axis::Y => r[1],
    }
}

// Resolve once in screen space. Target movement invalidates stationary/scroll
// evidence instead of silently recropping a moved container in each image.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite nonnegative bounds checked before pixel conversion"
)]
fn image_region(
    region: &ImageRegion,
    current: &BTreeMap<String, [f64; 4]>,
    prior: &BTreeMap<String, [f64; 4]>,
) -> Result<[u32; 4], String> {
    match region {
        ImageRegion::Pixels(region) => Ok(*region),
        ImageRegion::Target { target, inset } => {
            let bounds = current
                .get(target)
                .ok_or_else(|| format!("missing or ambiguous image target {target}"))?;
            if prior.get(target) != Some(bounds) {
                return Err(format!("image target moved: {target}"));
            }
            let [x, y, w, h] = *bounds;
            let [left, top, right, bottom] = inset.map(f64::from);
            let values = [
                (x + left).ceil(),
                (y + top).ceil(),
                (w - left - right).floor(),
                (h - top - bottom).floor(),
            ];
            if values
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > f64::from(u32::MAX))
                || values[2] < 1.0
                || values[3] < 1.0
            {
                return Err("invalid target image region".into());
            }
            Ok(values.map(|v| v as u32))
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one declarative match keeps each measurement with its validation and evidence"
)]
fn measure(
    check: &Check,
    driver: &mut FrameHarness,
    current: &BTreeMap<String, [f64; 4]>,
    frames: &[Frame],
    png: &Path,
    viewport: [u32; 2],
    state: &Value,
) -> Result<(bool, Value), String> {
    let rect = |name: &str| {
        current
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing or ambiguous target: {name}"))
    };
    let ranged = |n: f64, min: f64, max: f64| (n.is_finite() && n >= min && n <= max, json!(n));
    match check {
        Check::SemanticValue { target, equals } => {
            let node = semantic_node(driver, target)?;
            Ok((
                node.value.as_ref() == Some(equals),
                json!({"value":node.value,"role":format!("{:?}",node.role)}),
            ))
        }
        Check::Selected { target } => {
            let node = semantic_node(driver, target)?;
            Ok((node.state.selected, json!({"selected":node.state.selected})))
        }
        Check::ImageShift {
            from,
            region,
            axis,
            min,
            max,
        } => {
            let prior = frames
                .iter()
                .find(|f| &f.name == from)
                .ok_or_else(|| format!("missing prior checkpoint {from}"))?;
            let region = image_region(region, current, &prior.targets)?;
            let parent = png.parent().ok_or("capture has no parent directory")?;
            let before = image::open(parent.join(&prior.screenshot))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let after = image::open(png).map_err(|e| e.to_string())?.to_rgba8();
            let (shift, error, unshifted_error) =
                super::checks::image_shift(&before, &after, region, *axis)?;
            Ok((
                f64::from(shift) >= *min && f64::from(shift) <= *max,
                json!({"displacement":shift,"matching_error":error,"unshifted_error":unshifted_error,"region":region,"method":"rendered region translation estimate"}),
            ))
        }
        Check::ImageUnchanged { from, region } => {
            let prior = frames
                .iter()
                .find(|f| &f.name == from)
                .ok_or("missing prior checkpoint")?;
            let region = image_region(region, current, &prior.targets)?;
            let parent = png.parent().ok_or("capture has no parent")?;
            let before = image::open(parent.join(&prior.screenshot))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let after = image::open(png).map_err(|e| e.to_string())?.to_rgba8();
            for capture in [&before, &after] {
                if region[0]
                    .checked_add(region[2])
                    .is_none_or(|v| v > capture.width())
                    || region[1]
                        .checked_add(region[3])
                        .is_none_or(|v| v > capture.height())
                {
                    return Err("stationary region outside actual capture".into());
                }
            }
            let first = before
                .get_pixel_checked(region[0], region[1])
                .ok_or("empty image region")?;
            let has_detail = (region[1]..region[1].saturating_add(region[3])).any(|y| {
                (region[0]..region[0].saturating_add(region[2]))
                    .any(|x| before.get_pixel_checked(x, y) != Some(first))
            });
            if !has_detail {
                return Err("blank stationary region contains no content evidence".into());
            }
            let unchanged = (region[1]..region[1].saturating_add(region[3])).all(|y| {
                (region[0]..region[0].saturating_add(region[2]))
                    .all(|x| before.get_pixel_checked(x, y) == after.get_pixel_checked(x, y))
            });
            Ok((unchanged, json!({"unchanged":unchanged,"region":region})))
        }
        Check::State { pointer, equals } => {
            let observed = state
                .pointer(pointer)
                .ok_or_else(|| format!("adapter state missing {pointer}"))?;
            Ok((observed == equals, observed.clone()))
        }
        Check::Visible { target } => {
            let r = rect(target)?;
            let visible = visible_targets(driver);
            let clipped = visible
                .get(target)
                .ok_or_else(|| format!("no visible bounds for {target}"))?;
            Ok((
                r[2] > 0.0
                    && r[3] > 0.0
                    && clipped[2] >= r[2] - 0.5
                    && clipped[3] >= r[3] - 0.5
                    && r[0] >= -0.5
                    && r[1] >= -0.5
                    && r[0] + r[2] <= f64::from(viewport[0]) + 0.5
                    && r[1] + r[3] <= f64::from(viewport[1]) + 0.5,
                json!({"bounds":r,"visible_bounds":clipped}),
            ))
        }
        Check::Absent { target } => {
            let exists = target_exists(&driver.inspection().root, target);
            Ok((!exists, json!({"exists": exists})))
        }
        Check::Gap {
            first,
            second,
            axis,
            min,
            max,
        } => Ok(ranged(gap(rect(first)?, rect(second)?, *axis), *min, *max)),
        Check::Aligned {
            first,
            second,
            axis,
            tolerance,
        } => Ok(ranged(
            (coordinate(rect(first)?, *axis) - coordinate(rect(second)?, *axis)).abs(),
            0.0,
            *tolerance,
        )),
        Check::NoOverlap { first, second } => {
            Ok(ranged(overlap_area(rect(first)?, rect(second)?), 0.0, 0.0))
        }
        Check::TargetSize { target, min } => {
            rect(target)?;
            let visible = visible_targets(driver);
            let r = visible
                .get(target)
                .ok_or_else(|| format!("no visible bounds for {target}"))?;
            Ok((r[2] >= min[0] && r[3] >= min[1], json!([r[2], r[3]])))
        }
        Check::Focus { target } => {
            let (focused, evidence) = focused_target(driver, target);
            Ok((focused, evidence))
        }
        Check::Moved {
            target,
            from,
            axis,
            min,
            max,
        } => {
            let old = frames
                .iter()
                .find(|f| &f.name == from)
                .and_then(|f| f.targets.get(target))
                .ok_or_else(|| format!("missing prior checkpoint/target: {from}/{target}"))?;
            Ok(ranged(
                coordinate(rect(target)?, *axis) - coordinate(*old, *axis),
                *min,
                *max,
            ))
        }
        Check::Contrast {
            foreground,
            background,
            min,
            ..
        } => {
            let image = image::open(png).map_err(|e| e.to_string())?.to_rgba8();
            let a = image
                .get_pixel_checked(foreground[0], foreground[1])
                .ok_or("foreground outside capture")?
                .0;
            let b = image
                .get_pixel_checked(background[0], background[1])
                .ok_or("background outside capture")?
                .0;
            let ratio = pixel_contrast(a, b)?;
            Ok((
                ratio >= *min,
                json!({"ratio":ratio,"foreground_rgba":a,"background_rgba":b,"method":"explicit rendered pixel pair"}),
            ))
        }
        Check::Resource { id } => {
            let r = resources(driver.resources());
            Ok((has_resource(&r, id), r))
        }
        Check::Semantic { expectation } => {
            let spec = crate::headless::parse_spec(
                &json!({"actions":[{"expect":expectation}]}).to_string(),
            )?;
            for action in spec.actions {
                driver.perform(&action)?;
            }
            Ok((true, json!("semantic outcome verified")))
        }
    }
}

/// Execute a scenario, retaining failure evidence and never treating input dispatch as completion.
pub fn run(driver: &mut FrameHarness, scenario: &Scenario, out: &Path) -> Result<Report, String> {
    run_with_state(driver, scenario, out, || Value::Null)
}

/// Run with a read-only adapter state snapshot for product outcome assertions.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "ordered scenario lifecycle; validated finite wheel line values convert to winit f32 units"
)]
pub fn run_with_state(
    driver: &mut FrameHarness,
    scenario: &Scenario,
    out: &Path,
    mut state: impl FnMut() -> Value,
) -> Result<Report, String> {
    Scenario::parse(&serde_json::to_string(scenario).map_err(|e| e.to_string())?)?;
    if driver.inspection().viewport != scenario.viewport {
        return Err("driver viewport differs from scenario viewport".into());
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    driver.set_input_scale(scenario.scale)?;
    let mut report = Report { schema_version:1, scenario:serde_json::to_value(scenario).map_err(|e| e.to_string())?, passed:false,
        counts:Counts::default(), trace:vec![], frames:vec![], failures:vec![], coverage:vec![
            "Real retained shell/widget input path; isolated fixture state".into(),
            "Layout and screenshots in logical pixels; physical wheel input uses declared scale".into(),
            "Visibility checks declared clipping and hidden state, not sibling-overlay occlusion".into(),
            "Contrast covers explicit rendered pixel pairs only; no whole-screen certification".into(),
            "Hardware delivery, native gesture arbitration, perceived ease and animation smoothness require live acceptance".into()] };
    capture(
        driver,
        "initial",
        &[],
        scenario,
        out,
        &mut report,
        &mut state,
    )?;
    for (index, step) in scenario.steps.iter().enumerate() {
        let start = crate::time::now();
        let result = match step {
            Step::WaitState {
                pointer,
                equals,
                timeout_ms,
            } => {
                if driver.wait_until(crate::time::Duration::from_millis(*timeout_ms), |_| {
                    state().pointer(pointer) == Some(equals)
                }) {
                    Ok(())
                } else {
                    Err(format!(
                        "state wait timed out: {pointer} expected {equals}, observed {}",
                        state()
                    ))
                }
            }
            Step::Input { action } => {
                report.counts.record(action);
                perform_input(driver, action)
            }
            Step::Wheel {
                at,
                target,
                delta,
                unit,
                phase,
            } => (|| {
                let at = if let Some(target) = target {
                    if !targets(driver).contains_key(target) {
                        return Err(format!("missing or ambiguous wheel target {target}"));
                    }
                    let all = visible_targets(driver);
                    let r = all
                        .get(target)
                        .ok_or_else(|| format!("missing wheel target {target}"))?;
                    if r[2] <= 0.0 || r[3] <= 0.0 {
                        return Err(format!("empty wheel target {target}"));
                    }
                    [r[0] + r[2] / 2.0, r[1] + r[3] / 2.0]
                } else {
                    at.ok_or("missing wheel position")?
                };
                report.counts.wheel_events = report.counts.wheel_events.saturating_add(1);
                driver.send(&crate::synthetic_input::cursor_moved(
                    at[0] * scenario.scale,
                    at[1] * scenario.scale,
                ));
                let delta = match unit {
                    WheelUnit::Lines => {
                        MouseScrollDelta::LineDelta(delta[0] as f32, delta[1] as f32)
                    }
                    WheelUnit::PhysicalPixels => {
                        MouseScrollDelta::PixelDelta(PhysicalPosition::new(delta[0], delta[1]))
                    }
                };
                let phase = match phase {
                    WheelPhase::Started => TouchPhase::Started,
                    WheelPhase::Moved => TouchPhase::Moved,
                    WheelPhase::Ended => TouchPhase::Ended,
                    WheelPhase::Cancelled => TouchPhase::Cancelled,
                };
                driver.send(&WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta,
                    phase,
                });
                Ok(())
            })(),
            Step::Checkpoint { name, checks } => {
                capture(driver, name, checks, scenario, out, &mut report, &mut state)
            }
        };
        report.trace.push(json!({"step":index,"input":step,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"error":result.as_ref().err()}));
        if let Err(error) = result {
            report.failures.push(format!("step {index}: {error}"));
            capture(
                driver,
                "input-failure",
                &[],
                scenario,
                out,
                &mut report,
                &mut state,
            )?;
            break;
        }
    }
    if report.counts.clicks > scenario.budget.clicks {
        report.failures.push(format!(
            "click budget exceeded: {} > {}",
            report.counts.clicks, scenario.budget.clicks
        ));
    }
    if report.counts.keys > scenario.budget.keys {
        report.failures.push(format!(
            "key budget exceeded: {} > {}",
            report.counts.keys, scenario.budget.keys
        ));
    }
    if report.counts.semantic_actions > 0 {
        report
            .failures
            .push("semantic invocation bypassed visible route; cannot certify click budget".into());
    }
    report.passed = report.failures.is_empty();
    write_json(&out.join("report.json"), &report)?;
    let data = serde_json::to_string(&report)
        .map_err(|e| e.to_string())?
        .replace('<', "\\u003c");
    std::fs::write(
        out.join("index.html"),
        include_str!("report.html").replace("REPORT_DATA", &data),
    )
    .map_err(|e| e.to_string())?;
    Ok(report)
}
fn perform_input(driver: &mut FrameHarness, action: &Value) -> Result<(), String> {
    // Semantic identities locate the real hit target; pointer dispatch remains a click.
    if let Some(name) = action.get("click_named").and_then(Value::as_str) {
        let all = targets(driver);
        let r = all
            .get(name)
            .ok_or_else(|| format!("missing or ambiguous target: {name}"))?;
        if r[2] <= 0.0 || r[3] <= 0.0 {
            return Err(format!("empty target: {name}"));
        }
        let visible = visible_targets(driver);
        let v = visible
            .get(name)
            .ok_or_else(|| format!("target visibility unavailable: {name}"))?;
        let center = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        if v[2] <= 0.0
            || v[3] <= 0.0
            || center.0 < v[0]
            || center.1 < v[1]
            || center.0 > v[0] + v[2]
            || center.1 > v[1] + v[3]
        {
            return Err(format!("target center is clipped: {name}"));
        }
        driver.click_at(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        return Ok(());
    }
    let spec = crate::headless::parse_spec(&json!({"actions":[action]}).to_string())?;
    for input in spec.actions {
        driver.perform(&input)?;
    }
    Ok(())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn capture(
    driver: &mut FrameHarness,
    name: &str,
    checks: &[Check],
    scenario: &Scenario,
    out: &Path,
    report: &mut Report,
    state: &mut impl FnMut() -> Value,
) -> Result<(), String> {
    // Expectations may pump asynchronous state. Settle them before taking the
    // coherent geometry/pixel snapshot used by the remaining measurements.
    let semantic_results: Vec<_> = checks
        .iter()
        .map(|check| {
            if matches!(check, Check::Semantic { .. }) {
                Some(measure(
                    check,
                    driver,
                    &BTreeMap::new(),
                    &[],
                    Path::new(""),
                    scenario.viewport,
                    &Value::Null,
                ))
            } else {
                None
            }
        })
        .collect();
    driver.relayout();
    let screenshot = format!("{name}.png");
    let png = out.join(&screenshot);
    driver.render_png(&png);
    let current = targets(driver);
    let state = state();
    write_json(
        &out.join(format!("{name}.inspection.json")),
        &driver.inspection(),
    )?;
    write_json(
        &out.join(format!("{name}.resources.json")),
        &resources(driver.resources()),
    )?;
    let mut measured = vec![];
    for (check, settled) in checks.iter().zip(semantic_results) {
        let (passed, observed) = match settled.unwrap_or_else(|| {
            measure(
                check,
                driver,
                &current,
                &report.frames,
                &png,
                scenario.viewport,
                &state,
            )
        }) {
            Ok(result) => result,
            Err(e) => (false, json!({"error":e})),
        };
        if !passed {
            report
                .failures
                .push(format!("{name}: {check:?}: {observed}"));
        }
        measured.push(Measurement {
            check: serde_json::to_value(check).map_err(|e| e.to_string())?,
            passed,
            observed,
        });
    }
    report.frames.push(Frame {
        name: name.into(),
        screenshot,
        targets: current,
        visible_targets: visible_targets(driver),
        checks: measured,
        state,
    });
    Ok(())
}

#[cfg(test)]
mod presence_tests {
    #[test]
    fn semantic_presence_does_not_require_unique_bounds() {
        let mut root = crate::a11y::AccessNode::new(crate::a11y::Role::Group);
        let mut child = crate::a11y::AccessNode::new(crate::a11y::Role::Button);
        child.name = Some("duplicate".into());
        root.children = vec![child.clone(), child];
        assert!(super::semantic_exists(&root, "duplicate"));
        root.state.hidden = true;
        assert!(!super::semantic_exists(&root, "duplicate"));
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    #[test]
    fn target_regions_reject_missing_moved_empty_and_out_of_bounds_evidence() {
        let region = ImageRegion::Target {
            target: "list".into(),
            inset: [2, 3, 4, 5],
        };
        let bounds = BTreeMap::from([("list".into(), [10.0, 20.0, 100.0, 200.0])]);
        assert_eq!(
            image_region(&region, &bounds, &bounds).unwrap(),
            [12, 23, 94, 192]
        );
        assert!(image_region(&region, &BTreeMap::new(), &bounds).is_err());
        let moved = BTreeMap::from([("list".into(), [11.0, 20.0, 100.0, 200.0])]);
        assert!(image_region(&region, &moved, &bounds).is_err());
        for r in [
            [0.0, 0.0, 2.0, 2.0],
            [-100.0, 0.0, 100.0, 100.0],
            [f64::NAN, 0.0, 100.0, 100.0],
        ] {
            let bad = BTreeMap::from([("list".into(), r)]);
            assert!(image_region(&region, &bad, &bad).is_err());
        }
    }
}
