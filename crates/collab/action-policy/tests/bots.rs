//! A bot acts as itself for its installer: never above what the installer
//! may do right now, never beyond its own grant, never governing the workspace.

use tilo_action_policy::{
    Acting, ActionKind, Answer, Authority, Context, Grantee, KindSet, Level, Refusal, Role, Rule,
    Standing, TreePath, WorkspacePolicy, bot,
    roles::{self, governance},
};

const NOW: u64 = 1_000_000;
const WORKSPACE: &str = "r";
const OWNER: &str = "a11ce";
const EDITOR: &str = "b0b";
const BOT: &str = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

fn post() -> TreePath {
    TreePath::workspace(WORKSPACE)
        .app("so.tilo.chat")
        .kind("chat.workspace")
        .any(Level::Resource)
        .action("post")
}

fn send_out() -> TreePath {
    TreePath::workspace(WORKSPACE)
        .app("so.tilo.discord")
        .kind("discord.guild")
        .any(Level::Resource)
        .action("send")
}

/// The program's rules, written over any workspace as the Shortcuts editor does.
fn program_rules() -> Vec<Rule> {
    let any = TreePath::root().any(Level::Workspace);
    vec![
        Rule::new(
            any.clone()
                .app("so.tilo.chat")
                .kind("chat.workspace")
                .any(Level::Resource)
                .action("post"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        ),
        Rule::new(
            any.app("so.tilo.discord")
                .kind("discord.guild")
                .any(Level::Resource)
                .action("send"),
            KindSet::ALL,
            Answer::Allow,
        ),
    ]
}

fn workspace_with_bot(installer: &str, installer_role: Option<Role>) -> WorkspacePolicy {
    let mut workspace = WorkspacePolicy::new(WORKSPACE, OWNER);
    if let Some(role) = installer_role {
        workspace.members.insert(installer.to_owned(), role);
    }
    // `bot_grants_carry_ids_installer_and_the_governance_deny` covers the
    // error path; here a refusal would leave the bot nothing and fail the
    // allow assertions.
    workspace
        .grants
        .extend(bot::grants(WORKSPACE, BOT, installer, &program_rules()).unwrap_or_default());
    workspace
}

fn bot_of(workspace: WorkspacePolicy, installer: &str) -> Standing {
    Standing::new(
        workspace,
        Acting::bot(BOT).acting_for(Acting::person(installer)),
    )
}

#[test]
fn a_bot_answers_the_least_of_its_installer_and_its_grant() {
    let owners = bot_of(workspace_with_bot(OWNER, None), OWNER);
    assert_eq!(
        owners.answer(&post(), ActionKind::Undoable, &cx()),
        Answer::Allow
    );
    assert_eq!(
        owners.answer(&send_out(), ActionKind::Outside, &cx()),
        Answer::Allow
    );
    let elsewhere = TreePath::workspace(WORKSPACE)
        .app("so.tilo.notes")
        .action("append");
    assert_eq!(
        owners.answer(&elsewhere, ActionKind::Undoable, &cx()),
        Answer::Deny,
        "nothing beyond the derived grant"
    );

    // An Editor's own grants ask for Outside, so the bot's Outside step asks,
    // and an unattended run turns that into a refusal.
    let editors = bot_of(workspace_with_bot(EDITOR, Some(Role::Editor)), EDITOR);
    assert_eq!(
        editors.answer(&post(), ActionKind::Undoable, &cx()),
        Answer::Allow
    );
    assert_eq!(
        editors.answer(&send_out(), ActionKind::Outside, &cx()),
        Answer::Ask
    );

    let viewers = bot_of(workspace_with_bot(EDITOR, Some(Role::Viewer)), EDITOR);
    assert_eq!(
        viewers.answer(&post(), ActionKind::Undoable, &cx()),
        Answer::Deny
    );
}

#[test]
fn removing_the_installer_leaves_the_bot_nothing() {
    let workspace = workspace_with_bot(EDITOR, None);
    let orphan = bot_of(workspace, EDITOR);
    assert_eq!(
        orphan.answer(&post(), ActionKind::Undoable, &cx()),
        Answer::Deny
    );
    assert_eq!(
        orphan.answer(&post(), ActionKind::Read, &cx()),
        Answer::Deny
    );
}

#[test]
fn a_bot_never_governs_whatever_its_installer_may() {
    let owners = bot_of(workspace_with_bot(OWNER, None), OWNER);
    for action in governance::ALL {
        let node = roles::governance_action(WORKSPACE, action);
        assert_eq!(
            owners.answer(&node, roles::GOVERNANCE_KIND, &cx()),
            Answer::Deny,
            "{action}"
        );
    }
}

#[test]
fn bot_rules_refuse_governance_and_other_workspaces() {
    let governing = Rule::new(
        TreePath::root()
            .any(Level::Workspace)
            .kind(roles::ROOM_KIND)
            .action(governance::GRANT),
        KindSet::ALL,
        Answer::Allow,
    );
    assert_eq!(
        bot::scoped_rules(WORKSPACE, &[governing]),
        Err(bot::BotRuleError::Governance)
    );
    let other_workspace = Rule::new(
        TreePath::workspace("elsewhere").app("so.tilo.chat"),
        KindSet::READ,
        Answer::Allow,
    );
    assert_eq!(
        bot::scoped_rules(WORKSPACE, &[other_workspace]),
        Err(bot::BotRuleError::OutsideWorkspace)
    );
    let asking = Rule::new(
        TreePath::root().any(Level::Workspace).app("so.tilo.chat"),
        KindSet::READ,
        Answer::Ask,
    );
    assert_eq!(
        bot::scoped_rules(WORKSPACE, &[asking]),
        Err(bot::BotRuleError::NotAllow)
    );
    let scoped = bot::scoped_rules(WORKSPACE, &program_rules()).expect("valid bot rules");
    assert!(
        scoped
            .iter()
            .all(|rule| rule.node.workspace_id() == Some(WORKSPACE))
    );
}

#[test]
fn bot_grants_carry_ids_installer_and_the_governance_deny() {
    let grants = bot::grants(WORKSPACE, BOT, OWNER, &program_rules()).expect("valid bot rules");
    assert_eq!(grants.len(), 3);
    for grant in &grants {
        assert!(grant.id.as_str().starts_with(&bot::grant_prefix(BOT)));
        assert_eq!(grant.subject, Grantee::bot(BOT));
        assert_eq!(grant.granted_by, Grantee::person(OWNER));
    }
    let deny = grants.last().expect("the governance deny");
    assert_eq!(deny.node, roles::workspace_resource(WORKSPACE));
    assert_eq!(deny.answer, Answer::Deny);
}

#[test]
fn privileged_governance_refuses_an_installer_who_is_only_asked() {
    // Bob is an Editor, and the owner gave Editors an Ask on install-bot and
    // pause-bot.
    let mut workspace = WorkspacePolicy::new(WORKSPACE, OWNER);
    workspace.members.insert(EDITOR.into(), Role::Editor);
    for (n, action) in [governance::INSTALL_BOT, governance::PAUSE_BOT]
        .into_iter()
        .enumerate()
    {
        workspace.grants.push(tilo_action_policy::Grant::new(
            format!("ask:{n}"),
            Grantee::person(EDITOR),
            roles::governance_action(WORKSPACE, action),
            KindSet::OUTSIDE,
            Answer::Ask,
            Grantee::person(OWNER),
        ));
    }
    let bob = Standing::person(workspace.clone(), EDITOR);
    let authority: Authority<'_> = bob.authority();
    assert!(matches!(
        workspace.check_action(&authority, governance::INSTALL_BOT, &cx(), true),
        Err(Refusal::NotPermitted(_))
    ));
    assert!(
        workspace
            .check_action(&authority, governance::PAUSE_BOT, &cx(), true)
            .is_ok()
    );
    assert!(governance::is_privileged(governance::ADOPT_BOT));
    assert!(!governance::is_privileged(governance::RESUME_BOT));
}

#[test]
fn an_update_fits_within_when_every_new_allow_is_covered_by_an_old_one() {
    let old = bot::grants(WORKSPACE, BOT, OWNER, &program_rules()).expect("valid bot rules");
    let narrower =
        bot::grants(WORKSPACE, BOT, OWNER, &program_rules()[..1]).expect("valid bot rules");
    assert!(bot::fits_within(&old, &narrower));
    assert!(!bot::fits_within(&narrower, &old));
    assert!(bot::fits_within(&old, &old));
}
