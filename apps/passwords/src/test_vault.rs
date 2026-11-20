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

pub(crate) fn personal_window()
-> Result<ui::prelude::TestApp<crate::PasswordsShell>, ui::launch::LaunchError> {
    let person = "ad".repeat(32);
    let workspace = ui::resource::personal_cluster_of(&person).expect("test person's cluster");
    ui::prelude::TestApp::for_person_in(&person, &workspace.to_string())
}
