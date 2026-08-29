//! Permissions & Roles pane — a read-only view of the **real** per-workspace
//! capability grants the compositor persists.
//!
//! The compositor writes every workspace's granted capabilities to
//! `~/.config/qos/state.json` (its `WorkspaceTree`); the FUSE mount and the
//! launcher sandbox enforce exactly that list. This pane reads that same file
//! and renders the live grants — which app in which workspace holds camera,
//! microphone, screen-sharing, file, or network access, and for how long —
//! rather than a hand-mirrored description that could drift from what is
//! actually enforced.
//!
//! It is deliberately **read-only**. Granting happens at the compositor's
//! consent prompt (an app reaching outside what it owns) and revoking happens
//! in the workspace that owns the grant; this pane does not write
//! `state.json`, so it ships no buttons that would look live but change
//! nothing. When there are no grants — a fresh session, or one where no app
//! has been approved for anything — it shows an honest empty state, never a
//! fabricated permission.

use crate::widgets::{El, caption, card, cards, hgap, rows, status_pill, vgap};
use collab_types::caps::{CapOverride, Effect, Expiry, Principal, Scope, Subject, Verb};
use serde::Deserialize;
use std::path::PathBuf;
use ui::widget::{Row, Spacer, Text, Widget};
use vello::peniko::Color;

/// The compositor's persisted workspace tree, beside `settings.json`.
fn state_path() -> Option<PathBuf> {
    settings_store::config_root().map(|r| r.join("state.json"))
}

fn load_text() -> Option<String> {
    std::fs::read_to_string(state_path()?).ok()
}

/// Minimal projection of `state.json`: only the fields this pane reads. Serde
/// ignores every other workspace field, and each `capabilities` entry is the
/// real [`CapOverride`] the compositor serialised, so the parse tracks the
/// enforced shape rather than a copy of it.
#[derive(Deserialize, Default)]
struct TreeView {
    #[serde(default)]
    workspaces: Vec<WsView>,
}

#[derive(Deserialize, Default)]
struct WsView {
    #[serde(default)]
    name: String,
    #[serde(default)]
    capabilities: Vec<CapOverride>,
}

/// Which tint a capability reads in — kept separate from the rendered `Color`
/// so the projection is testable without a `Theme`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tint {
    Files,
    Camera,
    Microphone,
    Screen,
    Remote,
    Network,
}

impl Tint {
    fn color(self, c: &ui::tokens::colors::Colors) -> Color {
        match self {
            Tint::Files => c.semantic.info,
            Tint::Camera | Tint::Microphone => c.semantic.warning,
            Tint::Screen => c.accent[5],
            Tint::Remote => c.semantic.danger,
            Tint::Network => c.semantic.success,
        }
    }
}

/// One capability an app holds, projected into display terms.
struct CapView {
    kind: &'static str,
    detail: String,
    expiry: &'static str,
    tint: Tint,
}

/// One app's grants within a workspace.
struct AppGrants {
    app: String,
    caps: Vec<CapView>,
}

/// One workspace and the app grants it holds.
struct WsGrants {
    name: String,
    apps: Vec<AppGrants>,
}

/// Human-readable expiry, mirroring the compositor prompt's three lifetimes.
fn expiry_label(e: Expiry) -> &'static str {
    match e {
        Expiry::Never => "Always",
        Expiry::AtUnix(_) => "Temporary",
        Expiry::EndOfSession => "Until log out",
        Expiry::AfterUse => "Once",
    }
}

/// Project one stored capability into a display capability, or `None` when it
/// is not an app portal grant this pane renders (role/room/action/resource
/// overrides and restrictions are enforcement plumbing, not app permissions).
fn cap_view(cap: &CapOverride) -> Option<CapView> {
    if cap.effect != Effect::Grant {
        return None;
    }
    let expiry = expiry_label(cap.expiry);
    let (kind, detail, tint) = match &cap.scope {
        Scope::FsPath(p) => {
            let verb = if cap.verb.implies(Verb::Write) {
                "read & write"
            } else {
                "read-only"
            };
            ("Files", format!("{p}  ·  {verb}"), Tint::Files)
        }
        Scope::Device(d) if d == "camera" => ("Camera", "Capture video".to_string(), Tint::Camera),
        Scope::Device(d) if d == "microphone" => {
            ("Microphone", "Capture audio".to_string(), Tint::Microphone)
        }
        Scope::Device(d) if d == "screencast" => (
            "Screen sharing",
            "Stream this workspace".to_string(),
            Tint::Screen,
        ),
        Scope::Device(d) if d == "remote-desktop" => (
            "Remote control",
            "Inject pointer & keyboard".to_string(),
            Tint::Remote,
        ),
        Scope::NetHost(h) => {
            let detail = if h == "*" {
                "Any host".to_string()
            } else {
                h.clone()
            };
            ("Network", detail, Tint::Network)
        }
        _ => return None,
    };
    Some(CapView {
        kind,
        detail,
        expiry,
        tint,
    })
}

/// Project the whole state document into per-workspace, per-app grants,
/// keeping only workspaces that actually hold app grants. Stable ordering
/// (workspaces as stored; apps and their caps sorted) so the pane is
/// deterministic.
fn project(state_json: &str) -> Vec<WsGrants> {
    let tree: TreeView = match serde_json::from_str(state_json) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for ws in tree.workspaces {
        // Bucket this workspace's app grants by app id.
        let mut by_app: std::collections::BTreeMap<String, Vec<CapView>> =
            std::collections::BTreeMap::new();
        for cap in &ws.capabilities {
            let Subject::Principal(Principal::App(app)) = &cap.subject else {
                continue;
            };
            if let Some(view) = cap_view(cap) {
                by_app.entry(app.clone()).or_default().push(view);
            }
        }
        if by_app.is_empty() {
            continue;
        }
        let apps = by_app
            .into_iter()
            .map(|(app, mut caps)| {
                caps.sort_by(|a, b| a.kind.cmp(b.kind).then(a.detail.cmp(&b.detail)));
                AppGrants { app, caps }
            })
            .collect();
        out.push(WsGrants {
            name: ws.name,
            apps,
        });
    }
    out
}

/// Total number of app grants across every workspace — drives populated vs.
/// empty and the summary count.
fn grant_count(all: &[WsGrants]) -> usize {
    all.iter()
        .flat_map(|w| w.apps.iter())
        .map(|a| a.caps.len())
        .sum()
}

/// One capability line: a tinted kind pill, the detail, and a trailing
/// lifetime chip. Read-only — no controls.
fn cap_row(view: &CapView, c: &ui::tokens::colors::Colors) -> El {
    Box::new(
        Row::new()
            .push(status_pill(view.kind, view.tint.color(c)))
            .push(hgap(10.0))
            .push(Box::new(
                Text::new(&view.detail)
                    .with_size(12.0)
                    .with_color(c.text_dim),
            ))
            .push(Box::new(Spacer::new()))
            .push(status_pill(view.expiry, c.neutral[5])),
    )
}

/// One app's block within a workspace card: the app id, then its grants.
fn app_block(grants: &AppGrants, c: &ui::tokens::colors::Colors) -> El {
    let mut items: Vec<El> = Vec::new();
    items.push(Box::new(
        Text::new(&grants.app)
            .with_size(13.0)
            .with_color(c.text_primary),
    ));
    for cap in &grants.caps {
        items.push(cap_row(cap, c));
    }
    rows(items)
}

pub fn build() -> Box<dyn Widget> {
    build_view(load_text().as_deref())
}

/// Core builder, split from [`build`] so it can be exercised against an
/// explicit state document in tests without touching the real config dir.
fn build_view(state_json: Option<&str>) -> Box<dyn Widget> {
    let colors = ui::Theme::dark().tokens.colors;
    let c = &colors;
    let all = state_json.map(project).unwrap_or_default();
    let total = grant_count(&all);

    let mut panel: Vec<El> = Vec::new();

    // Intro: what these grants are and where they come from.
    panel.push(card(
        "App permissions",
        rows(vec![Box::new(
            Text::new(
                "Apps run sandboxed to the workspace they launch in. When an app \
                 reaches beyond it — a file, the camera or microphone, screen \
                 sharing, or the network — the compositor asks you, and an \
                 approval is recorded here as a capability the sandbox enforces.",
            )
            .with_size(12.0),
        )]),
    ));

    if all.is_empty() {
        // Honest empty state — no app has been granted anything (or the
        // compositor has not written its state file yet).
        panel.push(card(
            "No app permissions yet",
            rows(vec![
                Box::new(
                    Text::new(
                        "No app currently holds an out-of-sandbox grant. When you \
                         approve one at the compositor's prompt, it appears here — \
                         grouped by workspace and app — with the access it covers \
                         and how long it lasts.",
                    )
                    .with_size(12.0),
                ),
                vgap(2.0),
                caption(
                    "Nothing here means nothing is granted: apps see only their own \
                     workspace until you say otherwise.",
                ),
            ]),
        ));
    } else {
        // A quiet summary line above the per-workspace cards.
        let ws_n = all.len();
        let summary = format!(
            "{total} grant{} across {ws_n} workspace{}",
            if total == 1 { "" } else { "s" },
            if ws_n == 1 { "" } else { "s" },
        );
        panel.push(Box::new(
            Text::new(&summary).with_size(11.0).with_color(c.text_dim),
        ));

        for ws in &all {
            let mut body: Vec<El> = Vec::new();
            let last = ws.apps.len().saturating_sub(1);
            for (i, app) in ws.apps.iter().enumerate() {
                body.push(app_block(app, c));
                if i != last {
                    body.push(vgap(12.0));
                }
            }
            let title = if ws.name.trim().is_empty() {
                "Workspace".to_string()
            } else {
                ws.name.clone()
            };
            panel.push(card(&title, rows(body)));
        }
    }

    // Where enforcement actually happens — unchanged truth.
    panel.push(card(
        "Where enforcement happens",
        rows(vec![caption(
            "These grants are enforced by the compositor (file access, \
             collaboration, devices) and by the FUSE mount, not by this panel. \
             This is a read-only view of the list they apply; revoke a grant in \
             the workspace that owns it.",
        )]),
    ));

    cards(panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = usize::from(w.name() == name);
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    /// A real state.json fragment: one workspace, one app holding a camera
    /// grant and a read-only file grant. Built by serialising the actual
    /// `CapOverride` type so the fixture matches the compositor's on-disk
    /// shape exactly.
    fn sample_state() -> String {
        let cam = CapOverride::grant(
            Subject::app("org.qos.Camera"),
            Scope::Device("camera".into()),
            Verb::Read,
        );
        let file = CapOverride::grant(
            Subject::app("org.qos.Camera"),
            Scope::FsPath("/var/workspace/Photos".into()),
            Verb::Read,
        );
        let caps = serde_json::to_string(&vec![cam, file]).unwrap();
        format!(r#"{{"workspaces":[{{"name":"Studio","capabilities":{caps}}}]}}"#)
    }

    fn build_layout_of(pane: &mut Box<dyn Widget>) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn is_read_only_with_no_dead_buttons() {
        // Populated or empty, the pane carries no controls that would look live
        // but change nothing — it never writes state.json.
        for json in [None, Some(sample_state().as_str())] {
            let mut pane = build_view(json);
            build_layout_of(&mut pane);
            assert_eq!(count_named(pane.as_ref(), "Button"), 0, "no dead buttons");
        }
    }

    #[test]
    fn projection_reads_the_real_grants_from_state_json() {
        // The projection is exactly what `build_view` renders from, so
        // asserting on it asserts on the production path.
        let all = project(&sample_state());
        assert_eq!(all.len(), 1, "one workspace holds grants");
        assert_eq!(all[0].name, "Studio");
        assert_eq!(all[0].apps.len(), 1, "one app");
        assert_eq!(all[0].apps[0].app, "org.qos.Camera");
        assert_eq!(all[0].apps[0].caps.len(), 2, "camera + file grant");
        assert_eq!(grant_count(&all), 2);
        // The file grant carries the real path + mode; the camera grant its kind.
        let kinds: Vec<&str> = all[0].apps[0].caps.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&"Camera") && kinds.contains(&"Files"));
        let file = all[0].apps[0]
            .caps
            .iter()
            .find(|c| c.kind == "Files")
            .unwrap();
        assert!(file.detail.contains("/var/workspace/Photos"));
        assert!(file.detail.contains("read-only"));
    }

    #[test]
    fn cap_view_maps_each_scope_to_its_kind() {
        let g = |scope, verb| {
            cap_view(&CapOverride::grant(Subject::app("a"), scope, verb)).map(|v| v.kind)
        };
        assert_eq!(
            g(Scope::Device("camera".into()), Verb::Read),
            Some("Camera")
        );
        assert_eq!(
            g(Scope::Device("microphone".into()), Verb::Read),
            Some("Microphone")
        );
        assert_eq!(
            g(Scope::Device("screencast".into()), Verb::Read),
            Some("Screen sharing")
        );
        assert_eq!(
            g(Scope::Device("remote-desktop".into()), Verb::Write),
            Some("Remote control")
        );
        assert_eq!(g(Scope::NetHost("*".into()), Verb::Write), Some("Network"));
        // Read vs write is reflected in the file detail.
        let rw = cap_view(&CapOverride::grant(
            Subject::app("a"),
            Scope::FsPath("/w".into()),
            Verb::Write,
        ))
        .unwrap();
        assert!(rw.detail.contains("read & write"));
        // A restriction (not a grant) renders nothing.
        assert!(
            cap_view(&CapOverride::new(
                Subject::app("a"),
                Scope::Device("camera".into()),
                Effect::Restrict,
                Verb::Read,
            ))
            .is_none()
        );
    }

    #[test]
    fn populated_pane_renders_a_pill_per_grant() {
        // Two grants → the workspace card carries a kind pill + a lifetime chip
        // for each (4 badges), built from the real projection.
        let mut pane = build_view(Some(&sample_state()));
        build_layout_of(&mut pane);
        assert_eq!(
            count_named(pane.as_ref(), "Badge"),
            4,
            "two pills per grant"
        );
        // Intro + one workspace + enforcement cards are all present.
        assert!(count_named(pane.as_ref(), "Card") >= 3);
    }

    #[test]
    fn empty_pane_is_honest_and_names_no_fake_grant() {
        // No state document → an honest empty pane with zero grant pills.
        let mut pane = build_view(None);
        build_layout_of(&mut pane);
        assert_eq!(
            count_named(pane.as_ref(), "Badge"),
            0,
            "no grant pills when empty"
        );
    }

    #[test]
    fn workspace_without_app_grants_is_omitted() {
        // A workspace whose only capability is room membership (a person
        // subject) holds no *app* grant and must not render a card.
        let membership = CapOverride::membership(Subject::person("alice"), "room-1", Verb::Write);
        let caps = serde_json::to_string(&vec![membership]).unwrap();
        let json = format!(r#"{{"workspaces":[{{"name":"Empty","capabilities":{caps}}}]}}"#);
        assert!(
            project(&json).is_empty(),
            "no app grants → no workspace card"
        );
    }

    #[test]
    fn garbage_document_is_treated_as_empty() {
        assert!(project("not json {").is_empty());
        let mut pane = build_view(Some("not json {"));
        build_layout_of(&mut pane);
        assert_eq!(count_named(pane.as_ref(), "Badge"), 0);
    }
}
