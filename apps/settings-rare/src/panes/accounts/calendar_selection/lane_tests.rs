//! Vault reads must not hold the form or accept a superseded review.
use super::tests::reviewed_nextcloud;
use crate::accounts::AccountKind;
use crate::panes::accounts::AccountViewCommand;

#[test]
fn a_pending_calendar_vault_read_does_not_block_cancel_or_accept_changed_scope() {
    for change in ["cancel", "switch", "scope", "credential"] {
        let (_scratch, mut pane) = reviewed_nextcloud(serde_json::json!({"use_files": true}));
        let root = pane.account_root.clone();
        let (entered, started) = crate::channel::sync_channel(1);
        let (release, released) = crate::channel::sync_channel(1);
        let released = std::sync::Mutex::new(released);
        pane.account_root = root.clone().with_vault_gate(move || {
            entered.send(()).unwrap();
            released.lock().unwrap().recv().unwrap();
        });
        pane.save_calendar_selection();
        // Saving returned while the vault worker is still blocked.
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let id = accounts::AccountId::from("cloud");
        match change {
            "cancel" => pane.command(AccountViewCommand::Cancel),
            "switch" => pane.command(AccountViewCommand::Add(AccountKind::WebDav)),
            "scope" => {
                let mut store = root.store();
                let mut account = store.get(&id).unwrap().clone();
                account.config["use_files"] = false.into();
                store.upsert(account);
                root.save_store(&store).unwrap();
            }
            "credential" => {
                let mut vault = root.vault();
                vault.put(&id, &accounts::Secret::password("replacement-password"));
                vault.try_save().unwrap();
            }
            other => panic!("no change named {other}"),
        }
        let before = root.store().get(&id).cloned();
        release.send(()).unwrap();
        pane.settle_lane();
        assert_eq!(root.store().get(&id), before.as_ref(), "{change}");
        assert!(
            root.store()
                .get(&id)
                .unwrap()
                .config
                .get("calendars")
                .is_none()
        );
        if change == "switch" {
            assert_eq!(pane.form_kind, Some(AccountKind::WebDav));
            assert!(pane.form_error.is_none());
        }
    }
}
