//! `tilolang run`: a package under development, in a window of its own.
//!
//! The host runs the package straight from its directory, not from sources embedded at build
//! time. When a `.tl` file of the package or of a package it imports changes, the packages are
//! compiled again and every replica loads the new modules: resources keep their state (they
//! live in the room's record, by declaration id), and the open views are opened again over the
//! new code. A build with errors keeps the last good one running; the terminal lists the
//! errors and the window says it has some.
//!
//! `--peers N` shows N replicas of one room side by side, each its own collaborator with its own
//! copy of the record. A change committed in one replica is merged into the others at once, as
//! sync would deliver it, so concurrent edits and presence of several people can be tried on one
//! machine.
//!
//! The window is a plain toolkit window ([`ui::App`]): it attaches to no Tilo session, opens no
//! workspace, and its commits go through a door in this module. The room is kept between runs in
//! the state directory (`$XDG_STATE_HOME/tilolang-dev/…/room.automerge`); `--fresh` starts empty.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use tilo_automerge::ApplyIncoming as _;
use tilolang_tilo::action::register_actions;
use tilolang_tilo::tilolang::source::{BuildError, Stamp, compile_dirs, write_ids};
use tilolang_tilo::tilolang::{Compiled, OsEntropy, cli::RunRequest};
use ui::automerge::{AutoCommit, ChangeHash, ROOT};
use ui::resources::PublishesResources as _;
use ui::tilo_actions::{
    ActionContext, ActionInvoker, ActionRegistry, ControlInvocation, apply_local_with_context,
};
use ui::widget::arrange::{Fit, Row, Slot};
use ui::widget::controls::ActionControls as _;
use ui::widget::{AppModel, Element, ModelPath, Widget};

use crate::page::TilolangPage;

/// How often the sources are looked at for changes.
const POLL: Duration = Duration::from_millis(250);

/// The packages under development and their latest good build.
pub struct DevPackages {
    dirs: Vec<PathBuf>,
    stamp: Stamp,
    generation: u64,
    compiled: Vec<Compiled>,
    error: Option<String>,
}

impl DevPackages {
    /// Compile the packages in `dirs` (dependencies first, the package being developed last).
    ///
    /// # Errors
    /// The packages do not compile; the message lists every error by file.
    pub fn load(dirs: Vec<PathBuf>) -> Result<Self, String> {
        let mut dev = Self {
            stamp: Stamp::of(&dirs),
            dirs,
            generation: 0,
            compiled: Vec::new(),
            error: None,
        };
        dev.build()?;
        Ok(dev)
    }

    /// The directories this build reads.
    #[must_use]
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Counts the good builds: a page runs the build it last saw.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn compiled(&self) -> &[Compiled] {
        &self.compiled
    }

    /// The errors of the last build, when it failed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The app id of the package being developed.
    #[must_use]
    pub fn main_app(&self) -> Option<String> {
        self.compiled.last().map(|c| c.meta.app.clone())
    }

    /// Compile again when a source file changed since the last build. Returns whether anything
    /// a window shows changed: a new build, or errors that appeared or went away.
    pub fn refresh(&mut self) -> bool {
        let now = Stamp::of(&self.dirs);
        if now == self.stamp {
            return false;
        }
        self.stamp = now;
        let had_error = self.error.is_some();
        match self.build() {
            Ok(()) => true,
            Err(message) => {
                eprintln!("{message}");
                let changed = !had_error || self.error.as_deref() != Some(message.as_str());
                self.error = Some(message);
                changed
            }
        }
    }

    fn build(&mut self) -> Result<(), String> {
        let built =
            compile_dirs(&self.dirs, &[], &mut OsEntropy::default()).map_err(|e| match e {
                BuildError::Read(message) => message,
                e @ BuildError::Diagnostics { .. } => e.to_string(),
            })?;
        // New declarations got fresh ids: lock them now, so the next build and every replica
        // agree on them.
        for package in &built {
            write_ids(package)?;
        }
        self.compiled = built.into_iter().map(|b| b.compiled).collect();
        self.generation = self.generation.saturating_add(1);
        self.error = None;
        Ok(())
    }
}

/// One replica of the room: its own copy of the record and a window of the host over it.
pub struct Replica {
    doc: Rc<RefCell<AutoCommit>>,
    page: TilolangPage,
    author: String,
}

impl Replica {
    /// A replica of `doc` for `author`, running the packages under development.
    #[must_use]
    pub fn new(author: &str, doc: AutoCommit, dev: &Rc<RefCell<DevPackages>>) -> Self {
        Self {
            doc: Rc::new(RefCell::new(doc)),
            page: TilolangPage::developing(author.to_owned(), Rc::clone(dev)),
            author: author.to_owned(),
        }
    }

    /// The record this replica holds.
    #[must_use]
    pub fn doc(&self) -> Rc<RefCell<AutoCommit>> {
        Rc::clone(&self.doc)
    }

    #[must_use]
    pub fn page(&self) -> &TilolangPage {
        &self.page
    }

    pub fn page_mut(&mut self) -> &mut TilolangPage {
        &mut self.page
    }

    /// One frame of the window's model: sync from the record, take what the person did, and
    /// commit what that planned.
    pub fn frame(&mut self) -> bool {
        let doc = Rc::clone(&self.doc);
        self.page.sync(&*doc.borrow(), &ROOT);
        let changed = self.page.update(&*doc.borrow(), &ROOT);
        let committed = self.door();
        if committed {
            self.page.sync(&*doc.borrow(), &ROOT);
        }
        changed || committed
    }

    /// Commit every invocation the window handed over, as the action door does: built by the
    /// published action, applied to the record as this replica's person, and answered.
    pub fn door(&mut self) -> bool {
        let mut committed = false;
        while let Some(invocation) = self.page.take_control_invocation() {
            let outcome = self.commit(&invocation);
            committed |= outcome.is_ok();
            self.page.door_answered(outcome.err());
        }
        committed
    }

    fn commit(&self, invocation: &ControlInvocation) -> Result<(), String> {
        let mut doc = self.doc.borrow_mut();
        let resources = self.page.resources(&doc);
        let spec = resources
            .iter()
            .find(|resource| resource.id.as_str() == invocation.address.resource)
            .and_then(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|spec| spec.id == invocation.address.action)
            })
            .ok_or_else(|| format!("nothing publishes {:?}", invocation.address))?;
        let encoded = spec
            .build_json(&invocation.arguments)
            .map_err(|error| error.to_string())?;
        let action = ActionRegistry::with(register_actions)
            .decode(&encoded)
            .map_err(|error| error.to_string())?;
        let context = ActionContext::for_resource("room", "tl.room", "room")
            .with_app(crate::APP_ID, "tilolang")
            .with_invoker(ActionInvoker::actor(self.author.clone()));
        apply_local_with_context(
            &mut doc,
            action.as_ref(),
            encoded,
            Some(self.author.as_bytes()),
            context,
        )
        .map_err(|error| error.to_string())?;
        doc.commit();
        Ok(())
    }
}

/// Merge every replica's changes into every other, as sync delivers them. Returns whether any
/// record changed.
pub fn merge_replicas(docs: &[Rc<RefCell<AutoCommit>>]) -> bool {
    let Some((first, rest)) = docs.split_first() else {
        return false;
    };
    let heads = |doc: &Rc<RefCell<AutoCommit>>| -> Vec<ChangeHash> { doc.borrow_mut().get_heads() };
    let before: Vec<Vec<ChangeHash>> = docs.iter().map(heads).collect();
    for other in rest {
        let _ = first.borrow_mut().merge_incoming(&mut other.borrow_mut());
    }
    for other in rest {
        let _ = other.borrow_mut().merge_incoming(&mut first.borrow_mut());
    }
    docs.iter().map(heads).collect::<Vec<_>>() != before
}

/// The widget of one replica: the host's page over the replica's own record instead of the
/// window's document.
struct Pane {
    replica: Replica,
}

impl Widget for Pane {
    fn name(&self) -> &'static str {
        "TilolangReplica"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.replica.page]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.replica.page]
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        let doc = Rc::clone(&self.replica.doc);
        self.replica.page.sync(&*doc.borrow(), &ROOT);
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let doc = Rc::clone(&self.replica.doc);
        let changed = self.replica.page.update(&*doc.borrow(), &ROOT);
        self.replica.door() || changed
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.replica.page.handle_event(event, ctx);
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.replica.page.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.replica.page.apply_layout(tree, node, origin);
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.replica.page.get_rect()
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.replica
            .page
            .draw(scene, transform, text, theme, presence);
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.replica.page.take_structure_dirty()
    }
}

/// Where the room of a package under development is kept between runs.
fn room_file(main: &Path) -> Option<PathBuf> {
    use std::hash::{Hash as _, Hasher as _};
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    let identity = main.canonicalize().unwrap_or_else(|_| main.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity.hash(&mut hasher);
    let name = identity
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(
        state
            .join("tilolang-dev")
            .join(format!("{name}-{:016x}", hasher.finish()))
            .join("room.automerge"),
    )
}

/// The room a run starts from: the one the last run left, unless `fresh`.
fn starting_room(file: Option<&Path>, fresh: bool) -> AutoCommit {
    let Some(file) = file else {
        return AutoCommit::new();
    };
    if fresh {
        let _ = std::fs::remove_file(file);
        return AutoCommit::new();
    }
    std::fs::read(file)
        .ok()
        .and_then(|bytes| AutoCommit::load(&bytes).ok())
        .unwrap_or_default()
}

/// The names the replicas' people go by.
#[must_use]
pub fn peer_name(index: usize) -> String {
    format!("peer-{}", index.saturating_add(1))
}

/// The window of `tilolang run`: the replicas side by side; it rebuilds the packages when their
/// sources change, merges the replicas and keeps the room.
pub struct Workbench {
    dev: Rc<RefCell<DevPackages>>,
    docs: Vec<Rc<RefCell<AutoCommit>>>,
    root: Element,
    changes: Option<Receiver<()>>,
    room: Option<PathBuf>,
    saved: Vec<ChangeHash>,
}

impl Workbench {
    /// `peers` replicas of `room`, running `dev`.
    #[must_use]
    pub fn new(dev: DevPackages, room: AutoCommit, peers: usize) -> Self {
        let dev = Rc::new(RefCell::new(dev));
        let mut room = room;
        let mut docs = Vec::new();
        // Every replica is a copy of the one room, as a person joining it receives it.
        let mut row = Row::row().fill_width().with_id("tilolang-replicas");
        for index in 0..peers.max(1) {
            let replica = Replica::new(&peer_name(index), room.fork(), &dev);
            docs.push(replica.doc());
            let mut slot = Slot::new(Box::new(Pane { replica }));
            slot.fit = Fit::fill(1.0);
            row = row.push_slot(slot);
        }
        Self {
            dev,
            docs,
            root: Box::new(row),
            changes: None,
            room: None,
            saved: Vec::new(),
        }
    }

    /// Keep the room in `file` whenever it changes.
    #[must_use]
    pub fn keeping_room_in(mut self, file: Option<PathBuf>) -> Self {
        self.room = file;
        self
    }

    /// Look for source changes on a thread that wakes the window when there are some.
    #[must_use]
    pub fn watching(mut self) -> Self {
        let (sender, receiver) = ui::wake::waking_channel();
        let dirs = self.dev.borrow().dirs().to_vec();
        std::thread::Builder::new()
            .name("tilolang-watch".into())
            .spawn(move || {
                let mut last = Stamp::of(&dirs);
                loop {
                    std::thread::sleep(POLL);
                    let now = Stamp::of(&dirs);
                    if now != last {
                        last = now;
                        if sender.send(()).is_err() {
                            return;
                        }
                    }
                }
            })
            .ok();
        self.changes = Some(receiver);
        self
    }

    /// The packages under development.
    #[must_use]
    pub fn dev(&self) -> Rc<RefCell<DevPackages>> {
        Rc::clone(&self.dev)
    }

    fn save(&mut self) {
        let (Some(file), Some(first)) = (&self.room, self.docs.first()) else {
            return;
        };
        let heads = first.borrow_mut().get_heads();
        if heads == self.saved {
            return;
        }
        let bytes = first.borrow_mut().save();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(file, bytes) {
            Ok(()) => self.saved = heads,
            Err(error) => tracing::warn!(%error, file = %file.display(), "the room was not kept"),
        }
    }
}

impl Widget for Workbench {
    fn name(&self) -> &'static str {
        "TilolangWorkbench"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.root.as_mut()]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let woken = self
            .changes
            .as_ref()
            .is_some_and(|changes| changes.try_iter().count() > 0);
        if woken {
            self.dev.borrow_mut().refresh();
        }
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.root.update(state, path);
        // Only a frame that changed something can have committed a change to exchange.
        let merged = changed && merge_replicas(&self.docs);
        if changed {
            self.save();
        }
        changed || merged
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.root.handle_event(event, ctx);
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.root.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.root.apply_layout(tree, node, origin);
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.root.get_rect()
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.root.draw(scene, transform, text, theme, presence);
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.root.take_structure_dirty()
    }
}

/// `tilolang run`: open the window of [`Workbench`] for `request` and run it until it is closed.
///
/// # Errors
/// The packages do not compile.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(request: &RunRequest) -> Result<(), String> {
    crate::i18n::ensure();
    let dev = DevPackages::load(request.dirs.clone())?;
    let main = request.dirs.last().cloned().unwrap_or_default();
    let room = room_file(&main);
    let doc = starting_room(room.as_deref(), request.fresh);
    let title = dev
        .compiled()
        .last()
        .map_or_else(|| "tilolang".to_owned(), |c| c.meta.title.clone());
    let peers = request.peers;
    ui::App::from_factory(ui::State::new(), move || -> Box<dyn Widget> {
        Box::new(
            Workbench::new(dev, doc, peers)
                .keeping_room_in(room)
                .watching(),
        )
    })
    .with_window_title(title)
    .with_window_app_id(crate::APP_ID)
    .run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use tilolang_tilo::runtime::UiNode;

    use super::*;
    use crate::i18n::t;
    use crate::reconcile::{UiEvent, display};

    const COUNTER: &str = include_str!("../../tl/counter/app.tl");
    const COUNTER_IDS: &str = include_str!("../../tl/counter/app.ids");

    /// A copy of the counter package in a scratch directory, removed when dropped.
    struct Package(PathBuf);

    impl Package {
        fn counter(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tilolang-dev-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("app.tl"), COUNTER).unwrap();
            std::fs::write(dir.join("app.ids"), COUNTER_IDS).unwrap();
            Self(dir)
        }

        fn write(&self, file: &str, text: &str) {
            std::fs::write(self.0.join(file), text).unwrap();
        }

        fn dev(&self) -> Rc<RefCell<DevPackages>> {
            Rc::new(RefCell::new(
                DevPackages::load(vec![self.0.clone()]).unwrap(),
            ))
        }
    }

    impl Drop for Package {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        out.push(node);
        for child in &node.children {
            walk(child, out);
        }
    }

    fn nodes(replica: &Replica) -> Vec<UiNode> {
        let mut out = Vec::new();
        walk(replica.page().shown().expect("an app is open"), &mut out);
        out.into_iter().cloned().collect()
    }

    /// The count the counter's heading shows.
    fn shown_count(replica: &Replica) -> String {
        nodes(replica)
            .iter()
            .find(|node| node.kind == "heading" || node.kind == "text")
            .and_then(|node| node.props.get("value").map(display))
            .expect("the counter shows its value")
    }

    fn labels(replica: &Replica) -> Vec<String> {
        nodes(replica)
            .iter()
            .filter(|node| node.kind == "button")
            .filter_map(|node| node.props.get("label").map(display))
            .collect()
    }

    fn press(replica: &mut Replica, label: &str) {
        let key = nodes(replica)
            .iter()
            .find(|node| {
                node.kind == "button"
                    && node
                        .props
                        .get("label")
                        .is_some_and(|shown| display(shown) == label)
            })
            .map_or_else(
                || panic!("no button {label}: {:?}", labels(replica)),
                |node| node.key.clone(),
            );
        replica.page().act(UiEvent::Press(key));
        replica.frame();
    }

    fn started(author: &str, doc: AutoCommit, dev: &Rc<RefCell<DevPackages>>) -> Replica {
        let mut replica = Replica::new(author, doc, dev);
        replica.frame();
        replica.frame();
        replica
    }

    #[test]
    fn the_package_under_development_opens_from_its_directory() {
        let package = Package::counter("opens");
        let dev = package.dev();
        let replica = started("peer-1", AutoCommit::new(), &dev);
        assert_eq!(replica.page().open_app(), Some("so.tilo.counter"));
        assert_eq!(shown_count(&replica), "0");
        assert_eq!(labels(&replica), ["-1", "+1"]);
    }

    #[test]
    fn a_saved_change_reloads_the_app_and_the_room_keeps_its_state() {
        let package = Package::counter("reload");
        let dev = package.dev();
        let mut replica = started("peer-1", AutoCommit::new(), &dev);
        for _ in 0..3 {
            press(&mut replica, "+1");
        }
        assert_eq!(shown_count(&replica), "3");

        package.write("app.tl", &COUNTER.replace("\"+1\"", "\"Add one\""));
        assert!(dev.borrow_mut().refresh(), "a new build");
        replica.frame();
        assert_eq!(labels(&replica), ["-1", "Add one"], "the new code runs");
        assert_eq!(shown_count(&replica), "3", "the room kept its state");
        press(&mut replica, "Add one");
        assert_eq!(shown_count(&replica), "4");
        assert!(!dev.borrow_mut().refresh(), "nothing changed since");
    }

    #[test]
    fn a_new_file_joins_the_package_and_a_new_declaration_is_locked_on_disk() {
        let package = Package::counter("newfile");
        let dev = package.dev();
        let mut replica = started("peer-1", AutoCommit::new(), &dev);
        press(&mut replica, "+1");
        package.write(
            "reset.tl",
            "/// Ten at once.\naction jump(c: Counter) {\n    c.value += 10\n}\n",
        );
        package.write(
            "app.tl",
            &COUNTER.replace(
                "button(\"+1\", on: increment(c), style: .primary, size: .large) as plus",
                "button(\"+1\", on: increment(c), style: .primary, size: .large) as plus\n        button(\"Jump\", on: jump(c)) as jump",
            ),
        );
        dev.borrow_mut().refresh();
        assert_eq!(dev.borrow().error(), None);
        replica.frame();
        assert_eq!(labels(&replica), ["-1", "+1", "Jump"]);
        press(&mut replica, "Jump");
        assert_eq!(shown_count(&replica), "11");
        let ids = std::fs::read_to_string(package.0.join("app.ids")).unwrap();
        assert!(ids.contains("\njump = "), "{ids}");
        assert!(ids.contains("\ncontrols#jump = "), "{ids}");
        for line in COUNTER_IDS.lines().filter(|line| !line.starts_with('#')) {
            assert!(ids.contains(line), "kept {line}");
        }
    }

    #[test]
    fn a_build_with_errors_keeps_the_last_good_one_and_says_so() {
        let package = Package::counter("errors");
        let dev = package.dev();
        let mut replica = started("peer-1", AutoCommit::new(), &dev);
        press(&mut replica, "+1");
        package.write("app.tl", &COUNTER.replace("c.value += 1", "c.nothing += 1"));
        assert!(dev.borrow_mut().refresh());
        let error = dev.borrow().error().map(str::to_owned).expect("an error");
        assert!(error.contains("app.tl:15:"), "{error}");
        replica.frame();
        assert_eq!(
            replica.page().notice(),
            Some(t("tilolang-dev-errors").as_str())
        );
        press(&mut replica, "+1");
        assert_eq!(shown_count(&replica), "2", "the last good build still runs");

        package.write("app.tl", COUNTER);
        assert!(dev.borrow_mut().refresh());
        replica.frame();
        assert_eq!(replica.page().notice(), None, "fixed");
        assert_eq!(shown_count(&replica), "2");
    }

    #[test]
    fn two_peers_see_each_others_presses_and_concurrent_ones_add_up() {
        let package = Package::counter("peers");
        let dev = package.dev();
        let mut room = AutoCommit::new();
        let mut ada = started(&peer_name(0), room.fork(), &dev);
        let mut grace = started(&peer_name(1), room.fork(), &dev);
        let docs = [ada.doc(), grace.doc()];
        merge_replicas(&docs);
        ada.frame();
        grace.frame();

        press(&mut ada, "+1");
        assert_eq!(shown_count(&grace), "0", "not merged yet");
        assert!(merge_replicas(&docs));
        grace.frame();
        assert_eq!(shown_count(&grace), "1");

        press(&mut ada, "+1");
        press(&mut grace, "+1");
        merge_replicas(&docs);
        ada.frame();
        grace.frame();
        assert_eq!(shown_count(&ada), "3");
        assert_eq!(shown_count(&grace), "3");
        assert!(!merge_replicas(&docs), "nothing left to exchange");
    }

    #[test]
    fn both_peers_follow_a_reload() {
        let package = Package::counter("peers-reload");
        let dev = package.dev();
        let mut room = AutoCommit::new();
        let mut ada = started(&peer_name(0), room.fork(), &dev);
        let mut grace = started(&peer_name(1), room.fork(), &dev);
        let docs = [ada.doc(), grace.doc()];
        press(&mut grace, "+1");
        merge_replicas(&docs);
        package.write("app.tl", &COUNTER.replace("\"-1\"", "\"Take one\""));
        dev.borrow_mut().refresh();
        ada.frame();
        grace.frame();
        for replica in [&ada, &grace] {
            assert_eq!(labels(replica), ["Take one", "+1"]);
            assert_eq!(shown_count(replica), "1");
        }
    }

    #[test]
    fn the_room_is_kept_between_runs_unless_fresh() {
        let package = Package::counter("room");
        let file = package.0.join("state").join("room.automerge");
        let dev = DevPackages::load(vec![package.0.clone()]).unwrap();
        let mut bench =
            Workbench::new(dev, AutoCommit::new(), 2).keeping_room_in(Some(file.clone()));
        let window = AutoCommit::new();
        bench.sync(&window, &ROOT);
        bench.update(&window, &ROOT);
        bench.sync(&window, &ROOT);
        bench.update(&window, &ROOT);
        assert!(file.is_file(), "the opened room was kept");
        let kept = starting_room(Some(&file), false);
        let records = tilolang_tilo::action::resource_ids(&kept, "so.tilo.counter");
        assert_eq!(records.len(), 1, "the counter's root");
        let fresh = starting_room(Some(&file), true);
        assert!(tilolang_tilo::action::resource_ids(&fresh, "so.tilo.counter").is_empty());
        assert!(!file.exists());
    }
}
