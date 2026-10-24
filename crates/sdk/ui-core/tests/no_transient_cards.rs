//! A window says a transient notice in its bar ([`tilo_ui::window_toolbar`]),
//! never on a card over its content. The notification card
//! ([`tilo_ui::widget::toast::ToastCard`]) is the shell's: the desktop's
//! notifications, the lock screen and the shell's overlays paint it, and no
//! app or window widget does.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the card may be painted: its own module and tests, and the shell.
const ALLOWED: &[&str] = &[
    "crates/sdk/ui-core/src/widget/toast.rs",
    "crates/sdk/ui-core/src/widget/toast_tests.rs",
    "crates/sdk/ui-core/tests/toast_test.rs",
    "crates/sdk/ui-core/tests/no_transient_cards.rs",
    "crates/session/desktop-surface/",
    "crates/session/shell/",
];

/// What painting the card reads like.
const CARD: &[&str] = &[
    "ToastCard",
    "toast::paint_card",
    "toast::paint_card_surface",
    "toast::card_height",
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_window_widget_paints_a_transient_card() {
    let root = repository();
    let mut files = Vec::new();
    for top in ["apps", "crates", "examples"] {
        sources(&root.join(top), &mut files);
    }
    assert!(
        files.iter().any(|file| file.ends_with("widget/toast.rs")),
        "the walk reaches the card's own module"
    );
    let mut painted = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .expect("under the repository")
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|allowed| relative.starts_with(allowed)) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if CARD.iter().any(|card| line.contains(card)) {
                painted.push(format!("{relative}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        painted.is_empty(),
        "a window says a transient notice in its bar (window_toolbar::notify, \
         Transient::publish), not on a notification card over its content:\n{}",
        painted.join("\n")
    );
}
