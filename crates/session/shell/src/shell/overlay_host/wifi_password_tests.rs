use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::quick_settings::{QsLevel, QsSection, WifiNetwork};
use crate::shell::state::WorkspaceTree;
use std::cell::RefCell;
use ui::widget::Container;

/// Records the join attempts and can be told to reject them, so the
/// failure branch is exercised without a radio.
#[derive(Default)]
struct JoinRecorder {
    attempts: RefCell<Vec<(String, Option<String>)>>,
    reject: bool,
}

impl crate::shell::quick_settings::actions::QsActions for JoinRecorder {
    fn set_brightness(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_volume(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_wifi(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_bluetooth(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.attempts
            .borrow_mut()
            .push((ssid.to_string(), password.map(str::to_string)));
        if self.reject {
            Err("Secrets were required, but not provided".to_string())
        } else {
            Ok(())
        }
    }
    fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
        Ok(())
    }
}

/// A host sitting on the Wi-Fi drill-in with one protected network
/// and one open one visible.
fn host_on_wifi_pane(
    reject: bool,
) -> (
    ShellOverlayHost,
    crate::shell::chrome_state::Chrome,
    std::rc::Rc<JoinRecorder>,
) {
    struct Shim(std::rc::Rc<JoinRecorder>);
    impl crate::shell::quick_settings::actions::QsActions for Shim {
        fn set_brightness(&self, v: f32) -> Result<(), String> {
            self.0.set_brightness(v)
        }
        fn set_volume(&self, v: f32) -> Result<(), String> {
            self.0.set_volume(v)
        }
        fn set_wifi(&self, v: bool) -> Result<(), String> {
            self.0.set_wifi(v)
        }
        fn set_bluetooth(&self, v: bool) -> Result<(), String> {
            self.0.set_bluetooth(v)
        }
        fn connect_wifi(&self, s: &str, p: Option<&str>) -> Result<(), String> {
            self.0.connect_wifi(s, p)
        }
        fn set_power_profile(&self, p: os_actions::PowerProfile) -> Result<(), String> {
            self.0.set_power_profile(p)
        }
    }

    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let rec = std::rc::Rc::new(JoinRecorder {
        attempts: RefCell::new(Vec::new()),
        reject,
    });
    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings.nav_stack = vec![QsLevel::Root];
        st.quick_settings.section = Some(QsSection::Wifi);
        st.quick_settings.wifi = true;
        st.quick_settings.radios.wifi_available = true;
        st.quick_settings.wifi_networks = vec![
            WifiNetwork {
                ssid: "Cafe WiFi".into(),
                signal: 3,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: false,
                known: false,
            },
            WifiNetwork {
                ssid: "Airport Free".into(),
                signal: 2,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
                known: false,
            },
        ];
    }
    let host = ShellOverlayHost::new(Box::new(Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(Shim(rec.clone())));
    (host, chrome, rec)
}

fn press(h: &mut ShellOverlayHost, key: &Key) -> bool {
    let event = ui::synthetic_input::key_input(key, winit::event::ElementState::Pressed);
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        h.route_wifi_password_event(&event, ctx)
    })
}

/// The prompt's passphrase field.
fn wifi_field(h: &ShellOverlayHost) -> &TextInput {
    h.wifi_password_field()
        .expect("the prompt has a passphrase field")
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

fn finish_join(
    host: &mut ShellOverlayHost,
    chrome: &crate::shell::chrome_state::Chrome,
    password: Option<&str>,
    shared_credential: bool,
    result: Result<(), &str>,
) {
    host.apply_wifi_join_result(
        &mut chrome.borrow_mut(),
        crate::shell::quick_settings::refresh::WifiJoinResult {
            ssid: "Cafe WiFi".into(),
            password: password.map(str::to_owned),
            shared_credential,
            result: result.map_err(str::to_owned),
        },
    );
}

#[test]
fn wifi_password_prompt_captures_typing_and_never_submits_empty() {
    fn a_protected_network_asks_for_a_password_instead_of_failing_silently() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        let prompt = chrome.borrow().quick_settings.wifi_password.clone();
        let prompt = prompt.expect("a prompt is up");
        assert_eq!(prompt.ssid, "Cafe WiFi");
        assert!(wifi_field(&h).value().is_empty());
        assert!(
            wifi_field(&h).is_secret() && !wifi_field(&h).is_revealed(),
            "the passphrase is a secret field, hidden until its own control reveals it"
        );
        assert!(wifi_field(&h).has_reveal_control());
        assert!(
            rec.attempts.borrow().is_empty(),
            "nothing is attempted until there is a password to attempt with"
        );
        assert!(
            !prompt.can_submit(wifi_field(&h).value()),
            "Connect stays inert until something is typed"
        );
    }

    fn typing_reaches_the_buffer_and_never_the_palette() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["h", "u", "n", "k"] {
            press(&mut h, &ch(c));
        }
        assert_eq!(wifi_field(&h).value(), "hunk");
        assert!(
            !format!("{:?}", chrome.borrow().quick_settings).contains("hunk"),
            "the passphrase never enters chrome state"
        );
        assert!(
            !h.palette_visible,
            "a 'k' in a password must be a character, not the search chord"
        );
    }

    fn no_prompt_means_keys_fall_through_to_the_shell() {
        let (mut h, _chrome, _) = host_on_wifi_pane(false);
        assert!(
            !press(&mut h, &ch("k")),
            "with no field up, the shell's own chords must still work"
        );
    }

    fn an_empty_password_is_never_submitted() {
        let (mut h, _chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &Key::Named(NamedKey::Enter));
        assert!(
            rec.attempts.borrow().is_empty(),
            "every WPA network rejects an empty passphrase; asking wastes a round trip"
        );
    }

    a_protected_network_asks_for_a_password_instead_of_failing_silently();
    typing_reaches_the_buffer_and_never_the_palette();
    no_prompt_means_keys_fall_through_to_the_shell();
    an_empty_password_is_never_submitted();
}

#[test]
fn wifi_password_join_is_nonblocking_and_shares_the_secret_through_the_bridge() {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow feeds both assertions right after; ending it earlier would mean \
    cloning test fixture data for no real gain"
    )]
    fn enter_starts_a_nonblocking_password_join_and_success_finishes_it() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["p", "w", "1"] {
            press(&mut h, &ch(c));
        }
        press(&mut h, &Key::Named(NamedKey::Enter));
        assert!(
            rec.attempts.borrow().is_empty(),
            "the input thread never calls nmcli"
        );
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .is_some_and(|prompt| prompt.connecting),
            "the prompt immediately exposes progress"
        );
        finish_join(&mut h, &chrome, Some("pw1"), false, Ok(()));
        let st = chrome.borrow();
        assert!(st.quick_settings.wifi_joining.is_none());
        assert!(
            st.quick_settings.wifi_password.is_none(),
            "a successful join closes the prompt"
        );
        assert!(
            st.quick_settings
                .wifi_networks
                .iter()
                .any(|n| n.ssid == "Cafe WiFi" && n.connected),
            "the list the user lands back on shows the network they just joined"
        );
    }

    fn successful_join_shares_the_password_through_the_secret_bridge() {
        let (mut host, chrome, _) = host_on_wifi_pane(false);
        let shared = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = shared.clone();
        host.set_wifi_secret_bridge(std::rc::Rc::new(move |action| match action {
            WifiSecretAction::Share { ssid, password, .. } => {
                observed.borrow_mut().push((ssid, password));
                Ok(None)
            }
            WifiSecretAction::Lookup { .. } => Ok(None),
        }));
        host.begin_wifi_password("Cafe WiFi");
        for character in ["p", "w", "1"] {
            press(&mut host, &ch(character));
        }
        press(&mut host, &Key::Named(NamedKey::Enter));
        finish_join(&mut host, &chrome, Some("pw1"), false, Ok(()));
        assert_eq!(
            shared.borrow().as_slice(),
            [("Cafe WiFi".to_string(), "pw1".to_string())]
        );
    }

    fn shared_password_connects_without_entering_the_visible_editor() {
        let (mut host, chrome, recorder) = host_on_wifi_pane(false);
        host.set_wifi_secret_bridge(std::rc::Rc::new(|action| match action {
            WifiSecretAction::Lookup { ssid, .. } if ssid == "Cafe WiFi" => {
                Ok(Some("from-another-device".into()))
            }
            WifiSecretAction::Lookup { .. } | WifiSecretAction::Share { .. } => Ok(None),
        }));
        host.begin_wifi_password("Cafe WiFi");
        assert!(host.try_connect_with_shared_wifi_password());
        assert!(recorder.attempts.borrow().is_empty());
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .connecting
        );
        finish_join(
            &mut host,
            &chrome,
            Some("from-another-device"),
            true,
            Ok(()),
        );
        assert!(chrome.borrow().quick_settings.wifi_password.is_none());
        assert!(chrome.borrow().quick_settings.wifi_joining.is_none());
    }

    enter_starts_a_nonblocking_password_join_and_success_finishes_it();
    successful_join_shares_the_password_through_the_secret_bridge();
    shared_password_connects_without_entering_the_visible_editor();
}

#[test]
fn wifi_password_rejection_and_cancel_keep_or_discard_state_correctly() {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow feeds a derived reference (`p`) used through the assertions right \
    after; ending it earlier would mean cloning test fixture data for no real gain"
    )]
    fn a_rejected_password_keeps_the_prompt_up_and_says_why() {
        let (mut h, chrome, _) = host_on_wifi_pane(true);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["b", "a", "d"] {
            press(&mut h, &ch(c));
        }
        press(&mut h, &Key::Named(NamedKey::Enter));
        finish_join(&mut h, &chrome, Some("bad"), false, Err("rejected"));
        let st = chrome.borrow();
        let p = st
            .quick_settings
            .wifi_password
            .as_ref()
            .expect("the prompt survives a rejection");
        assert!(st.quick_settings.wifi_joining.is_none());
        assert!(!p.connecting, "the attempt is over");
        assert!(p.error.is_some(), "and the reason is on screen");
        assert_eq!(
            wifi_field(&h).value(),
            "bad",
            "a typo is a correction, not a retype"
        );
        assert_eq!(
            wifi_field(&h).selection_range_public(),
            Some((0, 3)),
            "and it is selected, so the next keystroke replaces it"
        );
    }

    fn typing_after_a_rejection_clears_the_stale_error() {
        let (mut h, chrome, _) = host_on_wifi_pane(true);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("x"));
        press(&mut h, &Key::Named(NamedKey::Enter));
        finish_join(&mut h, &chrome, Some("x"), false, Err("rejected"));
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        press(&mut h, &ch("y"));
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .unwrap()
                .error
                .is_none(),
            "the message was about the old value"
        );
    }

    fn escape_backs_out_and_takes_the_password_with_it() {
        let (mut h, chrome, rec) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("s"));
        press(&mut h, &Key::Named(NamedKey::Escape));
        assert!(chrome.borrow().quick_settings.wifi_password.is_none());
        assert!(wifi_field(&h).value().is_empty());
        assert!(rec.attempts.borrow().is_empty());
    }

    fn closing_the_popover_discards_a_half_typed_password() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        press(&mut h, &ch("s"));
        chrome.borrow_mut().overlays.quick_settings = false;
        h.clear_stale_wifi_password();
        assert!(
            chrome.borrow().quick_settings.wifi_password.is_none(),
            "a password buffer must not outlive the window it was typed into"
        );
        assert!(wifi_field(&h).value().is_empty());
    }

    a_rejected_password_keeps_the_prompt_up_and_says_why();
    typing_after_a_rejection_clears_the_stale_error();
    escape_backs_out_and_takes_the_password_with_it();
    closing_the_popover_discards_a_half_typed_password();
}

fn find_node<'a>(
    node: &'a ui::a11y::AccessNode,
    matches: &dyn Fn(&ui::a11y::AccessNode) -> bool,
) -> Option<&'a ui::a11y::AccessNode> {
    if matches(node) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_node(child, matches))
}

fn act(h: &mut ShellOverlayHost, target: &str, action: &ui::a11y::Action) -> bool {
    Widget::perform_semantic_action(
        h,
        target,
        action,
        &mut ui::widget::Edits::lend(&mut automerge::AutoCommit::new()),
        &automerge::ROOT,
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn wifi_password_prompt_is_published_and_operable_through_accessibility() {
    use ui::a11y::{Action, Role};

    fn the_prompt_is_a_modal_dialog_with_a_protected_focused_field() {
        let (mut h, _chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        for c in ["s", "e", "c"] {
            press(&mut h, &ch(c));
        }
        let semantics = h.accessibility().expect("shell semantics");
        let dialog = find_node(&semantics, &|node| {
            node.role == Role::Dialog && node.name.as_deref() == Some("Cafe WiFi")
        })
        .expect("the prompt is published under its network's name");
        assert!(dialog.state.modal);
        let field = find_node(dialog, &|node| node.role == Role::TextInput)
            .expect("the password field is published");
        assert_eq!(field.name.as_deref(), Some("Password"));
        assert!(field.state.protected, "readers treat it as password text");
        assert!(field.state.focused, "the field has the keyboard");
        assert_eq!(
            field.value, None,
            "the typed password never leaves the shell"
        );
        let names: Vec<_> = dialog
            .children
            .iter()
            .filter(|node| node.role == Role::Button)
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert_eq!(names, ["Cancel", "Connect"]);

        let update = ui::accesskit_bridge::build_tree_update(
            &h,
            &std::collections::HashMap::new(),
            None,
            1.0,
        );
        let password = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::PasswordInput)
            .map(|(_, node)| node)
            .expect("AT-SPI sees password text");
        assert_eq!(password.value(), None);
        assert!(
            password.supports_action(ui::accesskit::Action::SetValue),
            "a reader can type into it"
        );
    }

    fn set_value_and_connect_reach_the_join() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        let semantics = h.accessibility().expect("shell semantics");
        let target = |name: &str| {
            find_node(&semantics, &|node| {
                node.name.as_deref() == Some(name) && node.action_target.is_some()
            })
            .and_then(|node| node.action_target.clone())
            .expect("an operable node")
        };
        let (field, connect) = (target("Password"), target("Connect"));
        assert!(act(&mut h, &field, &Action::SetValue("hunter22".into())));
        assert_eq!(wifi_field(&h).value(), "hunter22");
        assert!(act(&mut h, &connect, &Action::Activate));
        assert!(
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .is_some_and(|prompt| prompt.connecting),
            "Connect through AT-SPI starts the same join Enter does"
        );
    }

    fn a_target_for_another_network_changes_nothing() {
        let (mut h, chrome, _) = host_on_wifi_pane(false);
        h.begin_wifi_password("Cafe WiFi");
        let semantics = h.accessibility().expect("shell semantics");
        let cancel = find_node(&semantics, &|node| {
            node.name.as_deref() == Some("Cancel") && node.action_target.is_some()
        })
        .and_then(|node| node.action_target.clone())
        .expect("Cancel is operable");
        h.cancel_wifi_password();
        h.begin_wifi_password("Airport Free");
        assert!(
            act(&mut h, &cancel, &Action::Activate),
            "a stale target is absorbed, never clicked through"
        );
        assert!(
            chrome.borrow().quick_settings.wifi_password.is_some(),
            "the prompt for another network stays up"
        );
        let semantics = h.accessibility().expect("shell semantics");
        let cancel = find_node(&semantics, &|node| {
            node.name.as_deref() == Some("Cancel") && node.action_target.is_some()
        })
        .and_then(|node| node.action_target.clone())
        .expect("Cancel is operable");
        assert!(act(&mut h, &cancel, &Action::Activate));
        assert!(chrome.borrow().quick_settings.wifi_password.is_none());
    }

    fn the_network_list_is_published_while_no_prompt_is_up() {
        let (h, _chrome, _) = host_on_wifi_pane(false);
        let semantics = h.accessibility().expect("shell semantics");
        assert!(
            find_node(&semantics, &|node| node.name.as_deref()
                == Some("Cafe WiFi")
                && node.role == Role::Button)
            .is_some(),
            "every Quick Settings page publishes, including the Wi-Fi list"
        );
    }

    the_prompt_is_a_modal_dialog_with_a_protected_focused_field();
    set_value_and_connect_reach_the_join();
    a_target_for_another_network_changes_nothing();
    the_network_list_is_published_while_no_prompt_is_up();
}

#[test]
fn wifi_secret_lookup_uses_cached_security() {
    let (mut host, chrome, _) = host_on_wifi_pane(false);
    for network in &mut chrome.borrow_mut().quick_settings.wifi_networks {
        if network.ssid == "Cafe WiFi" {
            network.security = os_actions::WifiSecurity::Wpa3;
        }
    }
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let observed = seen.clone();
    host.set_wifi_secret_bridge(std::rc::Rc::new(move |action| {
        if let WifiSecretAction::Lookup { ssid, security } = action {
            observed.borrow_mut().push((ssid, security));
        }
        Ok(None)
    }));

    host.begin_wifi_password("Cafe WiFi");
    assert!(
        !host.try_connect_with_shared_wifi_password(),
        "no device shared this network"
    );
    assert_eq!(
        seen.borrow().as_slice(),
        [("Cafe WiFi".to_string(), os_actions::WifiSecurity::Wpa3)],
        "the bridge is told the mode the scan listed, and scans nothing itself"
    );

    host.begin_wifi_password("Hidden Network");
    assert!(!host.try_connect_with_shared_wifi_password());
    assert_eq!(
        seen.borrow().len(),
        1,
        "a network the scan did not list is not looked up"
    );
}

#[test]
fn wifi_secret_share_uses_cached_security() {
    let (mut host, chrome, _) = host_on_wifi_pane(false);
    let shared = std::rc::Rc::new(RefCell::new(Vec::new()));
    let observed = shared.clone();
    host.set_wifi_secret_bridge(std::rc::Rc::new(move |action| {
        if let WifiSecretAction::Share { ssid, security, .. } = action {
            observed.borrow_mut().push((ssid, security));
        }
        Ok(None)
    }));
    host.begin_wifi_password("Cafe WiFi");
    for character in ["p", "w", "1"] {
        press(&mut host, &ch(character));
    }
    press(&mut host, &Key::Named(NamedKey::Enter));
    finish_join(&mut host, &chrome, Some("pw1"), false, Ok(()));
    assert_eq!(
        shared.borrow().as_slice(),
        [("Cafe WiFi".to_string(), os_actions::WifiSecurity::Wpa2)],
        "the joined network's mode comes from the scan it was picked from"
    );
}
