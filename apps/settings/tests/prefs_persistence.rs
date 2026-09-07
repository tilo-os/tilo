//! End-to-end persistence proofs — the real "control → shared JSON store"
//! path, driven through each pane's production `sync` and read back from
//! disk.
//!
//! These run in this dedicated test binary (a separate process) so pointing
//! `XDG_CONFIG_HOME` at a throwaway directory fully isolates the shared
//! `~/.config/qos/settings.json` the panes write. A process-wide lock
//! serialises the env mutation so the tests don't race each other.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::sync::Mutex;

use automerge::AutoCommit;
use automerge::transaction::Transactable;
use settings::accounts::{AccountDirectory, AccountKind};
use settings::prefs::SystemPrefs;
use settings::region::{RegionSettings, TimeStyle};
use ui::widget::Widget;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct AcceptAccount;

impl settings::account_probe::AccountTester for AcceptAccount {
    fn test(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<String, String> {
        Ok("signed in".into())
    }
}

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
fn display_night_light_toggle_persists_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("display");

    let mut pane = settings::panes::display::build();
    seed_then_set_bool(
        pane.as_mut(),
        settings::panes::display::NIGHT_LIGHT_KEY,
        true,
    );

    assert!(
        SystemPrefs::load().display.night_light,
        "the night-light toggle persisted"
    );
}

#[test]
fn input_device_speeds_persist_independently_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("input");

    let mut pane = settings::panes::mouse_keyboard::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root);
    let _ = doc.put(
        &root,
        settings::panes::mouse_keyboard::TOUCHPAD_POINTER_SPEED_KEY,
        automerge::ScalarValue::F64(0.9),
    );
    let _ = doc.put(
        &root,
        settings::panes::mouse_keyboard::MOUSE_POINTER_SPEED_KEY,
        automerge::ScalarValue::F64(0.2),
    );
    pane.sync(&mut doc, &root);

    let reloaded = SystemPrefs::load();
    assert!(
        (reloaded.input.touchpad_pointer_speed.unwrap_or_default() - 0.9).abs() < 1e-6,
        "touchpad speed persisted"
    );
    assert!(
        (reloaded.input.mouse_pointer_speed.unwrap_or_default() - 0.2).abs() < 1e-6,
        "mouse speed persisted"
    );
}

/// The Time & Date "24-hour clock" toggle folds into the shared locale that
/// Region & Formats owns, so the two panes agree and the choice survives a
/// restart. Driving the pane's real `sync` must write `locale.time_format`.
#[test]
fn time_date_clock_format_persists_to_shared_locale() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("timedate");

    // Fresh install defaults to 24-hour.
    assert_eq!(RegionSettings::load().locale.time_format, TimeStyle::H24);

    let mut pane = settings::panes::time_date::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root); // seeds h24 = true from the default locale
    // Flip the clock to 12-hour the way the Toggle would on click.
    let _ = doc.put(
        &root,
        settings::panes::time_date::H24_KEY,
        automerge::ScalarValue::Boolean(false),
    );
    pane.sync(&mut doc, &root);

    assert_eq!(
        RegionSettings::load().locale.time_format,
        TimeStyle::H12,
        "the clock-format toggle wrote through to the shared locale"
    );
}

/// The Power pane's idle preferences (suspend-when-idle, screen-blank timeout)
/// write through to the shared store the compositor's power layer reads.
#[test]
fn power_idle_prefs_persist_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("power");

    // Fresh install defaults: auto-suspend on, blank at 10 minutes.
    let base = SystemPrefs::load();
    assert!(base.power.auto_suspend);
    assert_eq!(base.power.blank_minutes, 10);

    let mut pane = settings::panes::power::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root); // seeds from defaults
    // Turn suspend off and pick a 30-minute blank the way the controls would.
    let _ = doc.put(
        &root,
        settings::panes::power::AUTO_SUSPEND_KEY,
        automerge::ScalarValue::Boolean(false),
    );
    let _ = doc.put(&root, settings::panes::power::BLANK_KEY, "30 minutes");
    pane.sync(&mut doc, &root);

    let reloaded = SystemPrefs::load();
    assert!(!reloaded.power.auto_suspend, "auto-suspend persisted off");
    assert_eq!(reloaded.power.blank_minutes, 30, "blank timeout persisted");
}

#[test]
fn display_scale_persists_fractional_factor_to_disk() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("display-scale");

    let mut pane = settings::panes::display::build();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root); // seed (writes "100%")
    // User picks a fractional 150% scale.
    let _ = doc.put(&root, settings::panes::display::DISPLAY_SCALE_KEY, "150%");
    pane.sync(&mut doc, &root); // observe + persist

    let scale = SystemPrefs::load().display.scale;
    assert!(
        (scale - 1.5).abs() < 1e-9,
        "the display-scale dropdown persisted a fractional 1.5, got {scale}"
    );
}

/// The External Accounts pane's Add flow writes a real account into the shared
/// canonical accounts store (the same store Mail reads), and Remove deletes it
/// — the full load/edit/save path through the pane, not just the store unit
/// test.
#[test]
fn accounts_add_and_remove_round_trip_through_the_pane() {
    let _g = ENV_LOCK.lock().unwrap();
    let _home = temp_home("accounts");

    // Fresh install → empty stores.
    assert!(AccountDirectory::load().is_empty());

    let mut pane = settings::panes::accounts::AccountsPane::new()
        .with_account_tester(std::sync::Arc::new(AcceptAccount));
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    pane.sync(&mut doc, &root);

    // Open the Add-email form, fill it, and Save.
    let _ = doc.put(&root, settings::panes::accounts::ADD_MAIL_KEY, true);
    pane.sync(&mut doc, &root);
    pane.set_form_fields_for_test("Test User", "test@gmail.com", "test@gmail.com", "");
    pane.set_form_password_for_test("s3cret-pw");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(&mut doc, &root),
        "bounded account probe and disk transaction complete"
    );

    // The account is now in the shared on-disk store Mail loads, auto-configured.
    let dir = AccountDirectory::load();
    let views = dir.views_of(AccountKind::Mail);
    assert_eq!(
        views.len(),
        1,
        "Add persisted a real account to the shared store"
    );
    assert_eq!(views[0].id, "test@gmail.com");
    assert!(
        views[0].summary.contains("imap.gmail.com"),
        "IMAP host was auto-configured from the address, got summary {:?}",
        views[0].summary
    );
    let id = views[0].id.clone();

    // The password persisted to the encrypted vault (not the plaintext store)…
    let account_id = accounts::AccountId::from(id.clone());
    assert_eq!(
        accounts::SecretVault::open()
            .get(&account_id)
            .map(|s| s.password),
        Some("s3cret-pw".to_string()),
        "the password was sealed in the vault, keyed by account id"
    );
    // …and never leaked into the plaintext account store on disk.
    let store_json = std::fs::read_to_string(
        accounts::store_path().expect("a config root under the temp XDG_CONFIG_HOME"),
    )
    .expect("the shared store was written");
    assert!(
        !store_json.contains("s3cret-pw"),
        "the password must not appear in the plaintext store JSON"
    );
    accounts::AccountSyncReporter::new(account_id.clone())
        .succeeded()
        .expect("seed sync health");

    // Now request removal via the trash button's key. Destructive account
    // removal is deliberately a two-stage action, so exercise the same
    // explicit confirmation the UI requires.
    let remove_key = format!("settings.accounts.remove.{id}");
    let _ = doc.put(&root, remove_key.as_str(), true);
    pane.sync(&mut doc, &root);
    assert!(
        !AccountDirectory::load()
            .views_of(AccountKind::Mail)
            .is_empty(),
        "the first action only opens the removal confirmation"
    );
    let _ = doc.put(&root, settings::panes::accounts::REMOVE_CONFIRM_KEY, true);
    pane.sync(&mut doc, &root);

    // The directory now reports empty for the Email kind — the account is gone
    // from the shared store on disk.
    assert!(
        AccountDirectory::load()
            .views_of(AccountKind::Mail)
            .is_empty(),
        "Remove deleted the account from the shared store on disk"
    );
    // …and its sealed credential was dropped from the vault too.
    assert!(
        accounts::SecretVault::open().get(&account_id).is_none(),
        "Remove also purged the account's vault secret"
    );
    assert!(
        accounts::AccountSyncStatusStore::load()
            .get(&account_id)
            .is_none(),
        "Remove also purged the account's local sync-health record"
    );
}
