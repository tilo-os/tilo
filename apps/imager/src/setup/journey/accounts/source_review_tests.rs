use super::*;

fn visible_text(widget: &dyn ui::widget::Widget) -> String {
    let mut text = widget
        .accessibility()
        .and_then(|node| node.name)
        .unwrap_or_default();
    for child in widget.children() {
        text.push_str(&visible_text(child));
    }
    text
}

#[test]
fn linux_nextcloud_source_review_production_view_preserves_local_only_changes_warning() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let config = home.join(".config");
    let path = config.join("Nextcloud/nextcloud.cfg");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = "[Accounts]\n0\\url=https://cloud.example.test/base\n0\\http_user=ada\n0\\authType=http\n0\\Folders\\1\\localPath=/not-a-granted-sync-folder\n";
    std::fs::write(&path, original).unwrap();
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let candidate = inventory
        .candidates
        .iter()
        .find(|c| c.provider == Provider::Nextcloud)
        .unwrap()
        .clone();
    let mut accounts = Accounts::default();
    accounts.update_sources(roots, inventory).unwrap();
    let overview = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(visible_text(&overview).contains(&t("installer-accounts-nextcloud-linux-summary")));
    accounts
        .apply(AccountRequest::Source(candidate.id))
        .unwrap();
    let review = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    let text = visible_text(&review);
    for key in [
        "installer-accounts-nextcloud-linux-files",
        "installer-accounts-nextcloud-linux-local-changes",
        "installer-accounts-nextcloud-linux-dual-boot",
        "installer-accounts-nextcloud-linux-calendar",
    ] {
        assert!(text.contains(&t(key)), "missing production copy: {key}");
        assert!(
            accounts.projection()["source_review"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(t(key)))
        );
    }
    assert!(!text.contains("/not-a-granted-sync-folder"));
    assert!(accounts.stage.is_none());
    assert!(accounts.prepared.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    accounts.apply(AccountRequest::Back).unwrap();
    assert!(accounts.at_overview());
    // A manually added account must not be described as a discovered Linux sync client.
    accounts.inventory = Inventory::default();
    accounts
        .apply(AccountRequest::Provider("nextcloud".into()))
        .unwrap();
    assert!(accounts.source_review_keys().is_empty());
}
