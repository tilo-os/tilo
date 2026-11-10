//! The page's workspace history: the session's activity ([`tilo_session::activity`]) over the seams
//! this page provides — its store, its apps, the people presence knows, the mounts it writes
//! records as, and the activity model's code, which it fetches the first time someone opens
//! Activity ([`defer_activity_pane`]) so the start module does not carry it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash as _, Hasher as _};
use std::pin::Pin;
use std::rc::Rc;

use tilo_desktop_surface::{ActivityPaneCode, OpenActivity};
use tilo_session::platform::authors::{People, Writers};
use tilo_session::platform::pane::Pane;

/// Fetches the activity model's code; `None` when it could not.
pub type LoadActivity = fn() -> Pin<Box<dyn Future<Output = Option<OpenActivity>>>>;

/// Where the page has the activity model's code.
#[derive(Clone, Copy)]
enum PaneCode {
    Ready(OpenActivity),
    /// Fetched when the model first opens.
    Deferred(LoadActivity),
    Loading,
    /// The last fetch failed: that request is dismissed, the next one
    /// fetches again.
    Failed(LoadActivity),
}

thread_local! {
    static PANE_CODE: RefCell<PaneCode> = const { RefCell::new(PaneCode::Deferred(missing)) };
    /// The desktops that asked for the model while its code loads.
    static WAITING: RefCell<Vec<crate::desktop::Shared>> = const { RefCell::new(Vec::new()) };
}

/// No loader yet: the bundling crate supplies one before it mounts a desktop, and Activity stays
/// unavailable without it.
fn missing() -> Pin<Box<dyn Future<Output = Option<OpenActivity>>>> {
    Box::pin(std::future::ready(None))
}

/// Fetch the activity model's code with `load` the first time it opens, instead of linking it in.
pub fn defer_activity_pane(load: LoadActivity) {
    PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Deferred(load));
}

fn wait_for_pane(desktop: &crate::desktop::Shared) {
    WAITING.with_borrow_mut(|waiting| {
        if !waiting
            .iter()
            .any(|other| std::sync::Arc::ptr_eq(&other.woken, &desktop.woken))
        {
            waiting.push(desktop.clone());
        }
    });
}

/// The page's activity model's code, as the session asks it.
pub(super) struct PagePane {
    pub(super) desktop: crate::desktop::Shared,
}

impl Pane for PagePane {
    fn pane_code(&self) -> ActivityPaneCode {
        let code = PANE_CODE.with_borrow(|code| *code);
        match code {
            PaneCode::Ready(open) => ActivityPaneCode::Ready(open),
            PaneCode::Loading => {
                wait_for_pane(&self.desktop);
                ActivityPaneCode::Loading
            }
            PaneCode::Deferred(load) => {
                PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Loading);
                wait_for_pane(&self.desktop);
                wasm_bindgen_futures::spawn_local(async move {
                    let loaded = load().await;
                    PANE_CODE.with_borrow_mut(|code| {
                        *code = loaded.map_or(PaneCode::Failed(load), PaneCode::Ready);
                    });
                    for desktop in WAITING.take() {
                        desktop.wake();
                    }
                    crate::browser::wake_desktops();
                });
                ActivityPaneCode::Loading
            }
            PaneCode::Failed(load) => {
                PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Deferred(load));
                ActivityPaneCode::Unavailable
            }
        }
    }
}

/// The name and colour presence shows for a person, by the principal they
/// write as: here now, else as they were when last seen; this page's own
/// person as the top bar does, never by the key they write as.
pub(super) fn person_of(
    here: &tilo_desktop::here::Here,
    people: &Rc<RefCell<HashMap<String, crate::desktop::PersonIdentity>>>,
    own: String,
) -> People {
    let here = Rc::clone(here);
    let people = Rc::clone(people);
    Rc::new(move |user: &str| {
        let here = here.borrow();
        here.person(user)
            .or_else(|| people.borrow().get(user).cloned())
            .or_else(|| (user == own).then(|| crate::shell::own_identity(&here)))
    })
}

/// What moves when a record of the workspace did: the records its windows show, by their changes.
///
/// The store the page keeps is its own to drive, and this is how its activity learns that what it
/// read is stale instead of reading the documents again on every frame.
pub(super) fn records_moved(shared: &crate::desktop::Shared) -> Rc<dyn Fn() -> u64> {
    let shared = shared.clone();
    Rc::new(move || {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut records = shared.bound_records();
        records.sort_unstable_by_key(|(tab, _)| *tab);
        let stores = shared.stores.borrow();
        for (_, checkout) in records {
            checkout.id.as_str().hash(&mut hasher);
            for store in stores.iter() {
                if let Some(heads) = store.read(
                    &checkout.id,
                    ui::automerge::transaction::Transactable::base_heads,
                ) {
                    heads.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    })
}

/// Who the records this page's mounts wrote belong to: each record's document writer is noted as
/// one of this page's own, and the people presence knows are named as it shows them.
pub(super) fn writers(people: People, author: &str) -> Writers {
    let mine = author.to_owned();
    Writers {
        noted: tilo_session::platform::authors::new_noted(move |actor| {
            crate::page_authors::note_mount(Some(actor), &mine, None);
        }),
        register: tilo_session::platform::authors::new_register(move |authors| {
            crate::page_authors::register(authors, &[], &*people);
        }),
    }
}

impl crate::browser::Desktop {
    /// Take up what the history wrote back and what the person opened from it there, and let a
    /// model describe the desk's changes once one answers.
    pub(super) fn serve_activity(&self) {
        if let Some(tree) = self.activity.take_restored() {
            self.handle.replace_tree(tree);
        }
        // The app a history row names opens on what the row is: the page mounts it as it does for a
        // notice's choice, through the window's own open.
        for (_, target) in self.activity.take_openings() {
            let Some(app) = target.app_id.clone() else {
                continue;
            };
            self.requests
                .borrow_mut()
                .push(crate::shell::HostRequest::ShowInApp {
                    app,
                    resource: target.id.clone(),
                });
        }
        self.activity.serve_following();
        let described = self.activity.describe();
        if self.activity.take_woken() || described {
            crate::browser::wake_desktops();
        }
    }
}
