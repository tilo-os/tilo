//! The page's side of browser storage: a synchronous mirror of each
//! profile's app files, and change notification across tabs.
//!
//! # The mirror
//!
//! OPFS answers only asynchronously, but app code that loads and saves its
//! own files is synchronous on every other target. A host that calls
//! [`mirror`] before mounting apps loads the `data`, `config` and `state`
//! trees of every app in the profile into page memory. From then on a
//! [`Dir`](crate::Dir) on one of those locations answers from the mirror:
//!
//! - [`blocking`](crate::blocking) reads, writes, renames and removals work,
//!   and change the mirror at once;
//! - every change is written through to OPFS in order, one at a time, under
//!   the profile's Web Lock ([`pending`], [`flush`], [`take_failure`]);
//! - the async operations on those locations go through the same queue and
//!   resolve when their change is in OPFS.
//!
//! Caches (`<app>/cache`) are not mirrored: they may be large, and only the
//! async API reaches them. Blocking locks are unsupported on the web: a lock
//! across tabs can only be awaited.
//!
//! Across tabs, the mirror is kept current by the notifications below; two
//! tabs replacing the same file concurrently leave the last one committed.
//! No-clobber creation and renames are decided by the page's mirror.
//!
//! # Notifications
//!
//! Every change `tilo-files` commits to a profile's OPFS tree is announced on
//! the `BroadcastChannel` `tilo-files:<profile>`. Other tabs apply it to
//! their mirror and report it to their [`Watch`](crate::Watch)es; the tab
//! that made it reports it at once.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{BroadcastChannel, MessageEvent};

use crate::watch::{ChangeKind, Watch, Watchers};
use crate::{Entry, FsError, Kind, Meta, VPath, memory, opfs};

type UpdateId = (String, VPath, String);
type UpdateResult = Rc<RefCell<UpdateState>>;

#[derive(Default)]
struct UpdateState {
    result: Option<Result<(), FsError>>,
    waiters: Vec<Waker>,
}

thread_local! {
    static UPDATES: RefCell<HashMap<UpdateId, UpdateResult>> = RefCell::default();
}

/// Start a locked, durable read/modify/write against a browser Dir capability.
/// `operation` identifies the caller's immutable edits, not a storage namespace.
/// Returns false while pending. Retry with the same arguments to consume the
/// committed result or error exactly once. Reads bypass the synchronous mirror;
/// the mirror changes only after commit. The closure must not reenter file writes
/// in this profile, whose writer lock it holds across awaits.
///
/// # Errors
/// Returns backend or closure failures; non-OPFS directories are unsupported.
pub fn update_atomic_blocking<F, Fut>(
    dir: &crate::Dir,
    path: &VPath,
    operation: &str,
    update: F,
) -> Result<bool, FsError>
where
    F: FnOnce(Option<Vec<u8>>) -> Fut + 'static,
    Fut: Future<Output = Result<Vec<u8>, FsError>> + 'static,
{
    let crate::Root::Opfs(scope) = &dir.root else {
        return Err(FsError::Unsupported {
            feature: crate::Feature::Files,
            reason: "acknowledged browser updates require an OPFS directory",
        });
    };
    crate::below_root(path)?;
    let id = (
        scope.profile().to_owned(),
        scope.prefix().join(path),
        operation.to_owned(),
    );
    UPDATES.with_borrow_mut(|updates| {
        if let Some(result) = updates.get(&id) {
            let completed = result.borrow().result.clone();
            if let Some(result) = completed {
                updates.remove(&id);
                return result.map(|()| true);
            }
            return Ok(false);
        }
        let result = Rc::new(RefCell::new(UpdateState::default()));
        updates.insert(id, Rc::clone(&result));
        let scope = scope.clone();
        let path = path.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let completed = scope.update_atomic(&path, update).await;
            let waiters = {
                let mut state = result.borrow_mut();
                state.result = Some(completed);
                std::mem::take(&mut state.waiters)
            };
            for waiter in waiters {
                waiter.wake();
            }
        });
        Ok(false)
    })
}

/// The per-app slots the mirror holds, as named by `tilo-paths`.
const MIRRORED: [&str; 3] = ["data", "config", "state"];

thread_local! {
    static HUBS: RefCell<HashMap<String, Rc<Hub>>> = RefCell::new(HashMap::new());
}

/// Load `profile`'s app files (`<app>/data`, `/config` and `/state`) into
/// page memory, so [`blocking`](crate::blocking) calls on those locations
/// work. Call once before mounting apps; later calls return at once.
///
/// # Errors
///
/// [`FsError::Unsupported`] without OPFS, [`FsError::InvalidPath`] for a
/// profile that is not one path component, or a failure reading the tree.
pub async fn mirror(profile: &str) -> Result<(), FsError> {
    let hub = hub(profile)?;
    if hub.mirror.borrow().is_some() {
        return Ok(());
    }
    let tree = memory::Scope::fresh();
    let root = opfs::Scope::new(profile.to_owned(), VPath::root())?;
    load(&root, &tree).await?;
    // Another call may have finished first; keep the mirror it made.
    if hub.mirror.borrow().is_none() {
        *hub.mirror.borrow_mut() = Some(Rc::new(Mirror {
            tree,
            root,
            queue: RefCell::default(),
            in_flight: Cell::new(false),
            failure: RefCell::default(),
            idle: RefCell::default(),
        }));
    }
    Ok(())
}

/// Whether [`mirror`] has loaded `profile` in this page.
#[must_use]
pub fn is_mirrored(profile: &str) -> bool {
    existing_hub(profile).is_some_and(|hub| hub.mirror.borrow().is_some())
}

/// Changes to `profile`'s mirror not yet committed to OPFS, including one
/// being written, plus explicit updates awaiting a consume-once acknowledgement.
#[must_use]
pub fn pending(profile: &str) -> usize {
    let updates = UPDATES.with_borrow(|updates| {
        updates
            .keys()
            .filter(|(owner, _, _)| owner == profile)
            .count()
    });
    updates.saturating_add(mirror_of(profile).map_or(0, |mirror| {
        mirror
            .queue
            .borrow()
            .len()
            .saturating_add(usize::from(mirror.in_flight.get()))
    }))
}

/// The latest change to `profile`'s mirror that OPFS refused, clearing it.
/// The mirror keeps the refused contents; the next write of the same file
/// tries again.
#[must_use]
pub fn take_failure(profile: &str) -> Option<FsError> {
    mirror_of(profile).and_then(|mirror| mirror.failure.borrow_mut().take())
}

/// Waits until every change made to `profile`'s mirror so far is in OPFS.
///
/// # Errors
///
/// Explicit update failures remain visible until the matching synchronous
/// retry consumes them. Queued mirror failures since [`take_failure`] are cleared.
pub async fn flush(profile: &str) -> Result<(), FsError> {
    let updates: Vec<_> = UPDATES.with_borrow(|updates| {
        updates
            .iter()
            .filter(|((owner, _, _), _)| owner == profile)
            .map(|(_, state)| Rc::clone(state))
            .collect()
    });
    for state in updates {
        std::future::poll_fn(|cx| {
            let mut state = state.borrow_mut();
            if let Some(result) = &state.result {
                Poll::Ready(result.clone())
            } else {
                state.waiters.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await?;
    }
    let Some(mirror) = mirror_of(profile) else {
        return Ok(());
    };
    Idle {
        mirror: Rc::clone(&mirror),
    }
    .await;
    mirror.failure.borrow_mut().take().map_or(Ok(()), Err)
}

/// One profile's notifications and, once loaded, its mirror.
struct Hub {
    watchers: Watchers,
    mirror: RefCell<Option<Rc<Mirror>>>,
    channel: Option<BroadcastChannel>,
}

fn existing_hub(profile: &str) -> Option<Rc<Hub>> {
    HUBS.with_borrow(|hubs| hubs.get(profile).cloned())
}

fn mirror_of(profile: &str) -> Option<Rc<Mirror>> {
    existing_hub(profile).and_then(|hub| hub.mirror.borrow().clone())
}

fn hub(profile: &str) -> Result<Rc<Hub>, FsError> {
    if let Some(hub) = existing_hub(profile) {
        return Ok(hub);
    }
    // Validates the profile the way every OPFS location does.
    opfs::Scope::new(profile.to_owned(), VPath::root())?;
    let channel = BroadcastChannel::new(&format!("tilo-files:{profile}")).ok();
    let hub = Rc::new(Hub {
        watchers: Watchers::default(),
        mirror: RefCell::new(None),
        channel,
    });
    if let Some(channel) = &hub.channel {
        let owner = profile.to_owned();
        let receive = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(hub) = existing_hub(&owner) {
                hub.receive(&event.data());
            }
        });
        channel.set_onmessage(Some(receive.as_ref().unchecked_ref()));
        // The hub lives as long as the page.
        receive.forget();
    }
    HUBS.with_borrow_mut(|hubs| hubs.insert(profile.to_owned(), Rc::clone(&hub)));
    Ok(hub)
}

/// A change as it crosses tabs.
enum Op {
    Write(VPath, Vec<u8>),
    Remove(VPath),
    CreateDir(VPath),
    Rename(VPath, VPath),
}

impl Op {
    /// What watchers are told, as full paths in the profile.
    fn changes(&self) -> Vec<(&VPath, ChangeKind)> {
        match self {
            Self::Write(path, _) | Self::CreateDir(path) => vec![(path, ChangeKind::Changed)],
            Self::Remove(path) => vec![(path, ChangeKind::Removed)],
            Self::Rename(from, to) => {
                vec![(from, ChangeKind::Removed), (to, ChangeKind::Changed)]
            }
        }
    }

    fn to_js(&self) -> JsValue {
        let message = Object::new();
        let set = |key: &str, value: &JsValue| {
            let _ = Reflect::set(&message, &JsValue::from_str(key), value);
        };
        let (op, path) = match self {
            Self::Write(path, bytes) => {
                set("bytes", &Uint8Array::from(bytes.as_slice()));
                ("write", path)
            }
            Self::Remove(path) => ("remove", path),
            Self::CreateDir(path) => ("dir", path),
            Self::Rename(from, to) => {
                set("to", &JsValue::from_str(to.as_str()));
                ("rename", from)
            }
        };
        set("op", &JsValue::from_str(op));
        set("path", &JsValue::from_str(path.as_str()));
        message.into()
    }

    fn from_js(message: &JsValue) -> Option<Self> {
        let get = |key: &str| Reflect::get(message, &JsValue::from_str(key)).ok();
        let text = |key: &str| get(key)?.as_string();
        let path = VPath::new(&text("path")?).ok()?;
        Some(match text("op")?.as_str() {
            "write" => {
                let bytes = get("bytes")?.dyn_into::<Uint8Array>().ok()?.to_vec();
                Self::Write(path, bytes)
            }
            "remove" => Self::Remove(path),
            "dir" => Self::CreateDir(path),
            "rename" => Self::Rename(path, VPath::new(&text("to")?).ok()?),
            _ => return None,
        })
    }
}

impl Hub {
    fn notify(&self, op: &Op) {
        for (path, kind) in op.changes() {
            self.watchers.notify(path.as_str(), kind).wake();
        }
    }

    fn announce(&self, op: &Op) {
        if let Some(channel) = &self.channel {
            let _ = channel.post_message(&op.to_js());
        }
    }

    /// Another tab committed `message`: bring the mirror up to date and
    /// tell the watchers.
    fn receive(&self, message: &JsValue) {
        let Some(op) = Op::from_js(message) else {
            return;
        };
        if let Some(mirror) = self.mirror.borrow().as_ref() {
            mirror.apply_remote(&op);
        }
        self.notify(&op);
    }
}

/// A profile's mirrored app files and the queue writing them to OPFS.
struct Mirror {
    /// The profile's tree; its root is the profile's OPFS directory.
    tree: memory::Scope,
    root: opfs::Scope,
    queue: RefCell<VecDeque<Queued>>,
    in_flight: Cell<bool>,
    failure: RefCell<Option<FsError>>,
    idle: RefCell<Vec<Waker>>,
}

struct Queued {
    op: Op,
    done: Option<Rc<Slot>>,
}

impl Mirror {
    /// Queue `op` for OPFS, replacing a queued, unobserved write of the same
    /// file, and start writing if idle.
    fn enqueue(self: &Rc<Self>, hub: &Rc<Hub>, op: Op, done: Option<Rc<Slot>>) {
        let mut queue = self.queue.borrow_mut();
        if let (Op::Write(path, bytes), None) = (&op, &done)
            && let Some(Queued {
                op: Op::Write(queued, queued_bytes),
                done: None,
            }) = queue.back_mut()
            && queued == path
        {
            queued_bytes.clone_from(bytes);
            return;
        }
        queue.push_back(Queued { op, done });
        drop(queue);
        if !self.in_flight.get() {
            self.in_flight.set(true);
            let (mirror, hub) = (Rc::clone(self), Rc::clone(hub));
            wasm_bindgen_futures::spawn_local(async move { mirror.drain(&hub).await });
        }
    }

    async fn drain(&self, hub: &Hub) {
        loop {
            let next = self.queue.borrow_mut().pop_front();
            let Some(Queued { op, done }) = next else {
                break;
            };
            let result = self.commit(&op).await;
            match &result {
                Ok(()) => hub.announce(&op),
                Err(error) => *self.failure.borrow_mut() = Some(error.clone()),
            }
            if let Some(slot) = done {
                slot.complete(result);
            }
        }
        self.in_flight.set(false);
        for waker in self.idle.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    async fn commit(&self, op: &Op) -> Result<(), FsError> {
        match op {
            Op::Write(path, bytes) => self.root.write_atomic(path, bytes).await,
            Op::Remove(path) => match self.root.remove(path).await {
                Err(FsError::NotFound) => Ok(()),
                other => other,
            },
            Op::CreateDir(path) => self.root.create_dir_all(path).await,
            Op::Rename(from, to) => self.root.rename_noreplace(from, to).await,
        }
    }

    fn apply_remote(&self, op: &Op) {
        let _ = match op {
            Op::Write(path, bytes) if mirrored(path) => self.tree.write_atomic(path, bytes),
            Op::Remove(path) if mirrored(path) => self.tree.remove(path),
            Op::CreateDir(path) if mirrored(path) => self.tree.create_dir_all(path),
            Op::Rename(from, to) if mirrored(from) && mirrored(to) => {
                self.tree.rename_noreplace(from, to)
            }
            _ => Ok(()),
        };
    }
}

/// Whether a full path in a profile is below a mirrored slot of an app.
fn mirrored(path: &VPath) -> bool {
    let mut parts = path.components();
    parts.next().is_some() && parts.next().is_some_and(|slot| MIRRORED.contains(&slot))
}

/// Loads every mirrored slot of every app below `root` into `tree`.
async fn load(root: &opfs::Scope, tree: &memory::Scope) -> Result<(), FsError> {
    let apps = match root.list(&VPath::root()).await {
        Ok(apps) => apps,
        Err(FsError::NotFound) => return Ok(()),
        Err(error) => return Err(error),
    };
    for app in apps.into_iter().filter(|entry| entry.kind == Kind::Dir) {
        for slot in MIRRORED {
            let Ok(base) = VPath::new(&format!("{}/{slot}", app.name)) else {
                continue;
            };
            let mut pending = vec![base];
            while let Some(dir) = pending.pop() {
                let entries = match root.list(&dir).await {
                    Ok(entries) => entries,
                    Err(FsError::NotFound) => continue,
                    Err(error) => return Err(error),
                };
                tree.create_dir_all(&dir)?;
                for entry in entries {
                    let Ok(path) = VPath::new(&entry.name).map(|name| dir.join(&name)) else {
                        continue;
                    };
                    match entry.kind {
                        Kind::File => {
                            let (bytes, modified) = root.read_with_modified(&path).await?;
                            tree.load_file(&path, bytes, modified);
                        }
                        Kind::Dir => pending.push(path),
                        Kind::Other => {}
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolves once a mirror has nothing left to write.
struct Idle {
    mirror: Rc<Mirror>,
}

impl Future for Idle {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.mirror.queue.borrow().is_empty() && !self.mirror.in_flight.get() {
            Poll::Ready(())
        } else {
            self.mirror.idle.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// The outcome of one queued change, for the async call that made it.
#[derive(Default)]
struct Slot {
    result: RefCell<Option<Result<(), FsError>>>,
    waker: RefCell<Option<Waker>>,
}

impl Slot {
    fn complete(&self, result: Result<(), FsError>) {
        *self.result.borrow_mut() = Some(result);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

struct Committed(Rc<Slot>);

impl Future for Committed {
    type Output = Result<(), FsError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(result) = self.0.result.borrow_mut().take() {
            return Poll::Ready(result);
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// A [`Dir`](crate::Dir) on a mirrored location: the mirror below the Dir's
/// prefix.
#[derive(Clone)]
pub(crate) struct View {
    hub: Rc<Hub>,
    mirror: Rc<Mirror>,
    /// The Dir's prefix in the profile.
    prefix: VPath,
    scope: memory::Scope,
}

/// The mirror view of an OPFS [`Dir`](crate::Dir), when its profile is
/// mirrored and its location is a mirrored slot.
pub(crate) fn view(scope: &opfs::Scope) -> Option<View> {
    let prefix = scope.prefix();
    if !mirrored(prefix) {
        return None;
    }
    let hub = existing_hub(scope.profile())?;
    let mirror = hub.mirror.borrow().clone()?;
    Some(View {
        scope: mirror.tree.below(prefix),
        prefix: prefix.clone(),
        hub,
        mirror,
    })
}

impl View {
    fn full(&self, path: &VPath) -> VPath {
        self.prefix.join(path)
    }

    /// Tell this tab's watchers and queue the change for OPFS.
    fn changed(&self, op: Op, done: Option<Rc<Slot>>) {
        self.hub.notify(&op);
        self.mirror.enqueue(&self.hub, op, done);
    }

    pub(crate) fn read(&self, path: &VPath) -> Result<Vec<u8>, FsError> {
        self.scope.read(path)
    }

    pub(crate) fn list(&self, path: &VPath) -> Result<Vec<Entry>, FsError> {
        self.scope.list(path)
    }

    pub(crate) fn metadata(&self, path: &VPath) -> Result<Meta, FsError> {
        self.scope.metadata(path)
    }

    pub(crate) fn write_atomic(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        self.write_with(path, bytes, None)
    }

    fn write_with(
        &self,
        path: &VPath,
        bytes: &[u8],
        done: Option<Rc<Slot>>,
    ) -> Result<(), FsError> {
        self.scope.write_atomic(path, bytes)?;
        self.changed(Op::Write(self.full(path), bytes.to_vec()), done);
        Ok(())
    }

    pub(crate) fn create_new(&self, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
        self.create_new_with(path, bytes, None)
    }

    fn create_new_with(
        &self,
        path: &VPath,
        bytes: &[u8],
        done: Option<Rc<Slot>>,
    ) -> Result<bool, FsError> {
        let created = self.scope.create_new(path, bytes)?;
        if created {
            self.changed(Op::Write(self.full(path), bytes.to_vec()), done);
        } else if let Some(done) = done {
            done.complete(Ok(()));
        }
        Ok(created)
    }

    pub(crate) fn rename_noreplace(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        self.rename_with(from, to, None)
    }

    fn rename_with(&self, from: &VPath, to: &VPath, done: Option<Rc<Slot>>) -> Result<(), FsError> {
        self.scope.rename_noreplace(from, to)?;
        self.changed(Op::Rename(self.full(from), self.full(to)), done);
        Ok(())
    }

    pub(crate) fn remove(&self, path: &VPath) -> Result<(), FsError> {
        self.remove_with(path, None)
    }

    fn remove_with(&self, path: &VPath, done: Option<Rc<Slot>>) -> Result<(), FsError> {
        self.scope.remove(path)?;
        self.changed(Op::Remove(self.full(path)), done);
        Ok(())
    }

    pub(crate) fn create_dir_all(&self, path: &VPath) -> Result<(), FsError> {
        self.create_dir_with(path, None)
    }

    fn create_dir_with(&self, path: &VPath, done: Option<Rc<Slot>>) -> Result<(), FsError> {
        self.scope.create_dir_all(path)?;
        self.changed(Op::CreateDir(self.full(path)), done);
        Ok(())
    }

    // The async operations: the same change, resolved once it is in OPFS.

    pub(crate) async fn write_atomic_committed(
        &self,
        path: &VPath,
        bytes: &[u8],
    ) -> Result<(), FsError> {
        let slot = Rc::new(Slot::default());
        self.write_with(path, bytes, Some(Rc::clone(&slot)))?;
        Committed(slot).await
    }

    pub(crate) async fn create_new_committed(
        &self,
        path: &VPath,
        bytes: &[u8],
    ) -> Result<bool, FsError> {
        let slot = Rc::new(Slot::default());
        let created = self.create_new_with(path, bytes, Some(Rc::clone(&slot)))?;
        Committed(slot).await?;
        Ok(created)
    }

    pub(crate) async fn rename_committed(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        let slot = Rc::new(Slot::default());
        self.rename_with(from, to, Some(Rc::clone(&slot)))?;
        Committed(slot).await
    }

    pub(crate) async fn remove_committed(&self, path: &VPath) -> Result<(), FsError> {
        let slot = Rc::new(Slot::default());
        self.remove_with(path, Some(Rc::clone(&slot)))?;
        Committed(slot).await
    }

    pub(crate) async fn create_dir_committed(&self, path: &VPath) -> Result<(), FsError> {
        let slot = Rc::new(Slot::default());
        self.create_dir_with(path, Some(Rc::clone(&slot)))?;
        Committed(slot).await
    }
}

/// A watch on an OPFS location, mirrored or not.
///
/// # Errors
///
/// [`FsError::InvalidPath`] for a profile that is not one path component.
pub(crate) fn watch(scope: &opfs::Scope, path: &VPath) -> Result<Watch, FsError> {
    let hub = hub(scope.profile())?;
    let inbox = hub
        .watchers
        .register(&scope.prefix().join(path), scope.prefix());
    Ok(Watch::new(inbox, Box::new(())))
}

/// A change made directly in OPFS (outside the mirror) has committed: tell
/// this tab's watchers and the other tabs.
pub(crate) fn committed_directly(scope: &opfs::Scope, change: &Direct<'_>) {
    let Ok(hub) = hub(scope.profile()) else {
        return;
    };
    let full = |path: &VPath| scope.prefix().join(path);
    let op = match change {
        Direct::Write(path, bytes) => Op::Write(full(path), bytes.to_vec()),
        Direct::Remove(path) => Op::Remove(full(path)),
        Direct::CreateDir(path) => Op::CreateDir(full(path)),
        Direct::Rename(from, to) => Op::Rename(full(from), full(to)),
    };
    if let Some(mirror) = hub.mirror.borrow().as_ref() {
        mirror.apply_remote(&op);
    }
    hub.notify(&op);
    hub.announce(&op);
}

/// A change [`committed_directly`] reports, relative to the Dir.
pub(crate) enum Direct<'a> {
    Write(&'a VPath, &'a [u8]),
    Remove(&'a VPath),
    CreateDir(&'a VPath),
    Rename(&'a VPath, &'a VPath),
}
