//! What tests of change marks read of a window: the marks it shows, their pixels, and the
//! door the run goes through.

use ui::framework::testing::HostedApp;
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

/// The window's change marks: the control in its header naming who changed what it shows ("Helper
/// (agent) changed 2 notes"), with the rect it takes. A window's mark is a button led by the
/// faces of everyone who changed what it shows, its label saying what they changed and how many.
pub(crate) fn marks(app: &mut HostedApp) -> Vec<(String, [f64; 4])> {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.into_iter()
        .filter(|node| node.widget == "Button" || node.widget == "Chip")
        .filter_map(|node| {
            let name = node.semantics.as_ref()?.name.clone()?;
            let says_a_change = [" changed ", " made ", " removed "]
                .iter()
                .any(|said| name.contains(said));
            says_a_change.then_some((name, node.rect))
        })
        .collect()
}

/// The list's rows: each one's title, rect and whether it carries a change mark (what it says
/// to assistive technology names who changed it).
pub(crate) fn rows(app: &mut HostedApp) -> Vec<(String, [f64; 4], bool)> {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.into_iter()
        .filter(|node| node.widget == "SwipeRow" || node.widget == "ListRow")
        .filter_map(|node| {
            let semantics = node.semantics.as_ref()?;
            let name = semantics.name.clone()?;
            let marked = semantics.value.as_deref().is_some_and(|value| {
                ["Made by", "Changed by", "Removed by"]
                    .iter()
                    .any(|said| value.contains(said))
            });
            Some((name, node.rect, marked))
        })
        .collect()
}

/// Chooses the sidebar's place `label`, as a person does: a row a change takes out of the
/// place it stood in is read where it went.
pub(crate) fn choose(app: &mut HostedApp, label: &str) {
    fn place<'a>(node: &'a ui::a11y::AccessNode, label: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.role == ui::a11y::Role::TreeItem
            && node
                .name
                .as_deref()
                .is_some_and(|name| name.starts_with(label))
        {
            return Some(node);
        }
        node.children.iter().find_map(|child| place(child, label))
    }
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    let bounds = all
        .iter()
        .filter_map(|node| node.semantics.as_ref())
        .find_map(|access| place(access, label))
        .and_then(|access| access.bounds)
        .unwrap_or_else(|| panic!("the sidebar offers {label}"));
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    settle(app);
}

/// A reference to the published resource of `app` labelled `label`, as an action's argument
/// names one.
pub(crate) fn reference(app_id: &str, app: &mut HostedApp, label: &str) -> serde_json::Value {
    fn flattened(
        resources: &[ui::tilo_actions::ResourceSpec],
    ) -> Vec<&ui::tilo_actions::ResourceSpec> {
        resources
            .iter()
            .flat_map(|r| std::iter::once(r).chain(flattened(&r.children)))
            .collect()
    }
    let resources = app.harness().resources();
    let prefix = format!("tl:{app_id}:");
    let found = flattened(&resources)
        .into_iter()
        .find(|r| r.label == label && r.id.as_str().starts_with(&prefix))
        .unwrap_or_else(|| panic!("nothing of {app_id} is labelled {label}"));
    serde_json::json!({
        "app": app_id,
        "kind": found.kind,
        "id": found.id.as_str().trim_start_matches(&prefix),
        "label": "",
    })
}

/// The rows standing in for removed items: their titles.
pub(crate) fn removed_rows(app: &mut HostedApp) -> Vec<String> {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.into_iter()
        .filter(|node| node.widget == "SwipeRow" || node.widget == "ListRow")
        .filter_map(|node| {
            let semantics = node.semantics.as_ref()?;
            semantics
                .value
                .as_deref()
                .is_some_and(|value| value.contains("Removed by"))
                .then(|| semantics.name.clone().unwrap_or_default())
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
        .map_or_else(
            || panic!("{resource} offers no `{label}`"),
            |action| action.id.clone(),
        )
}

/// How far the pixels of `rect` stray from grey at most: a mark in someone's colour does,
/// the neutral window around it does not.
pub(crate) fn chroma_in(app: &mut HostedApp, rect: [f64; 4]) -> u8 {
    let pixels = app.harness().render_rgba();
    let snapshot = app.harness().inspection();
    let [width, _] = snapshot.viewport;
    let scale = f64::from(width) / snapshot.root.rect[2].max(1.0);
    // Device pixels: whole, and never negative.
    let device = |v: f64| {
        usize::try_from(
            format!("{:.0}", (v * scale).max(0.0))
                .parse::<u64>()
                .unwrap(),
        )
        .unwrap()
    };
    let [x, y, w, h] = rect.map(device);
    let width = usize::try_from(width).unwrap();
    let mut most = 0u8;
    for py in y..y.saturating_add(h) {
        for px in x..x.saturating_add(w) {
            let at = py
                .saturating_mul(width)
                .saturating_add(px)
                .saturating_mul(4);
            if let Some(&[r, g, b, _]) = pixels
                .get(at..at.saturating_add(4))
                .and_then(|p| p.first_chunk::<4>())
            {
                most = most.max(r.max(g).max(b).saturating_sub(r.min(g).min(b)));
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
        .find(|r| {
            r.id.as_str().starts_with(under)
                && r.id.as_str() != under
                && r.actions.iter().any(|a| a.label == label)
        })
        .map_or_else(
            || {
                let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
                panic!("nothing under {under} offers `{label}`: {ids:?}")
            },
            |r| r.id.as_str().to_owned(),
        )
}

/// The rect of the `widget` whose name begins with `name`.
pub(crate) fn named(app: &mut HostedApp, widget: &str, name: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
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
        .map_or_else(
            || panic!("no {widget} named {name}: {names:?}"),
            |node| node.rect,
        )
}

/// The rect of the text field holding `text`.
pub(crate) fn text_field(app: &mut HostedApp, text: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.into_iter()
        .find(|node| {
            node.widget == "TextInput"
                && node
                    .semantics
                    .as_ref()
                    .and_then(|s| s.value.as_deref())
                    .is_some_and(|value| value == text)
        })
        .map_or_else(|| panic!("no field holds {text}"), |node| node.rect)
}
