//! The logins a page in a window of an app's own signs in with, kept in the workspace's vault
//! ([`person_vault::PAGE_LOGINS`]).
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
//!   ([`person_vault::SIGN_IN`]): the item is tagged with its app and purpose
//!   ([`person_vault::app_tag`]), as every item an app keeps for itself is.
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
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_vault::{
    self, ForgetPageLogin, ItemRef, KeepPageLogin, PageFillAt, PageFilled, PageLogin, PageLogins,
    PageLoginsAt,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};

use super::Workspaces;
use super::person_vault::argument;
use super::vault_release::{AppProcess, device_may_not_read};

/// What a page login action that reached the vault's provider, rather than the window it would
/// be answered to, is refused with.
pub const NOT_A_WINDOW: &str = "A page's logins are answered only to the window of an app's own \
                                that shows the page, on that window's own connection: a person \
                                reads their logins in Passwords.";

/// What is written into an item each time a window is handed it to fill a page.
const PAGE_FILL_USE: &str = "tilo.vault/page-fill";

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
        .app(person_vault::PROVIDER)
        .kind(person_vault::ITEM_KIND)
        .resource(item.to_string())
        .action(person_vault::PAGE_FILL)
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
        person_vault::app_tag(app),
        person_vault::purpose_tag(person_vault::SIGN_IN),
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

impl Workspaces {
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

    /// Answer `request`, one of the page login actions ([`person_vault::is_page_login`]), for
    /// `process`, a process of `app`'s own in `workspace` as the daemon that started it vouches
    /// for it, on its connection alone.
    ///
    /// # Errors
    /// Refused, in words a person reads, for a tilolang process, a device that may not read the
    /// vault for the app, a site that is no web origin, a login that belongs elsewhere or that the
    /// vault does not keep, and a login the app neither kept nor may fill (whose fill asks its
    /// person first).
    #[expect(clippy::too_many_lines, reason = "one arm per page login action")]
    pub async fn page_login(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if matches!(process, AppProcess::Tilolang | AppProcess::OwnTilolang) {
            return Err(refused(from_a_tilolang_process(app)));
        }
        let cert = self.device_cert();
        let person = hex::encode(cert.user_public_key);
        let device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        self.using_vault(workspace, &device, Some(app), false)
            .await
            .map_err(|why| refused(device_may_not_read(app, &why)))?;
        let caller = Caller::local_app(app)
            .acting_for(person)
            .in_workspace(workspace);
        let action = request.target.address.action.as_str();
        match action {
            person_vault::PAGE_LOGINS => {
                let at: PageLoginsAt = argument(request, "a page's logins are read by its site")?;
                answer(&self.logins_at(workspace, app, &site(&at.site)?).await?)
            }
            person_vault::KEEP_PAGE_LOGIN => {
                let keeping: KeepPageLogin = argument(
                    request,
                    "a page's login to keep names its site, its name and its secret",
                )?;
                let place = site(&keeping.site)?;
                let replacing = match &keeping.replacing {
                    Some(item) => Some(self.usable(workspace, app, item, &place).await?.facts.id),
                    None => None,
                };
                let item = NewItem::new(
                    vault_items::ItemKind::Login,
                    vec![place],
                    keeping.username.clone(),
                    keeping.secret.clone(),
                );
                let kept = self
                    .changing_vault(workspace, (Some(caller.acting()), Some(request)), |items| {
                        items.keep_tagged(&own_tags(app), replacing.as_ref(), item, "")
                    })
                    .await
                    .map_err(refused)?;
                let kept = SiteLogin {
                    facts: kept,
                    username: keeping.username.clone(),
                };
                answer(&shown(kept, app).ok_or_else(|| refused("the kept login has no reference"))?)
            }
            person_vault::FORGET_PAGE_LOGIN => {
                let forgetting: ForgetPageLogin = argument(
                    request,
                    "a page's login to forget names its site and the login",
                )?;
                let place = site(&forgetting.site)?;
                let id = self
                    .usable(workspace, app, &forgetting.item, &place)
                    .await?
                    .facts
                    .id;
                self.changing_vault(workspace, (Some(caller.acting()), Some(request)), |items| {
                    Ok(items.remove(&id))
                })
                .await
                .map_err(refused)?;
                answer(&self.logins_at(workspace, app, &place).await?)
            }
            person_vault::PAGE_FILL => {
                let item = person_vault::page_filled_item(&request.target).ok_or_else(|| {
                    ActionCallError::InvalidRequest {
                        reason:
                            "a fill is addressed at the login it fills: vault.item:<id>#page-fill"
                                .to_owned(),
                    }
                })?;
                let at: PageFillAt = argument(request, "a fill names the site its page is at")?;
                let place = site(&at.site)?;
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
            kind: person_vault::ITEM_KIND.to_owned(),
            effect: Some(tilo_actions::ActionKind::Read),
            ..AskedFacts::by(super::asking::asked_by(caller))
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
