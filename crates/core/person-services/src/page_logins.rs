//! The logins a page in a window of an app's own signs in with, kept in the workspace's vault
//! ([`workspace_vault::PAGE_LOGINS`]).
//!
//! A browser's window holds its page's engine, and only the engine reads what a person typed
//! into a login form or writes into one. So the window itself asks the vault, on its own
//! connection, which the device's daemon knows by the process it started ([`AppProcess`]); the
//! site is the one the window's engine says its page is at. A secret travels between that window
//! and the vault alone: never through the app's actions, its records or a tilolang process.
//!
//! - Listing the logins a site has answers what is safe to say of each: its title, the name it
//!   signs in with and where it belongs.
//! - Keeping a login the page sent writes it as the app's own, for signing in
//!   ([`workspace_vault::SIGN_IN`]): the item is tagged with its app and purpose
//!   ([`workspace_vault::app_tag`]), as every item an app keeps for itself is.
//! - Filling hands one login's name and secret to the window, for that site alone. A login the app
//!   kept itself is filled unasked. Any other is filled only where a grant of the workspace's
//!   lets the app fill that one item; with none, its person is asked once, as an asked record
//!   whose "Approve and grant" covers exactly that item, and nothing is filled until they allow it.
//! - Replacing or forgetting a login is held to the same rule: the app's own, or one it may fill.
//!
//! Every use is written into the item first, naming the app, never the secret.

use accounts::vault_items::{
    self, Allowance, ItemFacts, ItemId, NewItem, Place, SiteLogin, UsedFor,
};
use serde_json::Value;
use tilo_action_policy::{Answer, Context, Grantee, TreePath, decide};
use tilo_action_types::ActionKind;
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};

use crate::person_vault::asked_of;
use crate::vault_release::{AppProcess, device_may_not_read};
use tilo_workspace_records::vault::{
    self as workspace_vault, ItemRef, PageFilled, PageLogin, PageLogins, VaultAction,
};

/// What a page login action that reached the vault's provider, rather than the window it would
/// be answered to, is refused with.
pub const NOT_A_WINDOW: &str = "A page's logins are answered only to the window of an app's own \
                                that shows the page, on that window's own connection: a person \
                                reads their logins in Passwords.";

/// What a window of another app than Browser asking for the person's website sign-ins is refused
/// with.
pub const NOT_BROWSER: &str = "The website sign-ins you brought open only in your Browser";

/// What is written into an item each time a window is handed it to fill a page.
const PAGE_FILL_USE: &str = "tilo.vault/page-fill";

/// What a fill no running call of the app's asked for is refused with.
pub const ASKED_BY_NOBODY: &str = "A login fills a page only for a call of the app's action that \
                                   still runs, decided as whoever made that call; this fill names \
                                   none.";

/// What a fill asked by someone the workspace grants nothing of the login is refused with.
#[must_use]
pub fn not_granted(who: &str) -> String {
    format!(
        "{who} holds no grant for this login, so it fills no page with it. Its person grants it \
         this login in Passwords."
    )
}

/// What a page login action asked from a tilolang process is refused with.
#[must_use]
pub fn from_a_tilolang_process(app: &str) -> String {
    format!(
        "{app} asked from a tilolang process, which runs the code every app shares. A page's \
         logins are answered only to the window of {app}'s own that shows the page."
    )
}

/// What a fill waits on while its person is asked whether the app may fill that login.
#[must_use]
pub fn asked_first(app: &str) -> String {
    format!(
        "{app} may not fill this login yet: your person is asked whether it may, once. Nothing \
         was filled; fill it again once they allowed it."
    )
}

/// What replacing or forgetting a login the app may not fill is refused with.
#[must_use]
pub fn not_its_own(app: &str) -> String {
    format!(
        "{app} neither kept this login nor may fill it, so it does not change it. Change it in \
         Passwords, or let {app} fill it first."
    )
}

/// What a site the window names that is no web origin is refused with.
const NOT_A_SITE: &str = "A page's site is its web origin, such as https://example.com";

/// Where a grant that lets an app fill pages from `item` stands in `workspace`'s policy: what
/// "Approve and grant" on an asked fill writes, the fill being addressed at the item.
fn fill_node(workspace: &str, item: &ItemRef) -> TreePath {
    TreePath::workspace(workspace)
        .app(workspace_vault::PROVIDER)
        .kind(workspace_vault::ITEM_KIND)
        .resource(item.to_string())
        .action(workspace_vault::PAGE_FILL)
}

/// The site `text` names, when it is a web origin.
fn site(text: &str) -> Result<Place, ActionCallError> {
    Place::parse(text)
        .ok()
        .filter(Place::is_web)
        .ok_or_else(|| ActionCallError::InvalidRequest {
            reason: NOT_A_SITE.to_owned(),
        })
}

/// The tags a login `app` keeps for signing in carries: its app and its purpose, as the vault
/// names an app's own items.
fn own_tags(app: &str) -> [String; 2] {
    [
        workspace_vault::app_tag(app),
        workspace_vault::purpose_tag(workspace_vault::SIGN_IN),
    ]
}

/// Whether `app` kept the login `facts` describe for signing in.
fn kept_by(facts: &ItemFacts, app: &str) -> bool {
    own_tags(app).iter().all(|tag| facts.tags.contains(tag))
}

/// What a window is told of `facts`, the asking app being `app`.
fn shown(SiteLogin { facts, username }: SiteLogin, app: &str) -> Option<PageLogin> {
    Some(PageLogin {
        reference: ItemRef::from_id(facts.id.as_str())?,
        own: kept_by(&facts, app),
        title: facts.title,
        username,
        belongs_to: facts
            .places
            .iter()
            .map(|place| place.as_str().to_owned())
            .collect(),
    })
}

fn answer<T: serde::Serialize>(value: &T) -> Result<Value, ActionCallError> {
    serde_json::to_value(value).map_err(|_| ActionCallError::Unavailable {
        reason: "the answer could not be written".into(),
    })
}

fn refused(error: impl std::fmt::Display) -> ActionCallError {
    ActionCallError::refused(error.to_string())
}

impl crate::PersonServices {
    /// Whether a grant of `workspace`'s lets `app` fill pages from `item`.
    async fn app_fills(&self, workspace: &str, app: &str, item: &ItemRef) -> bool {
        let Some(policy) = self.policy_of(workspace).await else {
            return false;
        };
        let decision = decide(
            policy.grants.iter(),
            &[Grantee::app(app)],
            &fill_node(workspace, item),
            ActionKind::Read,
            &Context::at(tilo_clock::unix_now_ms()),
        );
        decision.answer == Answer::Allow && decision.grant().is_some()
    }

    /// Refuse a fill of `item` that `asked_by`, whoever made the call of `app`'s action that
    /// asked for it, holds no grant for in `workspace`'s policy: an agent needs a grant of its
    /// person's naming it for that login, a person needs the login to be theirs to use, and the
    /// app's own call is held to the app's grant, decided after this. Anyone else fills nothing.
    async fn asker_may_fill(
        &self,
        workspace: &str,
        app: &str,
        asked_by: &Caller,
        item: &ItemRef,
    ) -> Result<(), ActionCallError> {
        let acting = asked_by.acting();
        if matches!(&acting.by, tilo_member_types::Actor::App(by) if by.as_str() == app) {
            return Ok(());
        }
        let who = acting
            .agent_id()
            .or_else(|| acting.person_id())
            .unwrap_or("Whoever asked")
            .to_owned();
        let Some(policy) = self.policy_of(workspace).await else {
            return Err(refused(not_granted(&who)));
        };
        let node = fill_node(workspace, item);
        let cx = Context::at(tilo_clock::unix_now_ms());
        let granted = if let Some((agent, person)) =
            tilo_workspace_service::workspaces::workspace_stores::agent_of(asked_by)
        {
            let decision =
                policy
                    .party_of_agent(agent, person)
                    .decide(&node, ActionKind::Read, &cx);
            decision.answer == Answer::Allow && decision.grant().is_some()
        } else if let Some(person) = acting.person_id() {
            policy
                .party(person)
                .decide(&node, ActionKind::Read, &cx)
                .answer
                == Answer::Allow
        } else {
            false
        };
        if granted {
            Ok(())
        } else {
            Err(refused(not_granted(&who)))
        }
    }

    /// Whether `app` may use the login `facts` describe: it kept it itself, or a grant lets it
    /// fill pages from it.
    async fn may_use(&self, workspace: &str, app: &str, facts: &ItemFacts) -> bool {
        if kept_by(facts, app) {
            return true;
        }
        match ItemRef::from_id(facts.id.as_str()) {
            Some(item) => self.app_fills(workspace, app, &item).await,
            None => false,
        }
    }

    /// Answer `request`, one of the page login actions ([`workspace_vault::PAGE_LOGIN_ACTIONS`]), for
    /// `process`, a process of `app`'s own in `workspace` as the daemon that started it vouches
    /// for it, on its connection alone. A fill is decided as `asked_by` too: whoever made the
    /// call of the app's action that asked for it, which still runs (`None` where the fill names
    /// no such call).
    ///
    /// # Errors
    /// Refused, in words a person reads, for a tilolang process, a device that may not read the
    /// vault for the app, a site that is no web origin, a login that belongs elsewhere or that the
    /// vault does not keep, a fill no running call asked for or whose caller holds no grant for
    /// the login, and a login the app neither kept nor may fill (whose fill asks its person
    /// first).
    pub async fn page_login(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
        request: &InvokeRequest,
        asked_by: Option<&Caller>,
    ) -> Result<Value, ActionCallError> {
        let first = self.answered_first((app, workspace), process, request);
        if let Some(answered) = first.await {
            return answered;
        }
        let cert = self.device_cert();
        let device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        self.using_vault(workspace, &device, Some(app), false)
            .await
            .map_err(|why| refused(device_may_not_read(app, &why)))?;
        let caller = Caller::local_app(app)
            .acting_for(hex::encode(cert.user_public_key))
            .in_workspace(workspace);
        match asked_of(request)? {
            VaultAction::PageLogins { site: at } => {
                answer(&self.logins_at(workspace, app, &site(&at)?).await?)
            }
            VaultAction::KeepPageLogin {
                site: at,
                username,
                secret,
                replacing,
            } => {
                let place = site(&at)?;
                let replacing = match &replacing {
                    Some(item) => Some(self.usable(workspace, app, item, &place).await?.facts.id),
                    None => None,
                };
                let item = NewItem::new(
                    vault_items::ItemKind::Login,
                    vec![place],
                    username.clone(),
                    secret,
                );
                let kept = self
                    .changing_vault(workspace, (Some(caller.acting()), Some(request)), |items| {
                        items.keep_tagged(&own_tags(app), replacing.as_ref(), item, "")
                    })
                    .await
                    .map_err(refused)?;
                let kept = SiteLogin {
                    facts: kept,
                    username,
                };
                answer(&shown(kept, app).ok_or_else(|| refused("the kept login has no reference"))?)
            }
            VaultAction::ForgetPageLogin { site: at, item } => {
                let place = site(&at)?;
                let id = self.usable(workspace, app, &item, &place).await?.facts.id;
                self.changing_vault(workspace, (Some(caller.acting()), Some(request)), |items| {
                    Ok(items.remove(&id))
                })
                .await
                .map_err(refused)?;
                answer(&self.logins_at(workspace, app, &place).await?)
            }
            VaultAction::PageFill { site: at } => {
                let item = ItemRef::addressed(&request.target, workspace_vault::PAGE_FILL)
                    .ok_or_else(|| ActionCallError::InvalidRequest {
                        reason:
                            "a fill is addressed at the login it fills: vault.item:<id>#page-fill"
                                .to_owned(),
                    })?;
                let place = site(&at)?;
                let asked_by = asked_by.ok_or_else(|| refused(ASKED_BY_NOBODY))?;
                self.asker_may_fill(workspace, app, asked_by, &item).await?;
                let login = self.kept_login(workspace, &item, &place).await?;
                let facts = &login.facts;
                let own = kept_by(facts, app);
                if !own && !self.app_fills(workspace, app, &item).await {
                    self.ask_fill(workspace, &caller, request, &login)
                        .map_err(refused)?;
                    return Err(refused(asked_first(app)));
                }
                let what = format!("{app}'s page at {place}");
                let released = self
                    .changing_vault(workspace, (Some(caller.acting()), Some(request)), |items| {
                        items.use_for(
                            &facts.id,
                            vault_items::ItemKind::Login,
                            std::slice::from_ref(&place),
                            app,
                            UsedFor {
                                using: PAGE_FILL_USE,
                                what: &what,
                                allowed: (!own).then_some(Allowance::Always),
                            },
                        )
                    })
                    .await
                    .map_err(refused)?;
                answer(&PageFilled {
                    username: released.username.clone(),
                    secret: String::clone(&released.secret),
                })
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// What `request` is answered before anything else, where it is: refused for a tilolang
    /// process, and the person's website sign-ins for Browser's window alone. `None` for every
    /// other call of a window of the app's own.
    async fn answered_first(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
        request: &InvokeRequest,
    ) -> Option<Result<Value, ActionCallError>> {
        if matches!(process, AppProcess::Tilolang | AppProcess::OwnTilolang) {
            return Some(Err(refused(from_a_tilolang_process(app))));
        }
        match asked_of(request) {
            Ok(VaultAction::WebsiteSignIns { held }) => {
                Some(Box::pin(self.sign_ins_for_window((app, workspace), &held)).await)
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        }
    }

    /// The website sign-ins the person brought, all but those `held` names, for `app`'s window
    /// in `workspace`: the sites they open signed in in their own Browser, and in no other
    /// app's window.
    async fn sign_ins_for_window(
        &self,
        (app, workspace): (&str, &str),
        held: &[String],
    ) -> Result<Value, ActionCallError> {
        if app != tilo_app_id::apps::BROWSER {
            return Err(refused(NOT_BROWSER));
        }
        let cert = self.device_cert();
        let device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        self.using_vault(workspace, &device, Some(app), false)
            .await
            .map_err(|why| refused(device_may_not_read(app, &why)))?;
        let sign_ins = Box::pin(self.website_sign_ins(workspace, cert.user_public_key, held))
            .await
            .map_err(refused)?;
        answer(&sign_ins)
    }

    /// The logins `workspace`'s vault keeps for `site`, as `app`'s window is told them.
    async fn logins_at(
        &self,
        workspace: &str,
        app: &str,
        site: &Place,
    ) -> Result<PageLogins, ActionCallError> {
        let opened = self.open_vault(workspace).await.map_err(refused)?;
        let mut logins: Vec<PageLogin> = opened
            .items()
            .logins_at(site)
            .into_iter()
            .filter_map(|facts| shown(facts, app))
            .collect();
        // By the name each signs in with, so the page lists them the same way every time.
        logins.sort_by(|a, b| (&a.username, &a.title).cmp(&(&b.username, &b.title)));
        Ok(PageLogins { logins })
    }

    /// What is safe to say of `item`, a login the vault keeps for `site`.
    async fn kept_login(
        &self,
        workspace: &str,
        item: &ItemRef,
        site: &Place,
    ) -> Result<SiteLogin, ActionCallError> {
        let id =
            ItemId::parse(item.id()).ok_or_else(|| refused(vault_items::ItemError::NotFound))?;
        let items = self.open_vault(workspace).await.map_err(refused)?.items();
        match items
            .logins_at(site)
            .into_iter()
            .find(|listed| listed.facts.id == id)
        {
            Some(login) => Ok(login),
            None => Err(refused(
                items
                    .belongs_at(&id, site)
                    .err()
                    .unwrap_or(vault_items::ItemError::WrongKind),
            )),
        }
    }

    /// [`Self::kept_login`] for a login `app` may change: its own, or one it may fill.
    async fn usable(
        &self,
        workspace: &str,
        app: &str,
        item: &ItemRef,
        site: &Place,
    ) -> Result<SiteLogin, ActionCallError> {
        let login = self.kept_login(workspace, item, site).await?;
        if !self.may_use(workspace, app, &login.facts).await {
            return Err(refused(not_its_own(app)));
        }
        Ok(login)
    }

    /// Ask the person `caller` acts for whether its app may fill pages from the login `facts`
    /// describe, as an asked record whose "Approve and grant" writes the grant on that item
    /// alone: unless one asks already.
    fn ask_fill(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        SiteLogin { facts, username }: &SiteLogin,
    ) -> Result<(), String> {
        let (store, records) = self
            .vault_asks()
            .ok_or("This device keeps no asked records, so nobody can be asked")?;
        let app = caller.acting().app().map(ToString::to_string);
        let target = request.target.clone();
        let waits = store.waiting_in(workspace).into_iter().any(|waiting| {
            waiting.caller.acting().app().map(ToString::to_string) == app
                && store
                    .kept_ask(&waiting.asked.record)
                    .is_some_and(|kept| kept.request.target == target)
        });
        if waits {
            return Ok(());
        }
        let app = app.unwrap_or_default();
        let title = if facts.title.trim().is_empty() {
            "a login".to_owned()
        } else {
            facts.title.clone()
        };
        let places: Vec<&str> = facts.places.iter().map(Place::as_str).collect();
        let mut shown = vec![Fact::new("Belongs to", places.join(" "))];
        if !username.is_empty() {
            shown.insert(0, Fact::new("Signs in as", username.clone()));
        }
        let asked = AskedFacts {
            words: format!(
                "{app} asks to fill your login {title} into a page at that site. Approve and \
                 grant, and it fills this one login there whenever you choose it."
            ),
            on: Some(title),
            facts: shown,
            verb: "fill".to_owned(),
            plural: Some("this login".to_owned()),
            kind: workspace_vault::ITEM_KIND.to_owned(),
            effect: Some(tilo_actions::ActionKind::Read),
            ..AskedFacts::by(tilo_workspace_service::workspaces::asking::asked_by(caller))
        };
        // The site the window named is the one argument kept: the ask holds no secret.
        let request = InvokeRequest {
            target,
            ..request.clone()
        };
        records
            .write_asked(workspace, caller, &request, &asked)
            .map(drop)
    }
}

#[cfg(all(test, feature = "agents"))]
#[path = "page_login_tests.rs"]
pub(crate) mod page_login_tests;
