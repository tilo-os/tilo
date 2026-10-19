//! Installed bots as governed members: installed, updated, paused, adopted
//! and withdrawn only through governed changes, and never acting beyond
//! their installer or their grant.

use ed25519_dalek::SigningKey;
use tilo_action_policy::{ActionKind, Answer, KindSet, Level, Rule, TreePath};

use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};
use web_time::{Duration, SystemTime};

use crate::StatePeer;
use crate::installation::{AppearancePolicy, Installation, InstallationId};
use crate::state::delegates::GovernedDelegateRow;
use crate::workspace_membership::{
    BotDevice, BotInstall, BotRenewal, WorkspaceMembershipCommand as Command,
};

pub(super) const ROOT_SEED: [u8; 32] = [1; 32];
pub(crate) const BOT: &str = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";

pub(super) fn ns(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

pub(super) fn person() -> SigningKey {
    SigningKey::from_bytes(&ROOT_SEED)
}

pub(super) fn person_hex() -> String {
    hex::encode(person().verifying_key().to_bytes())
}

/// A bot device of `installer`: its key and a capability-free certificate.
pub(super) fn bot_device(installer: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: installer.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Bot: Discord Bridge".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, installer).unwrap();
    (key, cert)
}

pub(super) fn bot_delegation(
    installer: &SigningKey,
    device: [u8; 32],
    installation: &str,
    room: &str,
    now: SystemTime,
) -> Delegation {
    Delegation::sign(
        installer,
        device,
        Delegate::Bot {
            installation: installation.into(),
        },
        "Discord Bridge",
        [88, 101, 242],
        DelegateScope {
            rooms: vec![room.into()],
            apps: vec![],
        },
        ns(now.checked_sub(Duration::from_secs(1)).unwrap()),
        ns(now.checked_add(Duration::from_secs(3600)).unwrap()),
    )
    .unwrap()
}

pub(super) fn installation(program: &str, installed_by: &str) -> Installation {
    Installation {
        id: InstallationId::parse(BOT).unwrap(),
        program: program.into(),
        name: "Discord Bridge".into(),
        icon: None,
        installed_by: installed_by.into(),
        installed_at_ms: 1,
        appearance: AppearancePolicy::Own,
        source: None,
        armed_at: vec![],
        connector_cursors_from_ms: 1,
        inputs: std::collections::BTreeMap::default(),
        paused: false,
    }
}

#[test]
fn a_bot_row_acts_only_with_its_own_live_installation() {
    let (_, _, _, _, now) = crate::admission_tests::seeded();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let installer = person();
    let (_, cert) = bot_device(&installer, 8);
    let row = GovernedDelegateRow {
        delegation: bot_delegation(&installer, cert.device_public_key, BOT, room, now),
        withdrawn_at_ns: None,
        installation: Some(installation(&"a".repeat(64), &person_hex())),
    };
    assert_eq!(row.acting(&cert, room, ns(now)), Ok(()));

    let mut missing = row.clone();
    missing.installation = None;
    assert_eq!(
        missing.acting(&cert, room, ns(now)),
        Err(super::delegates::DelegateInactive::Mismatch)
    );

    let mut other = row.clone();
    other.delegation = bot_delegation(
        &installer,
        cert.device_public_key,
        "7f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
        room,
        now,
    );
    assert_eq!(
        other.acting(&cert, room, ns(now)),
        Err(super::delegates::DelegateInactive::Mismatch)
    );

    let mut adopted_elsewhere = row;
    adopted_elsewhere.installation = Some(installation(&"a".repeat(64), &"f".repeat(64)));
    assert_eq!(
        adopted_elsewhere.acting(&cert, room, ns(now)),
        Err(super::delegates::DelegateInactive::InstallerMismatch)
    );
}

fn chat_post() -> TreePath {
    TreePath::root()
        .any(Level::Room)
        .app("so.tilo.chat")
        .kind("chat.workspace")
        .any(Level::Resource)
        .action("post")
}

fn program(grant: Vec<Rule>) -> String {
    program_invoking(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
        grant,
    )
}

/// A one-step program invoking `action` on the first resource of its kind,
/// holding `grant`.
pub(crate) fn program_invoking(action: tilo_actions::ActionKey, grant: Vec<Rule>) -> String {
    let kind = action.kind.clone();
    let shortcut = tilo_automation::Shortcut {
        id: "bridge".into(),
        name: "Discord Bridge".into(),
        inputs: vec![],
        steps: vec![tilo_automation::Step::new(
            "act",
            tilo_automation::StepKind::Invoke {
                action,
                resource: tilo_automation::Selector::FirstOfKind(kind),
                args: std::collections::BTreeMap::default(),
                appearance: None,
            },
        )],
        output: None,
        grant,
        triggers: vec![],
        saved_by: None,
    };
    String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap()).unwrap()
}

fn post_rule() -> Rule {
    Rule::new(chat_post(), KindSet::READ_UNDOABLE, Answer::Allow)
}

fn install_command(
    peer: &StatePeer,
    program_text: &str,
    seed: u8,
    now: SystemTime,
) -> (Box<BotInstall>, DeviceCert) {
    let installer = person();
    let (_, cert) = bot_device(&installer, seed);
    let hash = tilo_automation::program::program_hash(program_text.as_bytes());
    let install = Box::new(BotInstall {
        installation: installation(&hash, &person_hex()),
        program: program_text.into(),
        device: Some(BotDevice {
            delegation: bot_delegation(&installer, cert.device_public_key, BOT, peer.room(), now),
            cert: cert.clone(),
        }),
    });
    (install, cert)
}

fn update_command(text: &str) -> Command {
    Command::InstallBot(Box::new(BotInstall {
        installation: installation(
            &tilo_automation::program::program_hash(text.as_bytes()),
            &person_hex(),
        ),
        program: text.into(),
        device: None,
    }))
}

fn post_in(peer: &StatePeer) -> TreePath {
    TreePath::room(peer.room())
        .app("so.tilo.chat")
        .kind("chat.workspace")
        .resource("chat:1")
        .action("post")
}

/// A grant of governance action `action` to the fixture's person.
fn governance_grant(peer: &StatePeer, id: &str, action: &str) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        id,
        tilo_action_policy::Subject::person(person_hex()),
        tilo_action_policy::roles::governance_action(peer.room(), action),
        KindSet::OUTSIDE,
        Answer::Allow,
        tilo_action_policy::Subject::person("another-person"),
    )
}

#[test]
fn the_owner_installs_a_bot_in_one_governed_change() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let text = program(vec![post_rule()]);
    let (install, cert) = install_command(&governor, &text, 8, now);
    let command = Command::InstallBot(install);
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &command,
            &authority,
            now
        ),
        Ok(true)
    );
    let bots = governor.governance().installations(now).unwrap();
    assert_eq!(bots.len(), 1);
    assert!(bots[0].acting);
    assert_eq!(bots[0].device_public_key, cert.device_public_key);
    assert_eq!(
        governor
            .governance()
            .bot_program(&bots[0].installation.program),
        Some(text)
    );
    assert_eq!(
        governor
            .governance()
            .installation(BOT, now)
            .map(|bot| bot.device_public_key),
        Some(cert.device_public_key)
    );
    let member = governor.permissions(cert.device_public_key, now).unwrap();
    assert!(member.allows(&post_in(&governor), ActionKind::Undoable, now));
    let notes = TreePath::room(governor.room())
        .app("so.tilo.notes")
        .action("append");
    assert!(!member.allows(&notes, ActionKind::Undoable, now));
    let grant = tilo_action_policy::roles::governance_action(
        governor.room(),
        tilo_action_policy::roles::governance::GRANT,
    );
    assert!(!member.allows(&grant, tilo_action_policy::roles::GOVERNANCE_KIND, now));
}

#[test]
fn install_refuses_a_delegation_for_another_installation_or_room() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let text = program(vec![post_rule()]);
    let (mut install, cert) = install_command(&governor, &text, 8, now);
    let installer = person();
    let other_room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    for delegation in [
        bot_delegation(
            &installer,
            cert.device_public_key,
            "7f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
            governor.room(),
            now,
        ),
        bot_delegation(&installer, cert.device_public_key, BOT, other_room, now),
        bot_delegation(&installer, [42; 32], BOT, governor.room(), now),
    ] {
        install.device.as_mut().unwrap().delegation = delegation;
        let command = Command::InstallBot(install.clone());
        assert!(
            governor
                .workspace_mut()
                .apply_workspace_membership_command(&owner_device, &command, &authority, now)
                .is_err()
        );
    }
    assert!(governor.governance().installations(now).unwrap().is_empty());
}

#[test]
fn install_refuses_a_program_with_unused_rules() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let delete = Rule::new(
        TreePath::root()
            .any(Level::Room)
            .app("so.tilo.chat")
            .kind("chat.workspace")
            .any(Level::Resource)
            .action("delete"),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    );
    let text = program(vec![post_rule(), delete]);
    let (install, _) = install_command(&governor, &text, 8, now);
    let command = Command::InstallBot(install);
    let refused = governor
        .workspace_mut()
        .apply_workspace_membership_command(&owner_device, &command, &authority, now)
        .unwrap_err();
    assert!(refused.contains("none of its steps"), "{refused}");
}

#[test]
fn install_refuses_a_program_that_does_not_match_its_hash() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let text = program(vec![post_rule()]);
    let (mut install, _) = install_command(&governor, &text, 8, now);
    install.installation.program = "b".repeat(64);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &owner_device,
                &Command::InstallBot(install),
                &authority,
                now
            )
            .is_err()
    );
}

#[test]
fn an_editor_cannot_install_by_default() {
    let (mut governor, authority, _, editor_device, now) = crate::admission_tests::seeded_editor();
    let text = program(vec![post_rule()]);
    let (install, _) = install_command(&governor, &text, 8, now);
    let command = Command::InstallBot(install);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&editor_device, &command, &authority, now)
            .is_err()
    );
    assert!(governor.governance().installations(now).unwrap().is_empty());
}

#[test]
fn an_update_wider_than_the_old_grant_needs_install_bot() {
    let (mut governor, authority, _, editor_device, now) = crate::admission_tests::seeded_editor();
    let install_grant = governance_grant(
        &governor,
        "install:editor",
        tilo_action_policy::roles::governance::INSTALL_BOT,
    );
    governor
        .governance_mut()
        .put_governed_grants(std::slice::from_ref(&install_grant), &authority, now)
        .unwrap();
    // A narrow program first: posting only read.
    let narrow = program(vec![Rule::new(chat_post(), KindSet::READ, Answer::Allow)]);
    let (install, _) = install_command(&governor, &narrow, 8, now);
    let command = Command::InstallBot(install);
    governor
        .workspace_mut()
        .apply_workspace_membership_command(&editor_device, &command, &authority, now)
        .unwrap();

    // The Editor keeps only pause-bot, then pushes the wider program.
    governor
        .governance_mut()
        .remove_governed_grants(std::slice::from_ref(&install_grant.id), &authority, now)
        .unwrap();
    let pause = governance_grant(
        &governor,
        "pause:editor",
        tilo_action_policy::roles::governance::PAUSE_BOT,
    );
    governor
        .governance_mut()
        .put_governed_grants(&[pause], &authority, now)
        .unwrap();
    let wide = program(vec![post_rule()]);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &editor_device,
                &update_command(&wide),
                &authority,
                now
            )
            .is_err(),
        "wider needs the privileged review"
    );
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &editor_device,
            &update_command(&narrow),
            &authority,
            now
        ),
        Ok(false),
        "the same grant is a no-op update anyone who may pause can apply"
    );
    governor
        .governance_mut()
        .put_governed_grants(&[install_grant], &authority, now)
        .unwrap();
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &editor_device,
                &update_command(&wide),
                &authority,
                now
            )
            .unwrap()
    );
    let bots = governor.governance().installations(now).unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(
        governor
            .governance()
            .bot_program(&bots[0].installation.program),
        Some(wide)
    );
    assert_eq!(
        governor
            .governance()
            .bot_program(&tilo_automation::program::program_hash(narrow.as_bytes())),
        None,
        "the replaced program is dropped"
    );
}

#[test]
fn a_bot_is_bounded_by_its_installers_current_authority() {
    let (mut governor, authority, _, editor_device, now) = crate::admission_tests::seeded_editor();
    let install_grant = governance_grant(
        &governor,
        "install:editor",
        tilo_action_policy::roles::governance::INSTALL_BOT,
    );
    governor
        .governance_mut()
        .put_governed_grants(&[install_grant], &authority, now)
        .unwrap();
    let text = program(vec![post_rule()]);
    let (install, cert) = install_command(&governor, &text, 8, now);
    let command = Command::InstallBot(install);
    governor
        .workspace_mut()
        .apply_workspace_membership_command(&editor_device, &command, &authority, now)
        .unwrap();
    let post = post_in(&governor);
    assert!(
        governor
            .permissions(cert.device_public_key, now)
            .unwrap()
            .allows(&post, ActionKind::Undoable, now)
    );
    // The installer loses posting; the bot loses it with them.
    let deny = tilo_action_policy::Grant::new(
        "no-post",
        tilo_action_policy::Subject::person(person_hex()),
        TreePath::room(governor.room()).app("so.tilo.chat"),
        KindSet::UNDOABLE,
        Answer::Deny,
        tilo_action_policy::Subject::person(person_hex()),
    );
    governor
        .governance_mut()
        .put_governed_grants(&[deny], &authority, now)
        .unwrap();
    let member = governor.permissions(cert.device_public_key, now).unwrap();
    assert!(!member.allows(&post, ActionKind::Undoable, now));
    assert!(member.allows(&post, ActionKind::Read, now));
}

#[test]
fn renewing_replaces_the_delegation_and_moving_withdraws_the_old_device() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let text = program(vec![post_rule()]);
    let (install, cert) = install_command(&governor, &text, 8, now);
    let command = Command::InstallBot(install);
    governor
        .workspace_mut()
        .apply_workspace_membership_command(&owner_device, &command, &authority, now)
        .unwrap();
    let later = now.checked_add(Duration::from_secs(60)).unwrap();
    let room = governor.room().to_owned();
    let renewal = |device: &DeviceCert| {
        Command::RenewBot(Box::new(BotRenewal {
            installation: InstallationId::parse(BOT).unwrap(),
            device: BotDevice {
                delegation: bot_delegation(&person(), device.device_public_key, BOT, &room, later),
                cert: device.clone(),
            },
        }))
    };
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&owner_device, &renewal(&cert), &authority, later)
            .unwrap()
    );
    let (_, moved) = bot_device(&person(), 9);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&owner_device, &renewal(&moved), &authority, later)
            .unwrap()
    );
    let rows = governor.governance().installations(later).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(
        |row| row.device_public_key == cert.device_public_key && row.withdrawn_at_ns.is_some()
    ));
    assert!(
        rows.iter()
            .any(|row| row.device_public_key == moved.device_public_key && row.acting)
    );
}

/// Install a bot running `program_text` in `peer` as the person of
/// `caller` (the fixture's root), with `appearance`; its device key and
/// certificate.
pub(crate) fn installed_bot(
    peer: &mut StatePeer,
    governor: &SigningKey,
    caller: &DeviceCert,
    program_text: &str,
    appearance: AppearancePolicy,
    now: SystemTime,
) -> (SigningKey, DeviceCert) {
    let installer = person();
    let (key, cert) = bot_device(&installer, 8);
    let mut row = installation(
        &tilo_automation::program::program_hash(program_text.as_bytes()),
        &person_hex(),
    );
    row.appearance = appearance;
    let command = Command::InstallBot(Box::new(BotInstall {
        installation: row,
        program: program_text.into(),
        device: Some(BotDevice {
            delegation: bot_delegation(&installer, cert.device_public_key, BOT, peer.room(), now),
            cert: cert.clone(),
        }),
    }));
    assert_eq!(
        peer.workspace_mut()
            .apply_workspace_membership_command(caller, &command, governor, now),
        Ok(true)
    );
    (key, cert)
}

fn installed(now: SystemTime) -> (StatePeer, SigningKey, DeviceCert, DeviceCert) {
    let (mut governor, authority, _, owner_device, _) = crate::admission_tests::seeded();
    let text = program(vec![post_rule()]);
    let (install, cert) = install_command(&governor, &text, 8, now);
    governor
        .workspace_mut()
        .apply_workspace_membership_command(
            &owner_device,
            &Command::InstallBot(install),
            &authority,
            now,
        )
        .unwrap();
    (governor, authority, owner_device, cert)
}

fn id() -> InstallationId {
    InstallationId::parse(BOT).unwrap()
}

#[test]
fn uninstall_is_final_and_keeps_the_row() {
    let (_, _, _, _, now) = crate::admission_tests::seeded();
    let (mut governor, authority, owner, cert) = installed(now);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &owner,
                &Command::UninstallBot { installation: id() },
                &authority,
                now
            )
            .unwrap()
    );
    let rows = governor.governance().installations(now).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].withdrawn_at_ns.is_some());
    assert!(!rows[0].acting);
    assert!(governor.permissions(cert.device_public_key, now).is_err());
    assert!(governor.bot_grants(BOT, now).unwrap().is_empty());
    assert!(
        governor
            .governance()
            .bot_program(&rows[0].installation.program)
            .is_some()
    );

    let text = program(vec![post_rule()]);
    let (again, _) = install_command(&governor, &text, 9, now);
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner,
            &Command::InstallBot(again),
            &authority,
            now
        ),
        Err("the bot was uninstalled".into())
    );
}

#[test]
fn pausing_needs_no_privilege_and_only_flips_the_flag() {
    let (_, _, _, _, now) = crate::admission_tests::seeded();
    let (mut governor, authority, owner, _) = installed(now);
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &owner,
                &Command::PauseBot { installation: id() },
                &authority,
                now
            )
            .unwrap()
    );
    assert!(
        governor
            .governance()
            .installation(BOT, now)
            .unwrap()
            .installation
            .paused
    );
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner,
            &Command::PauseBot { installation: id() },
            &authority,
            now
        ),
        Ok(false)
    );
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &owner,
                &Command::ResumeBot { installation: id() },
                &authority,
                now
            )
            .unwrap()
    );
    assert!(
        !governor
            .governance()
            .installation(BOT, now)
            .unwrap()
            .installation
            .paused
    );
}

#[test]
fn appearance_is_a_privileged_change() {
    let (_, _, _, _, now) = crate::admission_tests::seeded();
    let (mut governor, authority, owner, _) = installed(now);
    let (editor, _) = crate::admission_tests::add_editor(&mut governor, &authority, now);
    let personas = Command::SetBotAppearance {
        installation: id(),
        appearance: AppearancePolicy::Personas,
    };
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &personas, &authority, now)
            .is_err()
    );
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&owner, &personas, &authority, now)
            .unwrap()
    );
    assert_eq!(
        governor
            .governance()
            .installation(BOT, now)
            .unwrap()
            .installation
            .appearance,
        AppearancePolicy::Personas
    );
}

#[test]
fn adopting_moves_the_bot_to_the_adopter_and_reissues_its_grants() {
    let (_, _, _, _, now) = crate::admission_tests::seeded();
    let (mut governor, authority, _, old) = installed(now);
    let (editor, editor_root) = crate::admission_tests::add_editor(&mut governor, &authority, now);
    let adopter = SigningKey::from_bytes(&[5; 32]);
    assert_eq!(adopter.verifying_key().to_bytes(), editor_root);
    let (_, device) = bot_device(&adopter, 11);
    let adoption = Command::AdoptBot(Box::new(crate::workspace_membership::BotAdoption {
        installation: id(),
        device: BotDevice {
            delegation: bot_delegation(
                &adopter,
                device.device_public_key,
                BOT,
                governor.room(),
                now,
            ),
            cert: device.clone(),
        },
    }));
    // An Editor may not adopt by default: adopt-bot is privileged governance.
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &adoption, &authority, now)
            .is_err()
    );
    let adopt = tilo_action_policy::Grant::new(
        "adopt:editor",
        tilo_action_policy::Subject::person(hex::encode(editor_root)),
        tilo_action_policy::roles::governance_action(
            governor.room(),
            tilo_action_policy::roles::governance::ADOPT_BOT,
        ),
        KindSet::OUTSIDE,
        Answer::Allow,
        tilo_action_policy::Subject::person(person_hex()),
    );
    governor
        .governance_mut()
        .put_governed_grants(&[adopt], &authority, now)
        .unwrap();
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &adoption, &authority, now)
            .unwrap()
    );
    let live = governor.governance().installation(BOT, now).unwrap();
    assert_eq!(live.installation.installed_by, hex::encode(editor_root));
    assert_eq!(live.device_public_key, device.device_public_key);
    assert!(governor.permissions(old.device_public_key, now).is_err());
    for grant in governor.bot_grants(BOT, now).unwrap() {
        assert_eq!(
            grant.granted_by,
            tilo_action_policy::Subject::person(hex::encode(editor_root))
        );
    }
}

fn standup(version: &str) -> crate::workspace_membership::BotRequest {
    crate::workspace_membership::BotRequest {
        entry: "so.tilo.bots.standup".into(),
        version: version.into(),
    }
}

#[test]
fn the_owner_asks_for_a_directory_bot_and_takes_the_request_back() {
    let (mut governor, authority, _, owner_device, now) = crate::admission_tests::seeded();
    let ask = Command::RequestBot(standup("1.2.0"));
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &ask,
            &authority,
            now
        ),
        Ok(true)
    );
    let requests = governor.governance().bot_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request, standup("1.2.0"));
    assert_eq!(
        requests[0].requested_by,
        governor
            .permissions(owner_device.device_public_key, now)
            .unwrap()
            .person,
        "the request names who asked"
    );
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &ask,
            &authority,
            now
        ),
        Ok(false),
        "asking again for the same version changes nothing"
    );
    let newer = Command::RequestBot(standup("1.3.0"));
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &newer,
            &authority,
            now
        ),
        Ok(true)
    );
    assert_eq!(
        governor
            .governance()
            .bot_request("so.tilo.bots.standup")
            .map(|row| row.request.version),
        Some("1.3.0".to_owned()),
        "asking for another version replaces the request"
    );
    let withdraw = Command::WithdrawBotRequest {
        entry: "so.tilo.bots.standup".into(),
    };
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &withdraw,
            &authority,
            now
        ),
        Ok(true)
    );
    assert!(governor.governance().bot_requests().is_empty());
    assert_eq!(
        governor.workspace_mut().apply_workspace_membership_command(
            &owner_device,
            &withdraw,
            &authority,
            now
        ),
        Ok(false),
        "nothing to take back"
    );
}

#[test]
fn an_editor_cannot_ask_for_a_bot_by_default() {
    let (mut governor, authority, _, editor_device, now) = crate::admission_tests::seeded_editor();
    assert!(
        governor
            .workspace_mut()
            .apply_workspace_membership_command(
                &editor_device,
                &Command::RequestBot(standup("1.2.0")),
                &authority,
                now
            )
            .is_err()
    );
    assert!(governor.governance().bot_requests().is_empty());
}
