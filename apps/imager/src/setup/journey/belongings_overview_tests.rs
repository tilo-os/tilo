use super::super::{
    super::strings::{t, t_args},
    InstallerPreview, Page,
};
use super::*;
use ui::{theme::Theme, widget::Widget};

fn visible_text(widget: &dyn Widget) -> String {
    // ListRow exposes its title as the name and its visible subtitle as the
    // value. Inspect both (and descriptions), including for privacy assertions.
    let mut text = widget
        .accessibility()
        .map(|node| {
            [node.name, node.value, node.description]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    for child in widget.children() {
        text.push('\n');
        text.push_str(&visible_text(child));
    }
    text
}

fn preview(config_text: &str) -> (tempfile::TempDir, InstallerPreview) {
    let fixture = tempfile::tempdir().unwrap();
    let config = fixture.path().join(".config");
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::write(config.join("Nextcloud/nextcloud.cfg"), config_text).unwrap();
    let roots = account_transfer::SourceRoots {
        home: fixture.path().into(),
        config,
    };
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.page = Page::Belongings;
    preview.journey.migrate = Some(true);
    preview.journey.categories[2] = true;
    preview.journey.details = false;
    preview.belongings.accounts = account_transfer::discover(&roots);
    (fixture, preview)
}

#[test]
fn metadata_uuid_is_absent_from_default_overview_but_provider_and_count_remain() {
    let (_fixture, mut preview) = preview(
        "[Accounts]\n0\\url=https://cloud.example.test\n0\\user=5e9af371-82ab-4022-964b-7f52ae345abb@cloud.example.test\n",
    );
    assert_eq!(preview.belongings.accounts.candidates.len(), 1);
    let candidate = &preview.belongings.accounts.candidates[0];
    assert_eq!(candidate.provider, account_transfer::Provider::Nextcloud);
    assert!(super::super::accounts::candidate_label(candidate).contains("5e9af371"));
    let before = preview.accounts.projection();
    let text = visible_text(preview.composition(&Theme::dark()).as_ref());
    // Format the independently known fixture count through Fluent without
    // normalizing the production text (including any interpolation isolation).
    let expected = t_args(
        "installer-belongings-connections-found",
        &[
            ("provider", t("installer-accounts-nextcloud").into()),
            ("count", 1_i64.into()),
        ],
    );
    assert!(
        text.contains(&expected),
        "expected {expected:?} in {text:?}"
    );
    assert!(!text.contains("5e9af371"));
    assert!(!text.contains("cloud.example.test"));
    assert_eq!(preview.accounts.projection(), before);
    assert!(preview.accounts.prepared_handle().is_none());

    // The expanded review still identifies the source; no formatter or grant
    // checks on the account-specific route have changed.
    preview.journey.details = true;
    let detail = visible_text(preview.composition(&Theme::dark()).as_ref());
    assert!(detail.contains("5e9af371"));
}

#[test]
fn multiple_saved_accounts_have_deterministic_provider_counts_and_keep_detail_identity() {
    let (_fixture, mut preview) = preview(
        "[Accounts]\n0\\url=https://cloud.example.test\n0\\user=ada@example.test\n1\\url=https://work.example.test\n1\\user=work@example.test\n",
    );
    assert_eq!(preview.belongings.accounts.candidates.len(), 2);
    assert!(
        preview
            .belongings
            .accounts
            .candidates
            .iter()
            .all(|candidate| { candidate.provider == account_transfer::Provider::Nextcloud })
    );
    let mut dropbox = preview.belongings.accounts.candidates[0].clone();
    dropbox.provider = account_transfer::Provider::Dropbox;
    dropbox.label = "Private source name".into();
    preview.belongings.accounts.candidates.push(dropbox);
    // The provider/count pairs and their order come from this fixture, not the
    // overview helper's grouping or provider mapping.
    let expected = [
        ("installer-accounts-dropbox", 1_i64),
        ("installer-accounts-nextcloud", 2_i64),
    ]
    .map(|(provider, count)| {
        t_args(
            "installer-belongings-connections-found",
            &[("provider", t(provider).into()), ("count", count.into())],
        )
    })
    .join(", ");
    assert_eq!(preview.belongings.accounts_overview_hint(), expected);
    let text = visible_text(preview.composition(&Theme::light()).as_ref());
    assert!(
        text.contains(&expected),
        "expected {expected:?} in {text:?}"
    );
    for private in [
        "ada@example.test",
        "work@example.test",
        "Private source name",
    ] {
        assert!(!text.contains(private));
    }
    preview.belongings.accounts.candidates.reverse();
    assert_eq!(preview.belongings.accounts_overview_hint(), expected);
    preview.journey.details = true;
    let detail = visible_text(preview.composition(&Theme::light()).as_ref());
    assert!(detail.contains("ada@example.test"));
    assert!(detail.contains("work@example.test"));
}
