#![allow(
    clippy::exit,
    reason = "a capture that cannot observe its subject must fail the process"
)]

//! Offscreen shots of the one window shell (`NavShell`) and the one
//! navigation list (`NavList`), through the same `FrameHarness` frame the
//! live window and every app's `--shots` use.
//!
//! Writes, into `OUT_DIR` (else `$NAV_SHELL_SHOTS_OUT`, else
//! `target/look/nav-shell`):
//!
//! - `mail-1280-{dark,light,high-contrast}.png` — sidebar, overview and
//!   content docked, one header row across the window.
//! - `mail-1280-hidden.png` — the sidebar hidden; the toggle stays
//!   top-leading.
//! - `mail-800.png`, `mail-800-drawer.png` — the sidebar collapses to a
//!   floating drawer instead of squeezing the columns.
//! - `mail-560.png`, `mail-560-detail.png` — overview and content become
//!   push screens with a Back button.
//! - `mail-1280-text2x.png` — 200 % text.
//! - `settings-560.png` — `NavMode::Push`: the sidebar is the first screen.
//! - `rail-1280.png` — a docked icon rail.
//! - `mail-1280-history-{dark,light}.png` — a document with History: its
//!   control at the trailing edge of the header row.
//! - `mail-1280-history-open-{dark,light}.png` — History showing beside the
//!   app, its control reading as on.
//! - `mail-800-drawer-history-{dark,light}.png` — the drawer open under the
//!   header row: the columns stay under the scrim, History stays reachable.
//! - `mail-800-history-open-{dark,light}.png` — History showing at 800 × 600.
//! - `actions-{1280,800,520}-{dark,light}.png` — labelled actions, then
//!   their icons, then the More menu, as the row narrows.
//! - `pages-back-{dark,light}.png` — the shell's Back for an app's page
//!   stack, with a count after the title.
//! - `files-path-{dark,light}.png` — a path bar as the content header's
//!   leading fill.
//!
//! Run: `cargo run -p ui-examples --example ui-shots -- nav_shell_shots [OUT_DIR]`

use std::path::{Path, PathBuf};

use tilo_ui::TextRole;
use tilo_ui::headless::FrameHarness;
use tilo_ui::icons::Icon;
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    Button, Column, Element, NavAction, NavItem, NavList, NavMode, NavPane, NavSection, NavShell,
    Text,
};

fn mailboxes() -> NavList {
    NavList::new(vec![
        NavSection::untitled(
            "favourites",
            vec![
                NavItem::new("inbox", "Inbox")
                    .icon(Icon::Inbox)
                    .live_count(4),
                NavItem::new("drafts", "Drafts")
                    .icon(Icon::FileText)
                    .count(2),
                NavItem::new("sent", "Sent").icon(Icon::Send),
                NavItem::new("archive", "Archive").icon(Icon::Archive),
                NavItem::new("trash", "Trash").icon(Icon::Trash2),
            ],
        ),
        NavSection::new(
            "folders",
            "Folders",
            vec![
                NavItem::new("work", "Work")
                    .icon(Icon::Folder)
                    .count(128)
                    .children(vec![
                        NavItem::new("design-reviews", "Design reviews").icon(Icon::Folder),
                        NavItem::new("hiring", "Hiring").icon(Icon::Folder).count(6),
                    ])
                    .expanded(),
                NavItem::new("travel", "Travel").icon(Icon::Folder),
                NavItem::new("receipts", "Receipts and invoices for the tax return")
                    .icon(Icon::Folder)
                    .count(41),
            ],
        )
        .collapsible()
        .trailing_action(NavAction::new("new-folder", Icon::Plus, "New folder")),
    ])
    .selected("inbox")
    .with_accessible_name("Mailboxes")
}

fn messages() -> Element {
    let mut column = Column::new().with_intrinsic_height();
    for (from, subject, when) in [
        ("Robin Keller", "Design review moved to Thursday", "09:41"),
        ("Sam Okafor", "Notes from the onboarding study", "08:15"),
        ("Kai Tanaka", "Release candidate is green", "Yesterday"),
        ("Priya Nair", "Quarterly planning: first draft", "Yesterday"),
        ("Lena Fischer", "Lunch on Friday?", "Monday"),
    ] {
        column = column.push(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(
                    Text::new(from).with_role(TextRole::Label).strong(),
                ))
                .push(Box::new(Text::new(subject).with_role(TextRole::Body)))
                .push(Box::new(
                    Text::new(when).with_role(TextRole::Caption).dimmed(),
                )),
        ));
    }
    Box::new(column)
}

fn message() -> Element {
    Box::new(
        Column::new()
            .push(Box::new(
                Text::new("From Robin Keller")
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ))
            .push(Box::new(
                Text::new(
                    "Hi! The design review moved to Thursday at 14:00. I put the \
                     navigation shots in the shared folder; the sidebar now collapses \
                     into a drawer at narrow widths instead of squeezing the list.",
                )
                .with_role(TextRole::Body),
            )),
    )
}

fn icon_button(id: &str, icon: Icon, name: &str) -> Element {
    Box::new(Button::icon_only(id, icon, name))
}

fn mail() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(mailboxes()))
        .overview(messages())
        .content(message())
        .sidebar_header("Mail", Vec::new())
        .overview_header(
            "Inbox",
            vec![icon_button("compose", Icon::Pencil, "New message")],
        )
        .content_header(
            "Design review moved to Thursday",
            vec![
                icon_button("reply", Icon::Reply, "Reply"),
                icon_button("delete", Icon::Trash2, "Delete message"),
            ],
        )
}

fn settings() -> NavShell {
    NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(
            NavList::new(vec![NavSection::untitled(
                "categories",
                vec![
                    NavItem::new("network", "Network").icon(Icon::Wifi),
                    NavItem::new("bluetooth", "Bluetooth").icon(Icon::Bluetooth),
                    NavItem::new("displays", "Displays").icon(Icon::Monitor),
                    NavItem::new("sound", "Sound").icon(Icon::Volume2),
                    NavItem::new("power", "Power").icon(Icon::Battery),
                    NavItem::new("privacy", "Privacy").icon(Icon::Shield),
                ],
            )])
            .selected("displays"),
        ))
        .content(message())
        .sidebar_header("Settings", Vec::new())
        .content_header("Displays", Vec::new())
}

fn rail() -> NavShell {
    NavShell::new()
        .mode(NavMode::Docked)
        .sidebar(Box::new(
            NavList::new(vec![NavSection::untitled(
                "channels",
                vec![
                    NavItem::new("home", "Home").icon(Icon::Home),
                    NavItem::new("subs", "Following")
                        .icon(Icon::ListVideo)
                        .live_count(3),
                    NavItem::new("history", "History").icon(Icon::History),
                    NavItem::new("library", "Library").icon(Icon::GalleryVerticalEnd),
                ],
            )])
            .rail()
            .selected("home"),
        ))
        .content(message())
        .content_header("Home", Vec::new())
}

fn labelled(id: &str, label: &str, icon: Icon) -> Element {
    Box::new(Button::new(id, label).with_icon(icon).on_view(|| {}))
}

/// A message view whose labelled actions collapse to icons, then into More.
fn message_actions() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(mailboxes()))
        .content(message())
        .sidebar_header("Mail", Vec::new())
        .content_header(
            "Design review moved to Thursday",
            vec![
                labelled("reply", "Reply", Icon::Reply),
                labelled("forward", "Forward", Icon::Send),
                labelled("archive", "Archive", Icon::Archive),
                labelled("delete", "Delete", Icon::Trash2),
            ],
        )
        .content_count(3)
}

/// A drill-down page stack: the shell's Back pops it.
fn pages() -> NavShell {
    let stack = tilo_ui::LocalState::new(tilo_ui::NavStack::new("Dashboard"));
    stack.update(|pages| pages.push("Print queue"));
    NavShell::new()
        .content(message())
        .content_header(
            "Print queue",
            vec![icon_button("pause", Icon::Pause, "Pause all")],
        )
        .content_count(4)
        .with_back(&stack)
}

/// A path bar as the content header's leading fill.
fn files() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(mailboxes()))
        .content(message())
        .sidebar_header("Places", Vec::new())
        .content_header(
            "",
            vec![icon_button("new-folder", Icon::FolderPlus, "New folder")],
        )
        .content_header_leading(Box::new(
            Text::new("Home  ›  Documents  ›  Taxes 2026").with_role(TextRole::Label),
        ))
}

fn shoot(
    shell: NavShell,
    theme: &Theme,
    size: (u32, u32),
    out: &Path,
    set: impl Fn(&mut NavShell),
) {
    let mut harness = FrameHarness::for_root(State::new(), shell, size, theme.clone());
    for _ in 0..2 {
        set(harness.root_mut::<NavShell>());
        harness.root_mut::<NavShell>().snap();
        if let Err(error) = harness.relayout_at(size) {
            eprintln!("nav_shell_shots: layout failed: {error}");
            std::process::exit(1);
        }
    }
    harness.render_png(out);
    println!("wrote {}", out.display());
}

/// The mail window over a document with a few changes, with the window's
/// History control in its header row, and History showing when `open`.
fn shoot_history(theme: &Theme, open: bool, out: &Path) {
    shoot_history_at(theme, open, (1280, 800), |_| {}, out);
}

fn shoot_history_at(
    theme: &Theme,
    open: bool,
    size: (u32, u32),
    set: impl Fn(&mut NavShell),
    out: &Path,
) {
    use automerge::transaction::Transactable;
    let mut state = State::new();
    for subject in [
        "Design review",
        "Design review moved to Thursday",
        "Design review moved to Thursday, 14:00",
    ] {
        let _ = state.doc.put(automerge::ROOT, "subject", subject);
        state.commit_action(None, None);
    }
    let mut harness = FrameHarness::for_root(state, mail(), size, theme.clone());
    harness.enable_document_history_chrome();
    if open && let Err(error) = harness.open_history() {
        eprintln!("nav_shell_shots: History did not open: {error}");
        std::process::exit(1);
    }
    for _ in 0..2 {
        set(harness.root_mut::<NavShell>());
        harness.root_mut::<NavShell>().snap();
        if let Err(error) = harness.relayout_at(size) {
            eprintln!("nav_shell_shots: layout failed: {error}");
            std::process::exit(1);
        }
    }
    harness.render_png(out);
    println!("wrote {}", out.display());
}

pub fn run(args: &[String]) {
    let dir = args
        .first()
        .cloned()
        .or_else(|| std::env::var("NAV_SHELL_SHOTS_OUT").ok())
        .unwrap_or_else(|| "target/look/nav-shell".to_string());
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("nav_shell_shots: cannot create {dir}: {error}");
        std::process::exit(1);
    }
    let dir = PathBuf::from(dir);
    let wide = (1280, 800);

    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let name = theme.name.to_lowercase().replace(' ', "-");
        shoot(
            mail(),
            &theme,
            wide,
            &dir.join(format!("mail-1280-{name}.png")),
            |_| {},
        );
    }
    let theme = Theme::dark();
    shoot(
        mail(),
        &theme,
        wide,
        &dir.join("mail-1280-hidden.png"),
        |shell| {
            shell.close_sidebar();
        },
    );
    shoot(
        mail(),
        &theme,
        (800, 600),
        &dir.join("mail-800.png"),
        |_| {},
    );
    shoot(
        mail(),
        &theme,
        (800, 600),
        &dir.join("mail-800-drawer.png"),
        |shell| {
            shell.open_sidebar();
        },
    );
    shoot(
        mail(),
        &theme,
        (560, 800),
        &dir.join("mail-560.png"),
        |_| {},
    );
    shoot(
        mail(),
        &theme,
        (560, 800),
        &dir.join("mail-560-detail.png"),
        |shell| shell.show(NavPane::Content),
    );
    shoot(
        mail(),
        &Theme::dark().with_text_scale(2.0),
        wide,
        &dir.join("mail-1280-text2x.png"),
        |_| {},
    );
    shoot(
        settings(),
        &Theme::light(),
        (560, 800),
        &dir.join("settings-560.png"),
        |_| {},
    );
    shoot(
        settings(),
        &Theme::light(),
        wide,
        &dir.join("settings-1280.png"),
        |_| {},
    );
    shoot(rail(), &theme, wide, &dir.join("rail-1280.png"), |_| {});
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        shoot_history(
            &theme,
            false,
            &dir.join(format!("mail-1280-history-{name}.png")),
        );
        shoot_history(
            &theme,
            true,
            &dir.join(format!("mail-1280-history-open-{name}.png")),
        );
        shoot_header_row(&theme, name, &dir);
    }
}

/// The header row's capabilities in `theme`: an open drawer under the row,
/// History at 800 px, actions narrowing into More, an app's page stack and
/// a leading path bar.
fn shoot_header_row(theme: &Theme, name: &str, dir: &Path) {
    {
        let theme = theme.clone();
        // An open drawer under the header row: History stays reachable, the
        // columns stay under the scrim.
        shoot_history_at(
            &theme,
            false,
            (800, 600),
            NavShell::open_sidebar,
            &dir.join(format!("mail-800-drawer-history-{name}.png")),
        );
        shoot_history_at(
            &theme,
            true,
            (800, 600),
            |_| {},
            &dir.join(format!("mail-800-history-open-{name}.png")),
        );
        for (width, label) in [(1280, "1280"), (800, "800"), (520, "520")] {
            shoot(
                message_actions(),
                &theme,
                (width, 600),
                &dir.join(format!("actions-{label}-{name}.png")),
                |_| {},
            );
        }
        shoot(
            pages(),
            &theme,
            (1024, 700),
            &dir.join(format!("pages-back-{name}.png")),
            |_| {},
        );
        shoot(
            files(),
            &theme,
            (1280, 800),
            &dir.join(format!("files-path-{name}.png")),
            |_| {},
        );
    }
}
