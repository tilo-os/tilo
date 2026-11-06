//! The workspace directory in a page: the session's own sheet
//! ([`tilo_sheets::directory`]), drawn over the desktop, with what is this
//! page's own — the site whose directory it lists, the person a join is made
//! as, and the workspaces the page holds. Reads, joins and pasted links are
//! [`tilo_workspace_directory`]'s, over the page's `fetch`; what the person
//! picks is handed to the page as a [`HostRequest`], as the session's shell
//! hands the same effects to the desk.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde::Deserialize;
use tilo_sheets::Presentation;
use tilo_sheets::directory::{self, Ask, Directory, DirectoryEffect, Status};
use tilo_workspace_directory::{Held, Person, Store};

use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{LengthPercentageAuto, NodeId, Position, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::{AppModel, Element, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;
use ui::{LocalState, TextRenderer, Theme};

use crate::shell::{HostRequest, HostRequests};

/// What the page tells its desktop about the workspace directory
/// (`mountDesktop({directory})`): a page has no `community.toml`, no identity
/// on disk and no desk of its own to read them from.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct PageDirectoryConfig {
    /// The site whose directory the sheet lists: the page's own entry.
    #[serde(default)]
    pub site: String,
    /// The person a join is made as, as the page keeps them; absent where the
    /// page has no person of its own yet.
    #[serde(default)]
    pub person: Option<PagePerson>,
    /// The workspaces this page holds, each named by the host action id that
    /// opens it.
    #[serde(default)]
    pub held: Vec<Held>,
}

/// The person a page joins a public workspace as.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PagePerson {
    /// Their root key, hex: the key a door admits.
    #[serde(rename = "userPublicKey")]
    pub user: String,
    /// The name a workspace learns of them.
    #[serde(default)]
    pub name: String,
}

/// The sheet over the page, while it is up.
pub(crate) struct PageDirectory {
    requests: HostRequests,
    store: Store,
    presentation: Presentation,
    query: LocalState<String>,
    asked: Rc<RefCell<Vec<DirectoryEffect>>>,
    /// The site whose directory the sheet lists.
    community: Option<url::Url>,
    person: Option<Person>,
    held: Vec<Held>,
    /// The origin of the door last asked: where a page redeems the invitation
    /// of a door that names no origin.
    door_origin: Option<String>,
    content: Option<Element>,
    /// What the built sheet shows and the search it was built with, so it is
    /// built again only when either moved.
    built: (Directory, u64),
    replaced: bool,
    rect: Rect,
}

impl PageDirectory {
    pub(crate) fn new(
        requests: HostRequests,
        config: Option<&PageDirectoryConfig>,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let community = config
            .map(|config| config.site.trim())
            .filter(|site| !site.is_empty())
            .and_then(|site| tilo_workspace_directory::site_entry(site).ok());
        let person = config
            .and_then(|config| config.person.as_ref())
            .and_then(|person| {
                let user: [u8; 32] = hex::decode(&person.user).ok()?.try_into().ok()?;
                Some(Person {
                    user,
                    name: person.name.clone(),
                })
            });
        let store = Store::default();
        store.waking(Arc::clone(wake));
        Self {
            requests,
            store,
            presentation: Presentation::default(),
            query: LocalState::new(String::new()),
            asked: Rc::default(),
            community,
            person,
            held: config.map(|config| config.held.clone()).unwrap_or_default(),
            door_origin: None,
            content: None,
            built: (Directory::default(), u64::MAX),
            replaced: false,
            rect: Rect::ZERO,
        }
    }

    /// Put the sheet up, reading the page's directory as the session's shell
    /// reads its community's.
    pub(crate) fn open(&mut self) {
        self.query.set(String::new());
        self.store.set_note(None);
        self.store.read(self.community.clone(), None);
        self.presentation.open(false);
        self.replaced = true;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.presentation.is_open()
    }

    /// Take what the directory task answered, show it, and carry out what the
    /// person pressed since the last pass. Whether anything changed.
    pub(crate) fn follow(&mut self) -> bool {
        let mut changed = self.store.answered();
        for joined in self.store.take_joined() {
            changed = true;
            // A page joins no community of its own as it starts: a join it was
            // let into is the person's to open, not the page's.
            if joined.community.is_some() {
                continue;
            }
            let origin = joined
                .granted
                .origin
                .clone()
                .or_else(|| self.door_origin.clone())
                .unwrap_or_default();
            self.close();
            self.requests.borrow_mut().push(HostRequest::Joined {
                origin,
                invitation: joined.granted.invitation.clone(),
                admission_token: joined.granted.admission_token.clone(),
                workspace: joined.granted.workspace.clone(),
                name: joined.name.clone(),
            });
        }
        if self.presentation.is_open() {
            let shown = self.store.view(&self.held);
            let revision = self.query.revision();
            if self.content.is_none() || shown != self.built.0 || revision != self.built.1 {
                self.built = (shown.clone(), revision);
                // A page keeps no log of its own: a site that could not be
                // read says why here, where its people's consoles keep it.
                if let Status::Failed(error) = &shown.status {
                    web_sys::console::warn_1(&format!("workspace directory: {error}").into());
                }
                let asked = Rc::clone(&self.asked);
                let ask: Ask = Rc::new(move |effect: DirectoryEffect| {
                    asked.borrow_mut().push(effect);
                });
                self.content = Some(directory::sheet(&shown, &self.query, &ask));
                self.replaced = true;
                changed = true;
            }
        }
        let effects: Vec<DirectoryEffect> = std::mem::take(&mut *self.asked.borrow_mut());
        for effect in effects {
            changed = true;
            self.answer(effect);
        }
        if !self.presentation.is_open() && self.content.is_some() {
            self.content = None;
            self.replaced = true;
            changed = true;
        }
        changed
    }

    /// Carry out one thing the person pressed, as the session's shell carries
    /// out the same effect for the desk.
    fn answer(&mut self, effect: DirectoryEffect) {
        match effect {
            DirectoryEffect::Join { key } => self.join_listed(&key),
            DirectoryEffect::Open { key } => {
                self.close();
                self.requests.borrow_mut().push(HostRequest::Action(key));
            }
            DirectoryEffect::Look { text } => self.store.read(None, Some(text)),
            DirectoryEffect::LinkOrCode { text } => {
                let taken = self
                    .person
                    .clone()
                    .ok_or_else(no_person)
                    .and_then(|person| self.store.take(&text, &person));
                if let Err(reason) = taken {
                    self.store.set_note(Some(reason));
                }
            }
            DirectoryEffect::Refresh => self.store.read(self.community.clone(), None),
            DirectoryEffect::ClearNote => self.store.set_note(None),
            DirectoryEffect::Done => self.close(),
        }
    }

    /// Join the listing the sheet knows as `key`, as this page's person: one
    /// join at a time, and the sheet says why one did not start.
    fn join_listed(&mut self, key: &str) {
        let Some(listing) = self.store.listing(key) else {
            self.store
                .set_note(Some(tilo_sheets::word("directory-listing-gone")));
            return;
        };
        match self.store.joining() {
            Some(joining) if joining == key => self.store.set_note(Some(tilo_sheets::words(
                "directory-already-joining",
                &[("name", &listing.entry.name)],
            ))),
            // One join at a time: the first is still on its way.
            Some(_) => {}
            None => {
                self.door_origin = listing
                    .door
                    .as_ref()
                    .map(|door| door.origin().ascii_serialization());
                let joined = self
                    .person
                    .clone()
                    .ok_or_else(no_person)
                    .and_then(|person| self.store.join(key, &listing, &person));
                if let Err(reason) = joined {
                    self.store.set_note(Some(reason));
                }
            }
        }
    }

    fn close(&mut self) {
        self.presentation.close();
        self.content = None;
        self.replaced = true;
    }
}

/// Why nothing was asked: a page that named no person joins nowhere as
/// anybody.
fn no_person() -> String {
    "this page has no identity of its own yet, so it cannot join a workspace".to_owned()
}

impl Widget for PageDirectory {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.follow()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let content = self
            .content
            .as_mut()
            .map(|content| content.build_layout(taffy, theme, map));
        // Over the whole page, out of its flow: the sheet's scrim covers it.
        let style = Style {
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        let children: Vec<NodeId> = content.into_iter().collect();
        taffy.widget_node(style, &children, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(content) = self.content.as_mut()
            && let Some(child) = taffy.children(node).unwrap_or_default().first().copied()
        {
            content.apply_layout(taffy, child, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(content) = self.content.as_mut() {
            content.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(content) = self.content.as_mut() else {
            return;
        };
        let Some(&child) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        content.handle_event(event, &mut ctx.reborrow(ctx.path, child));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.content.as_deref().into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.content.as_mut() {
            Some(content) => vec![content.as_mut()],
            None => Vec::new(),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.replaced)
            | self
                .content
                .as_mut()
                .is_some_and(|content| content.take_structure_dirty())
    }

    fn traps_focus(&self) -> bool {
        self.presentation.is_drawn()
    }

    fn needs_redraw(&self) -> bool {
        self.content
            .as_ref()
            .is_some_and(|content| content.needs_redraw())
    }

    fn name(&self) -> &'static str {
        "PageDirectory"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
