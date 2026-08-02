//! Resource + action-model status for the Software store.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](qos_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state stays plain
//! app code. Applying that split to the software store leaves **nothing** on the
//! action side, and this module records that finding rather than fabricate a
//! wishlist / favourites model just to have actions.
//!
//! ## Everything that changes the world is external
//!
//! Installing, updating and uninstalling an application are **package-manager**
//! side effects: each dispatches a real `flatpak install` / `flatpak update` /
//! `flatpak uninstall` to the background [`Worker`](crate::worker) (see
//! [`crate::worker::Request`] and [`crate::flatpak`]), which shells out off the
//! UI thread. Browsing and searching the remote catalog hit the Flathub API, and
//! ratings / download counts are fetched from that backend. None of these mutate
//! the shared CRDT — they change the local machine's package set or fill a
//! transient in-memory model — so by the recipe's rule none of them is an action.
//!
//! ## The document's install-set is a *projection*, not a user-authored record
//!
//! The one thing that could look like a document-mutating action is the install
//! state the store keeps in the CRDT: a `store` map holding `installed`
//! (app-id → version), `installing` (app-id → progress) and a first-run `seeded`
//! flag (see [`crate::doc::StoreDoc`]). It is genuinely written through
//! `AutoCommit`, and it genuinely replicates and undoes — but it is **not** an
//! action, for a reason the recipe's rule makes precise: it is a *projection of
//! external system state*, reconciled every `sync` from the live `flatpak list`
//! [probe](crate::probe) and the worker's real action outcomes, not a record the
//! user authors into a shared document. Modelling install / uninstall as a
//! replicated [`Action`](qos_actions::Action) would be actively **wrong**:
//! installation is a per-machine effect, so replaying an "installed X" envelope
//! on a collaborator would make their document claim an app is present that their
//! machine never installed. The install-set is therefore a local cache/mirror of
//! the package manager — the CRDT is merely how that mirror is stored so it
//! survives undo and a `--shots` cold start — and mirroring an external world is
//! external, not an action. The first-run `seeded` flag is one-shot presentation
//! bookkeeping for the same reason.
//!
//! ## There is no user-authored collection to migrate
//!
//! What *would* legitimately be actions — a user-authored wishlist, favourites,
//! pinned apps, or custom collections persisted in the document — **do not
//! exist** in this app. The store carries no such record set (the tests below
//! prove the document holds only `store.{installed,installing,seeded}` and none
//! of `wishlist`, `favourites`, `pinned`, `collections`, `settings`, …). The
//! recipe is explicit that fabricating a model just to have actions is the wrong
//! move, so this app does not.
//!
//! ## Presentation is never an action
//!
//! The active tab (Explore / Installed / Updates), the open detail page, the
//! search query and focus, the category filter, scroll offset, the responsive
//! navigation flap, and transient toasts are all per-peer view state held on the
//! [`SoftwareStore`](crate::app::SoftwareStore) widget. The recipe names "which
//! book is open, the scroll position, the current selection, an overlay being
//! open" as presentation that must *never* be an action.
//!
//! ## Consequence
//!
//! The software store exports **no** actions and publishes **no** Ctrl+K resource
//! tree: [`Widget::resources`](ui::widget::Widget::resources) stays empty and
//! [`take_committed_action`](ui::widget::Widget::take_committed_action) stays
//! `None` (both toolkit defaults, so no [`ui::ActionHost`] is embedded — there is
//! nothing for it to dispatch, and `main.rs` registers no action decoder). The
//! tests below prove each half of this split on the real production path.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use winit::event::{DeviceId, Modifiers, WindowEvent};

    use ui::widget::Widget;

    use crate::app::SoftwareStore;
    use crate::doc::StoreDoc;
    use crate::probe::ProbeResult;

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A cursor move through the real winit event path — exercises the live
    /// `Widget::handle_event` without triggering any hit-target action.
    fn cursor_at(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: device_id(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    /// Pump one event through the production `Widget::handle_event`.
    fn send(app: &mut SoftwareStore, doc: &mut AutoCommit, ev: &WindowEvent) {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &Modifiers::default(),
            doc,
            &ROOT,
            &mut handled,
            &mut focus,
            NodeId::new(0),
            &taffy,
        );
    }

    /// An offline store with its first `sync` run — the same state the live app
    /// (and the `--shots` path) reaches on its first frame: the one-time default
    /// seed applied, no live probe. Uses the offline worker, so no subprocess or
    /// network is ever touched.
    fn synced() -> (SoftwareStore, AutoCommit) {
        let mut app = SoftwareStore::new(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// The whole CRDT footprint is the store's projected install-state: a `store`
    /// map holding only `installed`, `installing` and the first-run `seeded`
    /// flag. There is no user-authored record collection anywhere — no wishlist,
    /// favourites, pinned apps, custom collections or settings map — so there is
    /// nothing for an action to mutate. Driven through the real `Widget::sync`.
    #[test]
    fn crdt_footprint_is_only_the_projected_install_state() {
        let (_app, doc) = synced();

        // The document holds exactly one structural key at the root: `store`.
        let root_keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            root_keys,
            vec!["store".to_string()],
            "the document holds only the store map, nothing else at the root: {root_keys:?}"
        );

        // Inside `store`, the only keys are the projected install-state and the
        // one-shot seed flag.
        let store = match doc.get(&ROOT, "store").unwrap() {
            Some((Value::Object(ObjType::Map), id)) => id,
            other => panic!("root[\"store\"] must be an Automerge Map, got {other:?}"),
        };
        let store_keys: BTreeSet<String> = doc.keys(&store).collect();
        let allowed: BTreeSet<String> =
            ["installed", "installing", "seeded"].iter().map(|s| s.to_string()).collect();
        assert!(
            store_keys.is_subset(&allowed),
            "store map holds only projected install-state + the seed flag: {store_keys:?}"
        );
        assert!(store_keys.contains("installed"), "the projected installed-set is present");
        assert!(store_keys.contains("installing"), "the in-flight install progress is present");

        // `installed` / `installing` are Maps; `seeded` is a Bool.
        assert!(matches!(
            doc.get(&store, "installed").unwrap(),
            Some((Value::Object(ObjType::Map), _))
        ));
        assert!(matches!(
            doc.get(&store, "installing").unwrap(),
            Some((Value::Object(ObjType::Map), _))
        ));

        // No user-authored record collection exists anywhere to hang an action
        // off — proving this isn't a git-client-style "saved prefs are actions"
        // and that we did not fabricate a wishlist model.
        for forbidden in [
            "wishlist", "favourites", "favorites", "pinned", "starred", "bookmarks",
            "collections", "lists", "settings", "prefs",
        ] {
            assert!(
                doc.get(&ROOT, forbidden).unwrap().is_none(),
                "document must not carry a `{forbidden}` collection at the root",
            );
            assert!(
                doc.get(&store, forbidden).unwrap().is_none(),
                "store map must not carry a `{forbidden}` collection",
            );
        }
    }

    /// The installed-set is *projected from the live system*: a `flatpak list`
    /// probe reporting a catalog app installed is reconciled into the document by
    /// the real `Widget::sync`, overriding the first-run seed. This is the exact
    /// evidence that install-state is a mirror of the external package manager,
    /// not a user-authored CRDT record — hence not an action.
    #[test]
    fn installed_set_is_projected_from_the_live_system() {
        let mut probe = ProbeResult::default();
        probe.flatpak_available = true;
        // GIMP is a catalog app; the first-run seed lists it "behind" at 2.10.36.
        // The system probe reports it installed at a newer version.
        probe.flatpak_versions = BTreeMap::from([
            ("org.gimp.GIMP".to_string(), "2.10.99".to_string()),
            // An id the curated catalog does not know about: the projection must
            // not invent a record for it (the mirror is bounded by known apps).
            ("com.totally.Unknown".to_string(), "1.0".to_string()),
        ]);

        let mut app = SoftwareStore::new(Some(probe));
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let snap = StoreDoc::read_snapshot(&doc);
        assert_eq!(
            snap.installed.get("org.gimp.GIMP").map(String::as_str),
            Some("2.10.99"),
            "the live probe version was reconciled into the document, overriding the seed",
        );
        assert!(
            !snap.installed.contains_key("com.totally.Unknown"),
            "the projection does not invent a record for an app outside the catalog",
        );
    }

    /// The projected install-state is durable and converges for free through the
    /// toolkit's native CRDT path — save/load lands a peer on the byte-identical
    /// document (same head hashes, not merely the same values). This is precisely
    /// the persistence + convergence an action executor would provide, so wrapping
    /// the projection in an action would add nothing: no action is needed.
    #[test]
    fn projected_state_round_trips_byte_identically() {
        let (_app, mut doc) = synced();
        doc.commit();
        let bytes = doc.save();

        let mut peer = AutoCommit::load(&bytes).unwrap();
        assert_eq!(doc.get_heads(), peer.get_heads(), "byte-identical document on load");

        let here = StoreDoc::read_snapshot(&doc);
        let there = StoreDoc::read_snapshot(&peer);
        assert!(!here.installed.is_empty(), "the seed produced real projected install-state");
        assert_eq!(here.installed, there.installed, "the peer sees the same installed-set");
        assert_eq!(here.installing, there.installing, "the peer sees the same in-flight installs");
    }

    /// The honest steady state: the software store fabricates no action or
    /// resource surface. Exercised on the live widget *after* a real event
    /// (a cursor move through the production `handle_event`), `Widget::resources`
    /// is empty (nothing to publish to Ctrl+K) and no action-host undo boundary is
    /// recorded — both toolkit defaults, because no action is ever dispatched.
    #[test]
    fn exports_no_actions_and_no_resource_tree() {
        let (mut app, mut doc) = synced();

        // Real event path: move the pointer over the surface.
        send(&mut app, &mut doc, &cursor_at(200.0, 200.0));

        assert!(
            Widget::resources(&app, &doc).is_empty(),
            "the store publishes no Ctrl+K resource tree — its only shared state is a projection",
        );
        assert!(
            app.take_committed_action().is_none(),
            "no action is dispatched, so no action-host undo boundary is recorded",
        );
    }
}
