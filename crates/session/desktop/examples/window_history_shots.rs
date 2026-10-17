//! Captures of apps in a desktop window, and where their History control
//! stands: Chat and Notes at rest, Chat's tab with the pointer resting on
//! it (its window controls revealed), Chat with History open, and Chat in
//! the desktop surface with History open and with its tab's search open.
//! Run with an output directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a capture treats a failed setup as fatal"
)]

use std::cell::RefCell;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use ui::application::{
    ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, UserId};
use ui::record::{RecordCheckout, RecordOwners};
use ui::theme::Theme;
use ui::time::Duration;
use ui::widget::{AppKind, TabDescriptor, TileHost, WindowGroup, WindowWidth};

const ROOM: &str = "room";
const AUTHOR: &str = "u_me";
const SIZE: (u32, u32) = (1100, 720);

/// `definition`'s component on a record `seed` fills, mounted the way the
/// browser desktop mounts a window's application.
fn tile(
    profile: &Path,
    definition: &'static ApplicationDefinition,
    instance: u64,
    seed: impl FnOnce(&mut ui::automerge::AutoCommit),
) -> TileHost {
    let mut owners = RecordOwners::new(profile.to_owned());
    let records = owners.open(definition.app_id, ROOM).unwrap();
    let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
    if records.try_open_record(&checkout).is_err() {
        records
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                seed(doc);
                Ok(())
            })
            .unwrap();
    }
    let views = Rc::new(InstanceViewStore::new(profile.to_owned(), "device", ROOM).unwrap());
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: "device".into(),
            instance_id: instance,
            permissions: Rc::new(RefCell::new(PermissionContext::member(
                AUTHOR,
                ui::permissions::Role::Owner,
            ))),
            author: UserId(AUTHOR.into()),
            action_context: ui::tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    TileHost::mount_component(component, |_| {}).unwrap()
}

fn window(tab: u64, title: &str, kind: Option<AppKind>, host: TileHost) -> FrameHarness {
    let mut descriptor = TabDescriptor::new(tab, title);
    descriptor.app_kind = kind;
    let group = WindowGroup::from_hosts(1, vec![(descriptor, host)], tab, WindowWidth::Wide);
    FrameHarness::new_with_theme(ui::State::new(), Box::new(group), SIZE, Theme::dark())
}

fn capture(frame: &mut FrameHarness, out: &Path, name: &str) {
    frame.settle_for_capture(Duration::from_secs(2));
    frame.render_png(&out.join(name));
    eprintln!("wrote {}", out.join(name).display());
}

/// Rest the pointer on the window's first tab long enough for its pill to
/// open onto the window's controls.
fn rest_on_tab(frame: &mut FrameHarness) {
    let tabs = *frame
        .targets()
        .get("tabs")
        .expect("the window shows its tab row");
    let at = (tabs.x0 + 40.0, tabs.center().y);
    frame.send(&ui::synthetic_input::cursor_moved(at.0, at.1));
    std::thread::sleep(std::time::Duration::from_millis(600));
    frame.send(&ui::synthetic_input::cursor_moved(at.0 + 1.0, at.1));
    std::thread::sleep(std::time::Duration::from_millis(400));
    frame.relayout();
}

fn main() -> Result<(), Box<dyn Error>> {
    let out = std::env::args().nth(1).map_or_else(
        || std::path::PathBuf::from("target/window-history"),
        Into::into,
    );
    std::fs::create_dir_all(&out)?;
    let profile = tempfile::tempdir()?;

    let chat = tile(&profile.path().join("chat"), chat::definition(), 7, |doc| {
        chat::demo::seed(doc).unwrap();
    });
    let mut frame = window(7, "Chat", None, chat);
    capture(&mut frame, &out, "chat-window.png");
    rest_on_tab(&mut frame);
    capture(&mut frame, &out, "chat-window-tab-hover.png");
    if frame
        .perform_semantic_named("tabs", Some("tab-history:7"), &ui::a11y::Action::Activate)
        .is_ok()
    {
        frame.relayout();
        capture(&mut frame, &out, "chat-window-history-open.png");
    }

    let notes = tile(
        &profile.path().join("notes"),
        notes::definition(),
        8,
        |_| {},
    );
    let mut frame = window(8, "Notes", Some(AppKind::Editor), notes);
    capture(&mut frame, &out, "notes-window.png");

    desktop(&out, &profile.path().join("desktop"));
    Ok(())
}

/// Chat as the browser desktop shows it: a window of the desktop surface.
struct ChatTiles(std::path::PathBuf);

impl tilo_desktop_surface::TileSource for ChatTiles {
    fn tile(
        &mut self,
        _: &tilo_desktop_model::Workspace,
        tab: &tilo_desktop_model::Tab,
    ) -> TileHost {
        tile(&self.0, chat::definition(), tab.id, |doc| {
            chat::demo::seed(doc).unwrap();
        })
    }
}

/// Chat in the desktop surface: History opened from its tab, then the
/// tab's search control.
fn desktop(out: &Path, profile: &Path) {
    let mut minter = tilo_desktop_model::IdMinter::new(7);
    let id = minter.mint();
    let tab = tilo_desktop_model::Tab {
        id,
        title: "Chat".into(),
        tile: tilo_desktop_model::TileContent::LocalApp {
            app_id: "so.tilo.chat".into(),
            label: "Chat".into(),
            note: String::new(),
        },
    };
    let tree = tilo_desktop_surface::seed_tree(&mut minter, "Desk", vec![tab]);
    let (surface, _handle) =
        tilo_desktop_surface::DesktopSurface::new(tree, 7, Box::new(ChatTiles(profile.into())));
    let mut frame =
        FrameHarness::new_with_theme(ui::State::new(), Box::new(surface), SIZE, Theme::dark());
    for _ in 0..4 {
        frame.relayout();
    }
    let press = |frame: &mut FrameHarness, control: &str| {
        frame
            .perform_semantic_named(
                "tabs",
                Some(&format!("{control}:{id}")),
                &ui::a11y::Action::Activate,
            )
            .expect("the tab offers the control");
        for _ in 0..3 {
            frame.relayout();
        }
    };
    press(&mut frame, "tab-history");
    capture(&mut frame, out, "chat-desktop-history-open.png");
    press(&mut frame, "tab-history");
    press(&mut frame, "tab-search");
    capture(&mut frame, out, "chat-desktop-search.png");
    for letter in ["g", "e", "n"] {
        frame.send(&ui::synthetic_input::char_key(letter));
    }
    frame.relayout();
    capture(&mut frame, out, "chat-desktop-search-typed.png");
}
