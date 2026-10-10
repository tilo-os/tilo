use super::*;
use accounts::{SecretId, SecretItem};
use ui::automerge::{self, AutoCommit};
use zeroize::Zeroizing;

use crate::vault::{Dialog, DraftField, Request};

const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

fn temp_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tilo-passwords-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// Run `event` through the window as the host would.
fn send(app: &mut PasswordsSurface, doc: &mut AutoCommit, event: &WindowEvent) {
    ui::testing::Dispatch::new().doc(doc).send(app, event);
}

fn request(app: &mut PasswordsSurface, request: Request) {
    app.requests.borrow_mut().push(request);
    app.settle();
}

/// Every text a tree shows or reads out, flattened.
fn spoken(node: &ui::a11y::AccessNode) -> String {
    format!("{node:?}")
}

/// The text a resource tree makes public: ids, kinds, labels and payloads.
fn public_text(resources: &[ui::tilo_actions::Resource]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for resource in resources {
        let _ = writeln!(
            out,
            "{} {} {}",
            resource.id.0, resource.kind, resource.label
        );
        for action in &resource.actions {
            out.push_str(&action.label);
            if let Ok(encoded) =
                action.build_args(Vec::<(String, ui::tilo_actions::ArgValue)>::new())
            {
                out.push_str(&String::from_utf8_lossy(&encoded.payload));
            }
        }
        out.push_str(&public_text(&resource.children));
    }
    out
}

// Clipboard transport is injected at the host boundary, scoped to the test's
// thread, so these tests never touch the desktop clipboard.
struct TestClipboard {
    accepts: bool,
    text: std::sync::Mutex<Option<String>>,
}

impl ui::clipboard::ClipboardBackend for TestClipboard {
    fn set_text(&self, _target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
        if !self.accepts {
            return false;
        }
        *self.text.lock().unwrap() = Some(text.into());
        true
    }
    fn text(&self, _target: ui::clipboard::ClipboardTarget) -> Option<String> {
        self.text.lock().unwrap().clone()
    }
    fn name(&self) -> &'static str {
        "passwords-test-transport"
    }
}

fn clipboard_fixture(accepts: bool) -> ui::host::HostScope {
    let services =
        ui::host::HostServices::null().with_clipboard(std::sync::Arc::new(TestClipboard {
            accepts,
            text: std::sync::Mutex::new(Some("existing clipboard".into())),
        }));
    ui::host::enter(&services)
}

#[test]
fn the_window_is_the_shared_shell_and_follows_the_chosen_place() {
    let mut app = PasswordsSurface::demo();
    assert!(app.shell().has_sidebar());
    assert!(app.shell().has_overview(), "the vault shows its item list");

    // A tool page is the sidebar and the content: no item list beside it.
    request(&mut app, Request::Navigate("review".into()));
    assert_eq!(app.core().screen, Screen::Review);
    assert!(!app.shell().has_overview());

    // Choosing a place of the vault brings the list back, filtered.
    request(&mut app, Request::Navigate("filter:wifi".into()));
    assert!(app.shell().has_overview());
    let core = app.core();
    assert_eq!(core.filter, VaultFilter::Kind(SecretItemKind::Wifi));
    assert_eq!(core.visible().len(), 1);
    assert_eq!(
        core.selected_item().map(|item| item.title.as_str()),
        Some("Home Wi-Fi"),
        "the first item of the place opens"
    );
    drop(core);

    // The sidebar names every place in sentence case with its count.
    let sections = app.core().nav_sections();
    assert_eq!(sections.len(), 2);
    let sidebar = format!("{sections:?}");
    for label in [
        "All items",
        "Favorites",
        "Password review",
        "Import and export",
    ] {
        assert!(sidebar.contains(label), "{label} is in the sidebar");
    }
}

#[test]
fn list_rows_open_items_and_toggle_favorites_through_the_list() {
    let mut doc = AutoCommit::new();
    let mut app = PasswordsSurface::demo();
    let mut pane = ListPane::new(&app.core, &app.requests);
    assert!(pane.perform_semantic_action(
        "passwords.item:demo-mail",
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    app.settle();
    assert_eq!(
        app.core().selected.as_ref().map(|id| id.0.as_str()),
        Some("demo-mail")
    );

    let before = app.core().count(VaultFilter::Favorites);
    request(
        &mut app,
        Request::ToggleFavorite(SecretId("demo-mail".into())),
    );
    assert_eq!(app.core().count(VaultFilter::Favorites), before + 1);

    // The arrow keys step through the list.
    app.core_mut().step(true);
    assert_eq!(
        app.core().selected.as_ref().map(|id| id.0.as_str()),
        Some("demo-recovery")
    );
}

#[test]
fn nothing_the_window_publishes_or_reads_out_holds_a_secret() {
    let app = PasswordsSurface::demo();
    let secrets = ["Harbor-Lights-2019", "harbourlights", "4821-7730", "tv1234"];

    let rows: Vec<_> = app
        .item_rows()
        .into_iter()
        .map(ui::framework::LiveRow::into_resource)
        .collect();
    let resources = public_text(&rows);
    let hits = format!("{:?}", app.search_entries());
    for secret in secrets {
        assert!(!resources.contains(secret), "resources leak {secret}");
        assert!(!hits.contains(secret), "search leaks {secret}");
    }

    // Every page the content shows, with the vault's own items open.
    let shared = Rc::clone(&app.core);
    for screen in [
        Screen::Vault,
        Screen::Editor,
        Screen::Review,
        Screen::AppAccess,
        Screen::Transfer,
    ] {
        shared.borrow_mut().screen = screen;
        let page = crate::pages::content(&shared.borrow(), &shared, &app.requests);
        let tree = page
            .accessibility()
            .map(|node| spoken(&node))
            .unwrap_or_default();
        for secret in secrets {
            assert!(!tree.contains(secret), "{screen:?} reads out {secret}");
        }
    }
}

#[test]
fn reveal_and_copy_are_temporary_and_honest_about_the_clipboard() {
    eprintln!("phase a revealed value expires, and locks when the window loses focus");
    {
        let mut doc = AutoCommit::new();
        let mut app = PasswordsSurface::demo();
        app.core_mut().revealed = Some((
            SecretId("demo-bank".into()),
            Zeroizing::new("temporary-value".into()),
            ui::time::now() + std::time::Duration::from_secs(10),
        ));
        assert!(Widget::next_wake_deadline(&app).is_some());
        send(&mut app, &mut doc, &WindowEvent::Focused(false));
        assert!(app.core().revealed.is_none());

        let deadline = ui::time::now();
        app.core_mut().revealed = Some((
            SecretId("demo-bank".into()),
            Zeroizing::new("fixture secret".into()),
            deadline,
        ));
        Widget::sync(&mut app, &doc, &automerge::ROOT);
        assert!(app.core().revealed.is_none());
    }

    eprintln!("phase a pending transfer keeps the host polling on a fixed deadline");
    {
        let doc = AutoCommit::new();
        let mut app = PasswordsSurface::demo();
        app.core_mut().toast = None;
        let (tx, rx) = std::sync::mpsc::channel();
        app.core_mut().export_portal = Some(rx);
        let first = app.core().next_wake().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(app.core().next_wake(), Some(first));
        tx.send(ui::portal::PortalOutcome::Cancelled).unwrap();
        Widget::sync(&mut app, &doc, &automerge::ROOT);
        assert!(app.core().next_wake().is_none());
    }

    eprintln!("phase copying and revealing are distinct actions");
    {
        let _clipboard = clipboard_fixture(true);
        let root = temp_root("secret-actions");
        let mut model = crate::PasswordsModel::open_in(&root);
        let mut item = SecretItem::login("Account", "ada", "private-value");
        item.add_origin("https://example.invalid").unwrap();
        model.save_item(&mut item).unwrap();
        let mut app = PasswordsSurface::new(model);

        request(&mut app, Request::CopySecret);
        assert_eq!(ui::clipboard_paste().as_deref(), Some("private-value"));
        assert!(
            app.core().revealed.is_none(),
            "copying must not disclose the value"
        );
        assert!(
            app.core()
                .toast
                .as_ref()
                .is_some_and(|(message, _)| message.starts_with("Copied password"))
        );

        request(&mut app, Request::RevealSecret);
        assert_eq!(
            app.core()
                .revealed
                .as_ref()
                .map(|(_, value, _)| value.to_string()),
            Some("private-value".into())
        );
        request(&mut app, Request::RevealSecret);
        assert!(app.core().revealed.is_none(), "the same action hides it");
        let _ = std::fs::remove_dir_all(root);
    }

    eprintln!("phase a failed copy says so and leaves the clipboard alone");
    {
        let _clipboard = clipboard_fixture(false);
        let root = temp_root("copy-failure");
        let mut model = crate::PasswordsModel::open_in(&root);
        let mut item = SecretItem::login("Account", "ada", "private-value");
        item.add_origin("https://example.invalid").unwrap();
        model.save_item(&mut item).unwrap();
        let mut app = PasswordsSurface::new(model);
        for copy in [
            Request::CopySecret,
            Request::CopyIdentity,
            Request::CopyOrigins,
        ] {
            app.core_mut().toast = None;
            request(&mut app, copy);
            assert_eq!(
                app.core()
                    .toast
                    .as_ref()
                    .map(|(message, _)| message.as_str()),
                Some("Could not copy to the clipboard. Try again.")
            );
            assert_eq!(ui::clipboard_paste().as_deref(), Some("existing clipboard"));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn filters_favorites_and_confirmed_deletes_are_real_vault_state() {
    let root = temp_root("delete");
    let mut model = crate::PasswordsModel::open_in(&root);
    let mut kept = SecretItem::login("Kept", "ada", "kept-secret-1A");
    let mut gone = SecretItem::login("Removed", "ada", "removed-secret-1A");
    model.save_item(&mut kept).unwrap();
    model.save_item(&mut gone).unwrap();
    let mut app = PasswordsSurface::new(model);

    request(&mut app, Request::Open(gone.id.clone()));
    request(&mut app, Request::ToggleFavorite(gone.id.clone()));
    request(&mut app, Request::Navigate("filter:favorites".into()));
    assert_eq!(app.core().visible().len(), 1);

    request(&mut app, Request::Delete);
    assert!(matches!(
        app.core().dialog,
        Some(Dialog::DeleteConfirm { ref title, .. }) if title == "Removed"
    ));
    assert_eq!(
        app.core().model.as_ref().unwrap().items().len(),
        2,
        "nothing is deleted before the person confirms"
    );
    request(&mut app, Request::CloseDialog);
    assert!(app.core().dialog.is_none());
    assert_eq!(app.core().model.as_ref().unwrap().items().len(), 2);

    request(&mut app, Request::Delete);
    request(&mut app, Request::ConfirmDelete);
    let remaining = app.core().model.as_ref().unwrap().items();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].title, "Kept");
    assert!(app.core().visible().is_empty(), "no favorite is left");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[expect(clippy::too_many_lines, reason = "merged cases, one per phase")]
fn editor_saves_edits_and_discards_drafts_against_the_real_store() {
    eprintln!("phase a new login is saved, then edited without retyping its password");
    {
        let _clipboard = clipboard_fixture(true);
        let root = temp_root("editor");
        let mut app = PasswordsSurface::new(crate::PasswordsModel::open_in(&root));
        request(&mut app, Request::NewItem);
        assert_eq!(app.core().screen, Screen::Editor);
        {
            let mut core = app.core_mut();
            core.set_draft(DraftField::Name, "Example account");
            core.set_draft(DraftField::Username, "ada@example.com");
            core.set_draft(
                DraftField::Origin,
                "https://example.com, https://login.example.com",
            );
            core.set_draft(DraftField::Password, "correct horse battery staple");
            core.set_draft(DraftField::Notes, "recovery details");
        }
        request(&mut app, Request::Save);
        {
            let core = app.core();
            assert_eq!(core.screen, Screen::Vault);
            assert!(core.draft_password.is_empty());
            assert!(core.draft_notes.is_empty());
            assert_eq!(core.items.len(), 1);
            assert_eq!(core.items[0].title, "Example account");
            assert_eq!(core.items[0].origins.len(), 2);
            assert!(
                core.items[0].password.is_empty(),
                "the cache holds no secret"
            );
        }

        request(&mut app, Request::Edit);
        assert!(
            app.core()
                .draft_origin
                .contains("https://login.example.com")
        );
        app.core_mut()
            .set_draft(DraftField::Name, "Renamed account");
        request(&mut app, Request::Save);
        let stored = app.core().model.as_ref().unwrap().items();
        assert_eq!(stored[0].title, "Renamed account");
        assert_eq!(stored[0].password, "correct horse battery staple");
        request(&mut app, Request::CopyOrigins);
        let copied = ui::clipboard_paste().unwrap();
        assert!(copied.contains("https://example.com"));
        assert!(copied.contains("https://login.example.com"));
        let _ = std::fs::remove_dir_all(root);
    }

    eprintln!("phase a draft names what is missing next to the field");
    {
        let mut app = PasswordsSurface::demo();
        request(&mut app, Request::NewItem);
        request(&mut app, Request::Save);
        assert!(matches!(
            app.core().draft_error,
            Some((DraftField::Name, _))
        ));
        app.core_mut().set_draft(DraftField::Name, "Router");
        request(&mut app, Request::SetKind(SecretItemKind::Wifi));
        request(&mut app, Request::Save);
        assert!(matches!(
            app.core().draft_error,
            Some((DraftField::Password, ref message)) if message == "Add the password"
        ));
        request(&mut app, Request::Generate);
        assert!(app.core().draft_password.len() >= 20);
        assert!(app.core().draft_error.is_none());
    }

    eprintln!("phase cancelling discards the sensitive draft");
    {
        let mut app = PasswordsSurface::demo();
        request(&mut app, Request::NewItem);
        app.core_mut()
            .set_draft(DraftField::Password, "unsaved password");
        app.core_mut()
            .set_draft(DraftField::Notes, "unsaved private note");
        request(&mut app, Request::CancelEdit);
        let core = app.core();
        assert_eq!(core.screen, Screen::Vault);
        assert!(core.draft_password.is_empty());
        assert!(core.draft_notes.is_empty());
    }

    eprintln!("phase editing keeps fields and tags the editor does not show");
    {
        let root = temp_root("custom-fields");
        let mut model = crate::PasswordsModel::open_in(&root);
        let mut item = SecretItem::login("Deployment", "ada", "token");
        item.kind = SecretItemKind::Other;
        item.fields.insert("client-id".into(), "app-42".into());
        item.fields.insert("tenant".into(), "northstar".into());
        item.tags.extend(["favorite".into(), "work".into()]);
        let id = model.save_item(&mut item).unwrap();

        let mut app = PasswordsSurface::new(model);
        request(&mut app, Request::Edit);
        assert!(
            app.core()
                .draft_custom_fields
                .contains("client-id = app-42")
        );
        assert_eq!(app.core().draft_tags, "work");
        {
            let mut core = app.core_mut();
            core.set_draft(DraftField::Name, "Production deployment");
            core.set_draft(
                DraftField::CustomFields,
                "client-id = app-43\nenvironment = production",
            );
            core.set_draft(DraftField::Tags, "work, infrastructure");
        }
        request(&mut app, Request::Save);

        let stored = crate::PasswordsModel::open_in(&root)
            .items()
            .into_iter()
            .find(|candidate| candidate.id == id)
            .unwrap();
        assert_eq!(
            stored.fields.get("client-id").map(String::as_str),
            Some("app-43")
        );
        assert_eq!(
            stored.fields.get("environment").map(String::as_str),
            Some("production")
        );
        assert!(!stored.fields.contains_key("tenant"));
        assert!(stored.tags.contains("favorite"));
        assert!(stored.tags.contains("infrastructure"));
        let _ = std::fs::remove_dir_all(root);
    }

    eprintln!("phase the custom field parser is helpful and rejects ambiguous rows");
    {
        use crate::vault::parse_custom_fields;
        assert_eq!(
            parse_custom_fields("Account = Ada\nRegion: eu-west")
                .unwrap()
                .len(),
            2
        );
        assert!(parse_custom_fields("missing separator").is_err());
        assert!(parse_custom_fields("Token = one\ntoken = two").is_err());
        assert!(parse_custom_fields("network = reserved").is_err());
        assert_eq!(
            parse_custom_fields("note = first\\nsecond").unwrap()[0].1,
            "first\nsecond"
        );
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "merged cases, one per phase")]
fn import_export_and_backup_flows_round_trip_through_portals() {
    eprintln!("phase an import previews conflicts before writing");
    {
        use std::io::Write;
        let root = temp_root("import-store");
        let csv_path = temp_root("import").with_extension("csv");
        let mut selected = std::fs::File::create(&csv_path).unwrap();
        selected
            .write_all(
                b"name,url,username,password\nExample,https://example.com,ada,first-secret\n",
            )
            .unwrap();
        drop(selected);
        let mut app = PasswordsSurface::new(crate::PasswordsModel::open_in(&root));
        let (services, recorder) = ui::host::HostServices::recording();
        for expected_conflicts in [0, 1] {
            recorder.respond_to_open_file(ui::portal::OpenFileOutcome::Opened(
                ui::portal::OpenedFile::new(
                    std::fs::File::open(&csv_path).unwrap(),
                    "passwords.csv",
                ),
            ));
            ui::host::scoped(&services, || request(&mut app, Request::Import));
            let deadline = ui::time::now() + HANG_BOUND;
            while ui::time::now() < deadline && app.core().dialog.is_none() {
                app.core_mut().poll_transfers();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(matches!(
                app.core().dialog,
                Some(Dialog::ImportPreview { ref rows, ref skipped })
                    if rows.len() == 1
                        && skipped.iter().filter(|skip| **skip).count() == expected_conflicts
            ));
            request(&mut app, Request::ConfirmImport);
            assert_eq!(app.core().model.as_ref().unwrap().items().len(), 1);
        }
        assert_eq!(recorder.pick_requests()[0].title(), "Import passwords");
        let _ = std::fs::remove_file(csv_path);
        let _ = std::fs::remove_dir_all(root);
    }

    eprintln!("phase a plaintext export needs the acknowledgement and round trips");
    {
        let root = temp_root("export-store");
        let out = temp_root("export");
        let mut model = crate::PasswordsModel::open_in(&root);
        model
            .save_login(
                "Example",
                "ada",
                "export-secret",
                Some("https://example.com"),
            )
            .unwrap();
        let mut app = PasswordsSurface::new(model);
        request(&mut app, Request::Export);
        request(&mut app, Request::ConfirmExport);
        assert!(app.core().export_portal.is_none());
        assert!(matches!(
            app.core().dialog,
            Some(Dialog::ExportPlaintext { reminded: true, .. })
        ));
        request(&mut app, Request::Acknowledge(true));
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(out.clone()));
        ui::host::scoped(&services, || {
            request(&mut app, Request::ConfirmExport);
        });
        assert_eq!(
            recorder.pick_requests()[0].title(),
            "Export passwords as CSV"
        );
        app.core_mut().poll_transfers();
        let written = out.with_extension("csv");
        let rows =
            crate::transfer::parse_browser_csv(std::fs::File::open(&written).unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].password.as_str(), "export-secret");
        let _ = std::fs::remove_file(written);
        let _ = std::fs::remove_dir_all(root);
    }

    eprintln!("phase an encrypted backup restores complete items");
    {
        let base = temp_root("backup-flow");
        let destination = base.join("portable-vault");
        std::fs::create_dir_all(&base).unwrap();
        let mut source = crate::PasswordsModel::open_in(&base.join("source"));
        let mut item = SecretItem::login("Studio", "ada", "private-token");
        item.fields.insert("workspace".into(), "design".into());
        item.tags.extend(["work".into(), "favorite".into()]);
        source.save_item(&mut item).unwrap();

        let mut app = PasswordsSurface::new(source);
        request(&mut app, Request::Backup);
        app.core_mut().backup_password = Zeroizing::new("a memorable backup password".into());
        app.core_mut().backup_confirmation = Zeroizing::new("a different password!".into());
        request(&mut app, Request::ConfirmBackup);
        assert!(matches!(
            app.core().dialog,
            Some(Dialog::Backup { error: Some(_) })
        ));
        app.core_mut().backup_confirmation = Zeroizing::new("a memorable backup password".into());
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
        ui::host::scoped(&services, || {
            request(&mut app, Request::ConfirmBackup);
        });
        assert_eq!(
            recorder.pick_requests()[0].title(),
            "Back up password vault"
        );
        app.core_mut().poll_transfers();
        let backup_path = destination.with_extension("tilopw");
        assert!(std::fs::metadata(&backup_path).unwrap().len() > 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(app.core().backup_password.is_empty());

        let mut restored =
            PasswordsSurface::new(crate::PasswordsModel::open_in(&base.join("restored")));
        recorder.respond_to_open_file(ui::portal::OpenFileOutcome::Opened(
            ui::portal::OpenedFile::new(
                std::fs::File::open(&backup_path).unwrap(),
                "portable-vault.tilopw",
            ),
        ));
        ui::host::scoped(&services, || {
            request(&mut restored, Request::Restore);
        });
        assert_eq!(
            recorder.pick_requests()[1].title(),
            "Restore password vault"
        );
        restored.core_mut().poll_transfers();
        assert!(matches!(
            restored.core().dialog,
            Some(Dialog::Restore { .. })
        ));
        restored.core_mut().backup_password = Zeroizing::new("a memorable backup password".into());
        request(&mut restored, Request::ConfirmRestore);
        let items = restored.core().model.as_ref().unwrap().items();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].fields.get("workspace").map(String::as_str),
            Some("design")
        );
        assert!(items[0].tags.contains("favorite"));
        assert!(restored.core().backup_password.is_empty());
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn every_capture_scenario_stages_on_the_fixture_vault() {
    for scenario in SCENARIOS.iter().copied().map(Some).chain([None]) {
        let root = temp_root("capture");
        let mut app = PasswordsSurface::new(crate::PasswordsModel::open_in(&root));
        app.stage_scenario(scenario);
        let core = app.core();
        assert!(
            core.model.is_none(),
            "{scenario:?} never shows a real vault"
        );
        let expected = match scenario {
            Some("editor" | "new-item" | "delete") => Screen::Editor,
            Some("review") => Screen::Review,
            Some("app-access") => Screen::AppAccess,
            Some("transfer" | "import" | "export" | "backup") => Screen::Transfer,
            _ => Screen::Vault,
        };
        assert_eq!(core.screen, expected, "{scenario:?}");
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }
}
