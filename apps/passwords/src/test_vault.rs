use crate::PasswordsShell;
use crate::workspace_vault::WORKSPACE_VAULT_DOCUMENT;
use ui::prelude::TestApp;

pub(crate) struct TestVault {
    _home: tempfile::TempDir,
    _profile: tilo_paths::ProfileScope,
    _secrets: ui::secrets::SecretsScope,
}

pub(crate) fn isolated() -> TestVault {
    let home = tempfile::tempdir().unwrap();
    let profile = tilo_paths::scope_profile(home.path());
    let secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    TestVault {
        _home: home,
        _profile: profile,
        _secrets: secrets,
    }
}

/// Hold the workspace vault document's own keys in `workspace`, as a device that keeps the
/// workspace itself does: a test profile has no identity service to deliver them, and the
/// vault is sealed under them.
///
/// The keys belong to the workspace's document, not to a profile: the tests that open
/// windows of one workspace share them, and the first to ask mints them. Sealing is
/// serialised so two tests never mint different keys for the same document, which would
/// make each other's entries unreadable.
fn hold_vault_keys(workspace: &str) {
    assert!(
        ui::collaboration::document_content::hold_local_document_keys(
            workspace,
            <PasswordsShell as ui::framework::ResourceApp>::ID,
            WORKSPACE_VAULT_DOCUMENT,
        ),
        "the test mints the workspace vault's keys"
    );
}

/// A workspace the windows of these tests share, and the person's own desktop is not.
pub(crate) const WORKSPACE: &str = "6f0d7f0e-5a4b-4f6a-9f0a-2f3f6a0b9c11";

pub(crate) fn personal_window() -> Result<TestApp<PasswordsShell>, ui::launch::LaunchError> {
    let person = "ad".repeat(32);
    let workspace =
        ui::resource::person_workspace_of(&person).expect("test person's own workspace");
    hold_vault_keys(&workspace.to_string());
    TestApp::for_person_in(&person, &workspace.to_string())
}

/// The window in a workspace of its own, shared by these tests: what a member sees.
pub(crate) fn workspace_window() -> Result<TestApp<PasswordsShell>, ui::launch::LaunchError> {
    hold_vault_keys(WORKSPACE);
    TestApp::in_workspace(WORKSPACE)
}

/// The window of `person` in the shared test workspace, with `owner` lent to it as the
/// session lends this device's credential vault.
pub(crate) fn workspace_window_with_owner(
    person: &str,
    owner: accounts::CredentialOwner,
) -> Result<TestApp<PasswordsShell>, ui::launch::LaunchError> {
    hold_vault_keys(ui::framework::testing::TEST_WORKSPACE);
    TestApp::for_person_with_lent(person, owner)
}

/// Save `item` in the workspace vault the window `app` shows, as the person does, and let
/// the window show it.
pub(crate) fn seed(
    app: &mut TestApp<PasswordsShell>,
    item: &mut accounts::SecretItem,
) -> accounts::SecretId {
    let id = {
        let mut core = app.surface_mut::<crate::PasswordsWindow>().core_mut();
        let id = core
            .model
            .as_mut()
            .expect("the workspace vault is mounted")
            .save_item(item)
            .expect("the item is saved");
        core.selected = Some(id.clone());
        core.touch();
        id
    };
    app.settle();
    id
}

/// The items the window `app` shows, as the vault holds them. The window's record is made
/// durable first, so a conversion waiting for it finishes here.
pub(crate) fn stored(app: &mut TestApp<PasswordsShell>) -> Vec<accounts::SecretItem> {
    app.flush().expect("the workspace's records are kept");
    app.settle();
    let core = app.surface_mut::<crate::PasswordsWindow>().core_mut();
    core.model
        .as_ref()
        .expect("the workspace vault is mounted")
        .items()
}
