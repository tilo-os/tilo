//! Pictures of the overview the Tilo mark zooms out to.
//!
//! A desk with two workspaces, one of them running on Tilo Cloud: the mark
//! opens the overview, caught on its way in and at rest, with the person's
//! Tilo below the workspaces (Hosts & compute with the plan, usage and
//! invoices, an agent let in, and the account). Captured in both themes at
//! a desktop and a phone viewport, through the production frame path
//! ([`ui::headless::FrameHarness`]).
//!
//! ```text
//! cargo run -p tilo-desktop --example tilo_overview_shots -- ~/.cache/tilo-overview-shots
//! ```

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use tilo_actions::remote::agent_door::{Door, Grant, Run};
use tilo_actions::remote::host_account::{
    Entitlement, HostAccount, HostKind, Invoice, InvoiceStatus, Limits, PaymentMethod, PlanOffer,
    Standing, Usage, WorkspaceUsage,
};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::ClockSnapshot;
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::overlays::Overlays;
use tilo_desktop::overview_tilo::{AccountLine, TiloDesk};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::window_flight::WindowFlight;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::DesktopSurface;
use ui::headless::FrameHarness;
use ui::kurbo::Rect;
use ui::theme::Theme;

/// A workspace in `slot` whose strip `strip` holds one window per app, each window numbered by its
/// tab.
fn workspace(slot: u64, strip: u64, name: &str, apps: &[(u64, &str, &str)]) -> Workspace {
    let groups = apps
        .iter()
        .map(|&(id, app, label)| WindowGroup {
            id,
            active_tab: id,
            tabs: vec![Tab {
                id,
                title: label.into(),
                tile: TileContent::LocalApp {
                    app_id: app.into(),
                    label: label.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    let mut workspace = Workspace::new(WorkspaceSlot(slot), name);
    workspace.strips = vec![Strip {
        id: StripId(strip),
        name: "Main".into(),
        window_groups: groups,
    }];
    workspace
}

fn desk_tree() -> DesktopTree {
    let work = workspace(
        1,
        10,
        "Work",
        &[(20, "so.tilo.notes", "Notes"), (21, "so.tilo.chat", "Chat")],
    );
    let build = workspace(2, 20, "Build box", &[(30, "so.tilo.terminal", "Terminal")]);
    let mut tree = DesktopTree {
        active: work.slot,
        workspaces: vec![work, build],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(10));
    tree.active_strip.insert(WorkspaceSlot(2), StripId(20));
    tree
}

/// Tilo Cloud selling Pro, the account on it with a card, the build box's
/// Linux machine having run most of a working week.
fn tilo_cloud(build_box: &str) -> HostAccount {
    let pro = Limits {
        max_running: 2,
        max_vms: 5,
        sizes: vec!["small".into(), "medium".into()],
        included_vm_seconds: Some(100 * 3600),
        ..Limits::default()
    };
    HostAccount {
        host: "Tilo Cloud".into(),
        kind: HostKind::Provider,
        plans: vec![PlanOffer {
            id: "pro".into(),
            name: "Pro".into(),
            price: Some("$20 a month".into()),
            limits: pro.clone(),
        }],
        standing: Some(Standing {
            account: "acct-ada".into(),
            plan: Some("pro".into()),
            payment_method: Some(PaymentMethod::default()),
            entitlements: vec![Entitlement::LinuxVms],
            limits: Some(pro),
            usage: Usage {
                workspaces: vec![WorkspaceUsage {
                    workspace: build_box.into(),
                    vm_seconds: 37 * 3600,
                    running: true,
                    ..WorkspaceUsage::default()
                }],
                ..Usage::default()
            },
            credit: Some("$12.40".into()),
            invoices: vec![
                Invoice {
                    id: "in_2".into(),
                    amount: "$24.10".into(),
                    status: InvoiceStatus::Open,
                    url: "https://tilo.sh/billing/invoices/in_2".into(),
                    ..Invoice::default()
                },
                Invoice {
                    id: "in_1".into(),
                    amount: "$20.00".into(),
                    status: InvoiceStatus::Paid,
                    url: "https://tilo.sh/billing/invoices/in_1".into(),
                    ..Invoice::default()
                },
            ],
        }),
        manage_billing: Some("https://tilo.sh/billing".into()),
    }
}

fn tilo_desk(build_box: &str) -> TiloDesk {
    TiloDesk {
        hosts: vec![
            HostAccount::sells_nothing("Framework 16", HostKind::Device),
            tilo_cloud(build_box),
        ],
        door: Some(Door {
            grants: vec![Grant {
                id: "g1".into(),
                agent: "Claude Code".into(),
                runs: vec![Run {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }],
                ..Grant::default()
            }],
            ..Door::default()
        }),
        account: Some(AccountLine {
            name: "Ada Lovelace".into(),
            detail: Some("ada@tilo.sh".into()),
        }),
        ..TiloDesk::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("tilo-overview-shots"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    ui::i18n::I18nManager::init("en-US");
    for (size, (w, h)) in [("desktop", (1280_u32, 800_u32)), ("phone", (390, 844))] {
        for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let tree = desk_tree();
            let build_box = tree.workspaces[1].id().to_string();
            let (_, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
            let display = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
            let overlays = Overlays::new();
            overlays.set_displays(&[display], Some(display));
            let mut chrome = DesktopChrome::new(
                handle.clone(),
                "g".into(),
                std::rc::Rc::new(PermissiveRules::new(handle)),
                TopBarChrome::from_tree(&tree)
                    .with_tilo_mark()
                    .with_clock(ClockSnapshot {
                        unix_seconds: 1_794_250_800,
                        clock: "9:41".into(),
                        weekday: "Mon".into(),
                        date: "Nov 9".into(),
                    }),
                TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
                false,
            );
            chrome.set_overlays(overlays);
            let flight = WindowFlight::new();
            chrome.set_overview(flight.clone(), std::rc::Rc::default());
            chrome.set_tilo(tilo_desk(&build_box));
            let mut frame = FrameHarness::for_root(ui::state::State::new(), chrome, (w, h), theme);
            frame.relayout();
            frame
                .root_mut::<DesktopChrome>()
                .apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleTiloMenu);
            // The zoom's clock is the host's: stop it on the way in, then at rest.
            for (step, progress) in [("40", 0.4), ("70", 0.7), ("rest", 1.0)] {
                flight.set_clock(progress, HashMap::new());
                for _ in 0..30 {
                    frame.relayout();
                    if !frame.poll_frame().expect("a frame draws the overview") {
                        break;
                    }
                }
                let path = output.join(format!("zoom-{step}-{size}-{theme_name}.png"));
                frame.render_png(&path);
                println!("{}", path.display());
            }
        }
    }
    Ok(())
}
