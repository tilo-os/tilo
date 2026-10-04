use super::*;
use crate::model::demo_accounts;

fn opened() -> AddAccountPanel {
    let mut panel = AddAccountPanel::new();
    panel.open();
    panel
}

/// What a control of the sheet asks for, as the controls ask it.
fn ask(panel: &mut AddAccountPanel, msg: Msg) {
    panel.sheet.borrow_mut().queue.push_back(msg);
    panel.pump();
}

fn type_into(state: &LocalState<String>, text: &str) {
    state.set(text.to_owned());
}

/// A valid account the form saves.
fn filled() -> AddAccountPanel {
    let panel = opened();
    type_into(&panel.values.issuer, "Acme");
    type_into(&panel.values.label, "bob@acme.io");
    type_into(&panel.values.secret, "jbsw y3dp ehpk 3pxp");
    panel
}

/// What the sheet refuses to save, and where it says so.
#[test]
fn validation_names_the_problem_under_the_field_it_concerns() {
    // Case `no_secret_is_a_problem_with_the_secret_key`.
    {
        let mut panel = opened();
        type_into(&panel.values.issuer, "Acme");
        ask(&mut panel, Msg::Save);
        assert!(panel.is_open(), "an invalid form stays open");
        assert!(panel.take_event().is_none());
        assert_eq!(panel.problem().as_deref(), Some("Enter a secret key"));
        assert_eq!(
            panel.sheet.borrow().problem.as_ref().map(|p| p.field),
            Some(FieldId::Secret)
        );
    }

    // Case `a_secret_that_is_not_base32_is_refused`.
    {
        let mut panel = filled();
        type_into(&panel.values.secret, "not base32 !!");
        ask(&mut panel, Msg::Save);
        assert_eq!(
            panel.problem().as_deref(),
            Some("That secret isn\u{2019}t valid base32")
        );
    }

    // Case `an_account_name_that_is_a_key_is_refused_under_that_field`.
    {
        let mut panel = filled();
        type_into(&panel.values.label, "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP");
        ask(&mut panel, Msg::Save);
        assert_eq!(
            panel.problem().as_deref(),
            Some("Account names must not include a secret key")
        );
        assert_eq!(
            panel.sheet.borrow().problem.as_ref().map(|p| p.field),
            Some(FieldId::Label)
        );
    }

    // Case `an_account_needs_an_issuer_or_a_name`.
    {
        let mut panel = filled();
        type_into(&panel.values.issuer, "  ");
        type_into(&panel.values.label, "");
        ask(&mut panel, Msg::Save);
        assert_eq!(
            panel.problem().as_deref(),
            Some("Add an issuer or account name")
        );
    }

    // Case `fixing_the_problem_lets_the_save_through`.
    {
        let mut panel = opened();
        type_into(&panel.values.issuer, "Acme");
        ask(&mut panel, Msg::Save);
        assert!(panel.problem().is_some());
        type_into(&panel.values.secret, "JBSWY3DPEHPK3PXP");
        ask(&mut panel, Msg::Save);
        assert!(!panel.is_open());
        assert!(matches!(panel.take_event(), Some(AddPanelEvent::Save(_))));
    }
}

/// What a saved account carries.
#[test]
fn a_saved_account_carries_the_typed_details_and_advanced_choices() {
    // Case `the_secret_is_normalised_and_the_period_is_clamped`.
    {
        let mut panel = filled();
        type_into(&panel.values.period, "9999");
        ask(&mut panel, Msg::Save);
        let Some(AddPanelEvent::Save(account)) = panel.take_event() else {
            panic!("a valid form saves");
        };
        assert_eq!(account.issuer, "Acme");
        assert_eq!(account.label, "bob@acme.io");
        assert_eq!(
            account.secret, "JBSWY3DPEHPK3PXP",
            "spaces and case are normalised"
        );
        assert_eq!(account.period, 300, "the period stays in range");
        assert_eq!((account.algorithm, account.digits), (Algorithm::Sha1, 6));
    }

    // Case `the_algorithm_and_digit_segments_choose_the_parameters`.
    {
        let mut panel = filled();
        panel.values.algorithm.set(Algorithm::Sha512.index());
        panel.values.digits.set(1);
        ask(&mut panel, Msg::Save);
        let Some(AddPanelEvent::Save(account)) = panel.take_event() else {
            panic!("a valid form saves");
        };
        assert_eq!((account.algorithm, account.digits), (Algorithm::Sha512, 8));
    }

    // Case `closing_the_sheet_forgets_the_secret`.
    {
        let mut panel = filled();
        ask(&mut panel, Msg::Save);
        assert_eq!(
            Values::text(&panel.values.secret),
            "",
            "a saved form does not keep the key"
        );
        let mut panel = filled();
        ask(&mut panel, Msg::Cancel);
        assert!(matches!(panel.take_event(), Some(AddPanelEvent::Cancel)));
        assert_eq!(Values::text(&panel.values.secret), "");
    }
}

/// A setup link fills the form and shows what it carried.
#[test]
fn a_setup_link_fills_the_form() {
    // Case `a_complete_link_fills_every_field_and_collapses_the_link_row`.
    {
        let mut panel = opened();
        ask(&mut panel, Msg::ExpandLink);
        assert!(panel.sheet.borrow().uri_expanded, "the link field opens");
        type_into(
            &panel.values.uri,
            "otpauth://totp/GitHub:ada@example.com?secret=JBSWY3DPEHPK3PXP&issuer=GitHub",
        );
        ask(&mut panel, Msg::UriChanged);
        assert_eq!(
            panel.field_snapshot(),
            (
                "GitHub".to_owned(),
                "ada@example.com".to_owned(),
                "JBSWY3DPEHPK3PXP".to_owned()
            )
        );
        assert!(!panel.sheet.borrow().uri_expanded, "the link row collapses");
        assert_eq!(Values::text(&panel.values.uri), "", "the link is not kept");
    }

    // Case `a_link_with_non_default_parameters_opens_advanced`.
    {
        let mut panel = opened();
        panel.apply_uri(
            "otpauth://totp/AWS:ada?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=60",
        );
        assert!(
            panel.sheet.borrow().advanced_open,
            "what the link carried shows"
        );
        assert_eq!(
            panel.values.algorithm.with(|a| *a),
            Algorithm::Sha256.index()
        );
        assert_eq!(panel.values.digits.with(|d| *d), 1);
        assert_eq!(Values::text(&panel.values.period), "60");
    }

    // Case `a_broken_link_says_why_and_a_half_typed_one_stays_quiet`.
    {
        let mut panel = opened();
        ask(&mut panel, Msg::ExpandLink);
        type_into(&panel.values.uri, "otpauth://totp/x?issuer=y");
        ask(&mut panel, Msg::UriChanged);
        assert!(
            panel.problem().is_some_and(|p| p.contains("secret")),
            "a link without a key is refused"
        );
        let mut panel = opened();
        type_into(&panel.values.uri, "otpa");
        ask(&mut panel, Msg::UriChanged);
        assert!(
            panel.problem().is_none(),
            "text that is not yet a link is left alone"
        );
    }
}

/// The sheet's three flows.
#[test]
fn add_edit_and_set_up_are_three_flows_of_one_sheet() {
    let account = demo_accounts().remove(2);

    // Case `edit_loads_the_account_and_shows_non_default_parameters`.
    {
        let mut panel = AddAccountPanel::new();
        panel.open_for_edit(&account);
        assert_eq!(panel.flow(), Flow::Edit);
        assert_eq!(panel.field_snapshot().0, "AWS");
        assert!(
            panel.sheet.borrow().advanced_open,
            "AWS uses SHA256, so its parameters show"
        );
        ask(&mut panel, Msg::Save);
        let Some(AddPanelEvent::Edit(saved)) = panel.take_event() else {
            panic!("an edit saves as an edit");
        };
        assert_eq!(saved.id, account.id, "the identity is kept");
        assert_eq!(saved.algorithm, Algorithm::Sha256);
    }

    // Case `set_up_keeps_the_shared_identity_and_asks_only_for_the_key`.
    {
        let mut shared = account.clone();
        shared.secret.clear();
        let mut panel = AddAccountPanel::new();
        panel.open_for_provision(&shared);
        assert_eq!(panel.flow(), Flow::Provision);
        assert_eq!(panel.field_snapshot().0, "AWS");
        assert_eq!(
            panel.field_snapshot().2,
            "",
            "the key is not in the shared data"
        );
        type_into(&panel.values.secret, &account.secret);
        ask(&mut panel, Msg::Save);
        let Some(AddPanelEvent::Provision(saved)) = panel.take_event() else {
            panic!("a matching key installs");
        };
        assert_eq!(saved.id, shared.id);
        assert_eq!(saved.color, shared.color);
        assert_eq!(saved.order, shared.order);
    }

    // Case `a_key_for_a_different_account_is_refused`.
    {
        let mut shared = account.clone();
        shared.secret.clear();
        let mut panel = AddAccountPanel::new();
        panel.open_for_provision(&shared);
        panel.apply_uri("otpauth://totp/Other:eve?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256");
        ask(&mut panel, Msg::Save);
        assert!(panel.is_open(), "a mismatch stays open");
        assert!(
            panel
                .problem()
                .is_some_and(|p| p.contains("different account")),
            "the sheet says the link names another account"
        );
    }

    // Case `settings_that_differ_from_the_shared_ones_are_refused`.
    {
        let mut shared = account;
        shared.secret.clear();
        let mut panel = AddAccountPanel::new();
        panel.open_for_provision(&shared);
        type_into(&panel.values.secret, "JBSWY3DPEHPK3PXP");
        panel.values.digits.set(1);
        ask(&mut panel, Msg::Save);
        assert!(
            panel
                .problem()
                .is_some_and(|p| p.contains("different TOTP settings"))
        );
    }
}

/// Opening starts clean, and the open sheet is a modal dialog in the tree.
#[test]
fn opening_starts_clean_and_mounts_a_modal_dialog() {
    // Case `each_visit_starts_from_defaults`.
    {
        let mut panel = filled();
        ask(&mut panel, Msg::ToggleAdvanced);
        panel.open();
        assert_eq!(
            panel.field_snapshot(),
            (String::new(), String::new(), String::new())
        );
        let sheet = panel.sheet.borrow();
        assert!(!sheet.advanced_open && !sheet.uri_expanded && sheet.problem.is_none());
    }

    // Case `the_closed_sheet_mounts_nothing`.
    {
        let mut panel = AddAccountPanel::new();
        assert!(!panel.is_open());
        assert!(panel.widget().is_none());
        panel.open();
        assert!(panel.is_open() && panel.widget().is_some());
        ask(&mut panel, Msg::Cancel);
        assert!(!panel.is_open() && panel.widget().is_none());
    }

    // Case `toggling_advanced_asks_the_window_to_lay_the_sheet_out_again`.
    {
        let mut panel = opened();
        assert!(panel.take_structure_dirty(), "a new sheet is a new layout");
        assert!(!panel.take_structure_dirty());
        ask(&mut panel, Msg::ToggleAdvanced);
        assert!(panel.sheet.borrow().advanced_open);
        assert!(panel.take_structure_dirty(), "a section opened");
    }

    // Case `picking_an_image_leaves_the_sheet_open_for_the_app_to_answer`.
    {
        let mut panel = opened();
        ask(&mut panel, Msg::PickImage);
        assert!(panel.is_open());
        assert!(matches!(
            panel.take_event(),
            Some(AddPanelEvent::PickImageForQr)
        ));
    }
}
