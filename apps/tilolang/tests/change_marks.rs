//! What tests of change marks read of a window: the marks it shows, their pixels, and the
//! door the run goes through.

use ui::framework::testing::HostedApp;
use ui::headless::InspectionNode;
use ui::tilo_actions::{ActionContext, ActionInvoker, ActorKind};

pub(crate) const RUN: &str = "issue-7@8000000000000001";

/// The agent's run of issue 7, acting for `ada`, as the broker stamps it.
pub(crate) fn run() -> (ActionInvoker, ActionContext) {
    (
        ActionInvoker::actor("agent:helper")
            .with_kind(ActorKind::Agent)
            .acting_for("ada"),
        ActionContext::default().with_group(
            RUN,
            "Tidy the notes",
            ui::tilo_actions::context::TASK_RUN_GROUP,
        ),
    )
}

pub(crate) fn settle(app: &mut HostedApp) {
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

pub(crate) fn walk<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

/// The change marks the window shows: each chip that offers to show what someone did, with
/// its rect.
pub(crate) fn marks(app: &mut HostedApp) -> Vec<(String, [f64; 4])> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "Chip")
        .filter_map(|node| {
            let name = node.semantics.as_ref()?.name.clone()?;
            name.starts_with("Show what").then_some((name, node.rect))
        })
        .collect()
}

/// The id of the action labelled `label` the published resource `resource` offers.
pub(crate) fn action_id(app: &mut HostedApp, resource: &str, label: &str) -> String {
    fn flattened(
        resources: &[ui::tilo_actions::ResourceSpec],
    ) -> Vec<&ui::tilo_actions::ResourceSpec> {
        resources
            .iter()
            .flat_map(|r| std::iter::once(r).chain(flattened(&r.children)))
            .collect()
    }
    let resources = app.harness().resources();
    flattened(&resources)
        .into_iter()
        .find(|published| published.id.as_str() == resource)
        .and_then(|published| published.actions.iter().find(|a| a.label == label))
        .map(|action| action.id.clone())
        .unwrap_or_else(|| panic!("{resource} offers no `{label}`"))
}

/// How far the pixels of `rect` stray from grey at most: a mark in someone's colour does,
/// the neutral window around it does not.
pub(crate) fn chroma_in(app: &mut HostedApp, rect: [f64; 4]) -> u8 {
    let pixels = app.harness().render_rgba();
    let snapshot = app.harness().inspection();
    let [width, _] = snapshot.viewport;
    let scale = f64::from(width) / snapshot.root.rect[2].max(1.0);
    let [x, y, w, h] = rect.map(|v| (v * scale).round() as i64);
    let mut most = 0u8;
    for py in y.max(0)..(y + h) {
        for px in x.max(0)..(x + w) {
            let at = usize::try_from((py * i64::from(width) + px) * 4).unwrap_or(usize::MAX);
            if let Some([r, g, b, _]) = pixels.get(at..at + 4).map(|p| [p[0], p[1], p[2], p[3]]) {
                most = most.max(r.max(g).max(b) - r.min(g).min(b));
            }
        }
    }
    most
}

/// Where the shots go, when asked for: a relative directory is under the build's target
/// directory, where a remote builder can hand it back.
pub(crate) fn shots_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("TILO_LEGIBILITY_SHOTS")?);
    let dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(target) if dir.is_relative() => std::path::PathBuf::from(target).join(dir),
        _ => dir,
    };
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

/// The first resource under `under` that offers the action labelled `label`: an item.
pub(crate) fn published_with(app: &mut HostedApp, under: &str, label: &str) -> String {
    fn flattened(
        resources: &[ui::tilo_actions::ResourceSpec],
    ) -> Vec<&ui::tilo_actions::ResourceSpec> {
        resources
            .iter()
            .flat_map(|r| std::iter::once(r).chain(flattened(&r.children)))
            .collect()
    }
    let resources = app.harness().resources();
    let all = flattened(&resources);
    all.iter()
        .find(|r| r.id.as_str().starts_with(under) && r.actions.iter().any(|a| a.label == label))
        .map(|r| r.id.as_str().to_owned())
        .unwrap_or_else(|| {
            let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
            panic!("nothing under {under} offers `{label}`: {ids:?}")
        })
}

/// The rect of the `widget` whose name begins with `name`.
pub(crate) fn named(app: &mut HostedApp, widget: &str, name: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let names: Vec<String> = all
        .iter()
        .filter_map(|node| {
            let name = node.semantics.as_ref()?.name.clone()?;
            Some(format!("{}:{name}", node.widget))
        })
        .collect();
    all.into_iter()
        .find(|node| {
            node.widget == widget
                && node
                    .semantics
                    .as_ref()
                    .and_then(|s| s.name.as_deref())
                    .is_some_and(|named| named.starts_with(name))
        })
        .map(|node| node.rect)
        .unwrap_or_else(|| panic!("no {widget} named {name}: {names:?}"))
}

/// The rect of the text field holding `text`.
pub(crate) fn text_field(app: &mut HostedApp, text: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .find(|node| {
            node.widget == "TextInput"
                && node
                    .semantics
                    .as_ref()
                    .and_then(|s| s.value.as_deref())
                    .is_some_and(|value| value == text)
        })
        .map(|node| node.rect)
        .unwrap_or_else(|| panic!("no field holds {text}"))
}
