//! End-to-end persistence proofs — the real "control → shared JSON store"
//! path, driven through each pane's production `sync` and read back from
//! disk.
//!
//! These run in this dedicated test binary (a separate process) so pointing
//! `XDG_CONFIG_HOME` at a throwaway directory fully isolates the shared
//! `~/.config/qos/settings.json` the panes write. A process-wide lock
//! serialises the env mutation so the tests don't race each other.

use std::path::PathBuf;
use std::sync::Mutex;

use automerge::transaction::Transactable;
use automerge::AutoCommit;
use settings::accounts::{AccountDirectory, AccountKind, MailStore};
use settings::prefs::SystemPrefs;
use ui::widget::Widget;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Create a fresh, empty config root for a test and point the process at it.
/// Returns the directory (kept alive for the test's duration).
fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "qos-settings-persist-{}-{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: guarded by ENV_LOCK so no other test reads/writes the env
    // concurrently; this binary is a separate process from the other tests.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    }
    dir
}

fn seed_then_set_bool(pane: &mut dyn Widget, key: &str, value: bool) {
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root); // first sync seeds
    let _ = doc.put(&root, key, value);
    pane.sync(&mut doc, &root); // observe + persist
}

#[test]
fn audio_event_sounds_toggle_persists_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("audio");

    let mut pane = settings::panes::audio::build();
    // Default is `true`; flip it off through the production path.
    seed_then_set_bool(pane.as_mut(), settings::panes::audio::EVENT_SOUNDS_KEY, false);

    let reloaded = SystemPrefs::load();
    assert!(
        !reloaded.audio.event_sounds,
        "the event-sounds toggle wrote through to the shared store"
    );
}

#[test]
fn display_night_light_toggle_persists_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("display");

    let mut pane = settings::panes::display::build();
    seed_then_set_bool(pane.as_mut(), settings::panes::display::NIGHT_LIGHT_KEY, true);

    assert!(
        SystemPrefs::load().display.night_light,
        "the night-light toggle persisted"
    );
}

#[test]
fn input_pointer_speed_persists_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("input");

    let mut pane = settings::panes::mouse_keyboard::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root);
    let _ = doc.put(
        &root,
        settings::panes::mouse_keyboard::POINTER_SPEED_KEY,
        automerge::ScalarValue::F64(0.9),
    );
    pane.sync(&mut doc, &root);

    let reloaded = SystemPrefs::load();
    assert!(
        (reloaded.input.pointer_speed - 0.9).abs() < 1e-6,
        "pointer speed persisted, got {}",
        reloaded.input.pointer_speed
    );
}

#[test]
fn updates_channel_persists_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("updates");

    let mut pane = settings::panes::updates::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root);
    let _ = doc.put(&root, settings::panes::updates::CHANNEL_KEY, "Nightly");
    pane.sync(&mut doc, &root);

    assert_eq!(
        SystemPrefs::load().updates.channel,
        "Nightly",
        "release channel persisted"
    );
}

/// The External Accounts pane's Add flow writes a real account into the same
/// `mail/accounts.json` Mail reads, and Remove deletes it — the full
/// load/edit/save path through the pane, not just the store unit test.
#[test]
fn accounts_add_and_remove_round_trip_through_the_pane() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("accounts");

    // Fresh install → empty stores.
    assert!(AccountDirectory::load().is_empty());

    let mut pane = settings::panes::accounts::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root);

    // Open the Add-email form, fill it, and Save.
    let _ = doc.put(&root, settings::panes::accounts::ADD_MAIL_KEY, true);
    pane.sync(&mut doc, &root);
    let _ = doc.put(&root, settings::panes::accounts::FORM_NAME_KEY, "Test User");
    let _ = doc.put(&root, settings::panes::accounts::FORM_EMAIL_KEY, "test@gmail.com");
    let _ = doc.put(&root, settings::panes::accounts::FORM_SAVE_KEY, true);
    pane.sync(&mut doc, &root);

    // The account is now in the on-disk store Mail loads, auto-configured.
    let store = MailStore::load();
    assert_eq!(store.accounts.len(), 1, "Add persisted a real account");
    assert_eq!(store.accounts[0].config.email, "test@gmail.com");
    assert_eq!(
        store.accounts[0].config.imap_host, "imap.gmail.com",
        "IMAP host was auto-configured from the address"
    );
    let id = store.accounts[0].id.clone();

    // Now Remove it via the trash button's key.
    let remove_key = format!("settings.accounts.remove.{id}");
    let _ = doc.put(&root, remove_key.as_str(), true);
    pane.sync(&mut doc, &root);

    assert!(
        MailStore::load().accounts.is_empty(),
        "Remove deleted the account from disk"
    );
    // And the directory now reports empty for the Email kind.
    assert!(AccountDirectory::load().views_of(AccountKind::Mail).is_empty());
}
