//! Browser's asks for the website sessions a person selected, answered where the workspace's
//! vault opens: the session hands them on from Browser's own connection, which it decided, with
//! the person who asked.
//!
//! Only the person's own, enabled selection is listed and opened for them, and only theirs is
//! forgotten. The cookies of anyone else's selection never leave this device's vault, and the
//! cookies of theirs leave it only for the one Browser that opens the site.

use account_credentials::service_sessions::SERVICE_SESSION_TYPE;
use accounts::Account;
use browser_continuity_core::sessions::SessionCookies;
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionDescriptor as Descriptor,
    BrowserServiceSessionReply as Reply,
};

use super::Workspaces;
use super::website_sessions::WebsiteSession;

/// What an ask is refused with when the person has no such selection, or it does not open.
pub const UNAVAILABLE: &str = "Selected website session unavailable";

/// The app that opens a site with a website session, and whose use is written down.
const BROWSER: &str = tilo_app_id::apps::BROWSER;

impl Workspaces {
    /// One of `workspace`'s website sessions listed, opened or forgotten for the person
    /// `identity`, as the workspace's accounts and its vault answer for it on this device.
    ///
    /// # Errors
    /// The ask is no website session's, the person has no such selection, or the accounts
    /// record or the vault does not read or take the write here: said in the log, answered
    /// [`UNAVAILABLE`].
    pub async fn website_session_ask(
        &self,
        workspace: &str,
        identity: [u8; 32],
        command: Command,
    ) -> Result<Reply, String> {
        let unavailable = |why: String| {
            tracing::warn!(%why, "a website session was not answered");
            UNAVAILABLE.to_owned()
        };
        let kept = Box::pin(self.website_sessions(workspace, SERVICE_SESSION_TYPE))
            .await
            .map_err(unavailable)?;
        let reply = match decide(identity, kept, command)? {
            Decided::Answered(reply) => reply,
            Decided::Open(account, session) => {
                Box::pin(self.website_session_opened(workspace, &account, BROWSER))
                    .await
                    .map_err(unavailable)?;
                Reply::Resolved { session }
            }
            Decided::Forget(account, forgotten) => {
                Box::pin(self.forget_website_session(workspace, &account))
                    .await
                    .map_err(unavailable)?;
                forgotten
            }
        };
        if !reply.valid() {
            return Err(UNAVAILABLE.into());
        }
        Ok(reply)
    }
}

/// What a website session's command comes to.
enum Decided {
    /// Answered from what was read, with nothing written.
    Answered(Reply),
    /// The account's session, handed on once its use is written into its entry.
    Open(Account, SessionCookies),
    /// The account whose session goes, and the answer once it went.
    Forget(Account, Reply),
}

/// The setting `account` names `key` with.
fn setting<'a>(account: &'a Account, key: &str) -> Option<&'a str> {
    account.config.get(key).and_then(serde_json::Value::as_str)
}

/// The cookies `kept` holds where they are the selection of the person `identity`, made under
/// `binding` for the service its account names. Metadata presence is no proof of a usable,
/// current person's selection: the cookies themselves are.
fn selection(kept: &WebsiteSession, identity: [u8; 32], binding: &str) -> Option<SessionCookies> {
    let account = &kept.account;
    if setting(account, "setup_binding") != Some(binding) {
        return None;
    }
    let session: SessionCookies = serde_json::from_str(kept.cookies.as_deref()?).ok()?;
    (session.validate().is_ok()
        && session.session_id == account.id.as_str()
        && session.identity_public_key == identity
        && session.setup_binding == binding
        && setting(account, "service") == Some(session.service.as_str()))
    .then_some(session)
}

/// What `command` comes to for the person `identity`, as the workspace's website sessions `kept`
/// stand: only the person's own, enabled selection is listed and opened, and only their own is
/// forgotten.
fn decide(
    identity: [u8; 32],
    kept: Vec<WebsiteSession>,
    command: Command,
) -> Result<Decided, String> {
    if kept.len() > runtime_collab::MAX_BROWSER_SERVICE_SESSIONS {
        return Err("Too many selected website sessions".into());
    }
    let mut kept = kept;
    let mut take = |id: &str| {
        let at = kept
            .iter()
            .position(|kept| kept.account.id.as_str() == id)?;
        Some(kept.swap_remove(at))
    };
    Ok(match command {
        Command::List {} => Decided::Answered(Reply::List {
            sessions: kept
                .iter()
                .filter(|kept| kept.account.enabled)
                .filter_map(|kept| {
                    let binding = setting(&kept.account, "setup_binding")?;
                    let session = selection(kept, identity, binding)?;
                    Some(Descriptor {
                        id: kept.account.id.as_str().into(),
                        service: session.service,
                        setup_binding: binding.into(),
                    })
                })
                .collect(),
        }),
        Command::Resolve { id, setup_binding } => {
            let kept = take(&id)
                .filter(|kept| kept.account.enabled)
                .ok_or(UNAVAILABLE)?;
            let session = selection(&kept, identity, &setup_binding).ok_or(UNAVAILABLE)?;
            Decided::Open(kept.account, session)
        }
        Command::Forget { id, setup_binding } => {
            let forgotten = Reply::Forgotten {
                id: id.clone(),
                setup_binding: setup_binding.clone(),
            };
            match take(&id) {
                // Already gone: a forget asked again after it went.
                None => Decided::Answered(forgotten),
                // Its cookies left the vault before a removal of the account that did not
                // finish: the account goes now.
                Some(kept)
                    if kept.cookies.is_none()
                        && setting(&kept.account, "setup_binding") == Some(&setup_binding) =>
                {
                    Decided::Forget(kept.account, forgotten)
                }
                Some(kept) => {
                    selection(&kept, identity, &setup_binding).ok_or(UNAVAILABLE)?;
                    Decided::Forget(kept.account, forgotten)
                }
            }
        }
        // Account secrets, items and what a session lends are not website sessions.
        Command::StoreAccountSecret { .. }
        | Command::ReleaseAccountSecret { .. }
        | Command::ReportAccountSync { .. }
        | Command::UseItem { .. }
        | Command::LendsItems {} => return Err(UNAVAILABLE.into()),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;

    use browser_continuity_core::sessions::{Cookie, SameSite, Service};

    use super::super::fixtures::{hosting_device, test_host_keyed};

    /// The person's selected Slack session, made under `binding`.
    fn selected(identity: [u8; 32]) -> SessionCookies {
        SessionCookies {
            schema: 1,
            session_id: "selected-slack".into(),
            service: Service::Slack,
            identity_public_key: identity,
            setup_binding: "prepared-operation".into(),
            source_snapshot_digest: [9; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "slack.com".into(),
                host_only: false,
                name: "d".into(),
                value: "private-session-marker".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::None,
            }],
        }
    }

    /// The workspace's account of `session`, with the cookies its vault entry keeps.
    fn kept(session: &SessionCookies, enabled: bool) -> WebsiteSession {
        let mut account = Account::new(session.session_id.clone(), SERVICE_SESSION_TYPE);
        account.config =
            serde_json::json!({"service": "slack", "setup_binding": "prepared-operation"});
        account.enabled = enabled;
        WebsiteSession {
            account,
            cookies: Some(serde_json::to_string(session).unwrap().into()),
        }
    }

    fn resolve() -> Command {
        Command::Resolve {
            id: "selected-slack".into(),
            setup_binding: "prepared-operation".into(),
        }
    }

    fn forget(binding: &str) -> Command {
        Command::Forget {
            id: "selected-slack".into(),
            setup_binding: binding.into(),
        }
    }

    /// The person's selection and another member's, kept in the vault of a workspace this
    /// device made: the person's is listed and opened for them alone, and once they forget it
    /// its account is gone from the workspace and it no longer opens. Forgetting it again
    /// answers that it is forgotten; another member's is never theirs to forget.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selection_the_workspaces_vault_keeps_is_listed_opened_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let (host, _keys) = test_host_keyed(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces();
        let workspace = workspaces.create("Sessions").await.unwrap().workspace;
        let person = cert.user_public_key;

        let mine = selected(person);
        let mut theirs = selected([8; 32]);
        theirs.session_id = "their-slack".into();
        for session in [&mine, &theirs] {
            workspaces
                .keep_website_session(
                    &workspace,
                    &kept(session, true).account,
                    ("Slack", session.service.url()),
                    serde_json::to_string(session).unwrap().into(),
                )
                .await
                .expect("the workspace's vault keeps the selection");
        }
        let held = workspaces
            .website_sessions(&workspace, SERVICE_SESSION_TYPE)
            .await
            .expect("the workspace's sessions read");
        assert!(
            held.len() == 2 && held.iter().all(|kept| kept.cookies.is_some()),
            "both selections are kept with their cookies"
        );
        let ask = |command| workspaces.website_session_ask(&workspace, person, command);

        let Ok(Reply::List { sessions }) = ask(Command::List {}).await else {
            panic!("the person's selections are listed");
        };
        let listed: Vec<_> = sessions.iter().map(|session| session.id.as_str()).collect();
        assert_eq!(listed, ["selected-slack"], "theirs alone");
        let Ok(Reply::Resolved { session }) = ask(resolve()).await else {
            panic!("the person's selection opens");
        };
        assert_eq!(session, mine, "byte for byte the selection they made");
        let theirs_resolved = Command::Resolve {
            id: "their-slack".into(),
            setup_binding: "prepared-operation".into(),
        };
        assert!(
            ask(theirs_resolved).await.is_err(),
            "another member's never opens for them"
        );

        assert!(matches!(
            ask(forget("prepared-operation")).await,
            Ok(reply) if reply.matches(&forget("prepared-operation"))
        ));
        assert!(
            matches!(ask(Command::List {}).await, Ok(Reply::List { sessions }) if sessions.is_empty())
        );
        assert!(
            ask(resolve()).await.is_err(),
            "a forgotten selection no longer opens"
        );
        let left = workspaces
            .website_sessions(&workspace, SERVICE_SESSION_TYPE)
            .await
            .expect("the workspace's sessions read");
        assert_eq!(left.len(), 1, "only another member's selection is left");
        assert_eq!(left[0].account.id.as_str(), "their-slack");
        assert!(left[0].cookies.is_some(), "with its cookies");
        assert!(
            matches!(ask(forget("prepared-operation")).await, Ok(reply) if reply.matches(&forget("prepared-operation"))),
            "forgetting it again answers that it is forgotten"
        );
        let theirs_forgotten = Command::Forget {
            id: "their-slack".into(),
            setup_binding: "prepared-operation".into(),
        };
        assert!(
            ask(theirs_forgotten).await.is_err(),
            "another member's is not theirs to forget"
        );
    }

    /// Only the person's own, enabled selection is listed and opened, and only theirs is
    /// forgotten; one that is gone already is forgotten again without asking whose it was.
    #[test]
    fn only_the_persons_own_enabled_selection_is_listed_opened_and_forgotten() {
        let person = [7; 32];
        let session = selected(person);
        let listed =
            |kept: Vec<WebsiteSession>, identity| match decide(identity, kept, Command::List {}) {
                Ok(Decided::Answered(Reply::List { sessions })) => sessions,
                _ => panic!("a list is answered from what was read"),
            };
        let sessions = listed(vec![kept(&session, true)], person);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.session_id);
        assert!(listed(vec![kept(&session, true)], [8; 32]).is_empty());
        assert!(listed(vec![kept(&session, false)], person).is_empty());

        let Ok(Decided::Open(_, opened)) = decide(person, vec![kept(&session, true)], resolve())
        else {
            panic!("the person's selection opens");
        };
        assert_eq!(opened, session);
        assert!(!format!("{opened:?}").contains("private-session-marker"));
        assert!(decide([8; 32], vec![kept(&session, true)], resolve()).is_err());
        assert!(decide(person, vec![kept(&session, false)], resolve()).is_err());
        let mut gone = kept(&session, true);
        gone.cookies = None;
        assert!(decide(person, vec![gone], resolve()).is_err());

        assert!(
            decide(
                [8; 32],
                vec![kept(&session, true)],
                forget("prepared-operation")
            )
            .is_err()
        );
        assert!(
            decide(
                person,
                vec![kept(&session, true)],
                forget("another-operation")
            )
            .is_err()
        );
        let Ok(Decided::Forget(account, forgotten)) = decide(
            person,
            vec![kept(&session, false)],
            forget("prepared-operation"),
        ) else {
            panic!("the person forgets their own selection, enabled or not");
        };
        assert_eq!(account.id.as_str(), "selected-slack");
        assert!(forgotten.matches(&forget("prepared-operation")));
        assert!(matches!(
            decide([8; 32], Vec::new(), forget("prepared-operation")),
            Ok(Decided::Answered(reply)) if reply.matches(&forget("prepared-operation"))
        ));
    }
}
