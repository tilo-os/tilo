//! The collaboration keystone: two peers co-edit one shared CRDT
//! document, each renders the shared state **locally** (its own render
//! of its own converged Automerge doc — never a streamed frame), and
//! each sees the other's caret/presence.
//!
//! Runs against the production typed protocol: two real
//! `CollaborationManager`s over Cap'n Proto Unix sockets, real `automerge` documents, and the real
//! `tilo_ui::widget::TextInput` render pass — no mocks of the thing under test.
//!
//! Three proofs: converged co-edit renders identically on both peers
//! (`peer_edit_converges_...`), a caret round-trips through the real
//! presence path and paints into the local render
//! (`remote_caret_presence_...`), and a signed op validates/rejects
//! through the real `collab_types::validate_inbound_op` pipeline by role
//! (`signed_edit_is_permission_gated`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use collab_types::CommitNamed as _;

use tilo_ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::permissions::{PermissionContext, UserId};
use tilo_ui::presence::{PathSegment, PresenceCtx, PresenceKey};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{TextInput, Widget};

use crate::support::typed_collab::TypedBroker;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use std::collections::HashMap;
use taffy::prelude::*;

const W: u32 = 480;
const H: u32 = 120;

fn text_obj(doc: &AutoCommit) -> Option<ObjId> {
    match doc.get(&ROOT, "text") {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => None,
    }
}

fn text_of(doc: &AutoCommit) -> String {
    text_obj(doc)
        .and_then(|id| doc.text(&id).ok())
        .unwrap_or_default()
}

/// `drawn` is the exact draw-pass output, compared with `==`. `pixels`
/// proves the frame really rasterises, but two rasterisations of one
/// scene aren't guaranteed bit-identical — vello's tile `atomicAdd`
/// coverage accumulation can land an antialiased edge pixel one 8-bit
/// step either side — so pixels are compared with
/// `OffscreenRenderer::pixels_differing_beyond_aa` and by ink count.
struct LocalRender {
    drawn: Drawn,
    pixels: Vec<u8>,
}

/// The vello `Scene` a draw pass emitted, flattened to bytes so two frames
/// can be compared exactly — more specific than comparing rasterised images.
#[derive(PartialEq, Eq)]
struct Drawn(Vec<u8>);

impl std::fmt::Debug for Drawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.0.hash(&mut h);
        write!(
            f,
            "Drawn({} bytes, digest {:016x})",
            self.0.len(),
            h.finish()
        )
    }
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    push_u32(out, v.to_bits());
}

fn drawn_of(scene: &Scene) -> Drawn {
    let e = scene.encoding();
    let mut b = Vec::new();
    for t in &e.path_tags {
        b.push(t.0);
    }
    for v in &e.path_data {
        push_u32(&mut b, *v);
    }
    for t in &e.draw_tags {
        push_u32(&mut b, t.0);
    }
    for v in &e.draw_data {
        push_u32(&mut b, *v);
    }
    for t in &e.transforms {
        for v in t.matrix {
            push_f32(&mut b, v);
        }
        for v in t.translation {
            push_f32(&mut b, v);
        }
    }
    for s in &e.styles {
        push_u32(&mut b, s.flags_and_miter_limit);
        push_f32(&mut b, s.line_width);
    }
    for g in &e.resources.glyphs {
        push_u32(&mut b, g.id);
        push_f32(&mut b, g.x);
        push_f32(&mut b, g.y);
    }
    for run in &e.resources.glyph_runs {
        push_f32(&mut b, run.font_size);
        push_u32(&mut b, run.font.index);
        push_u32(&mut b, u32::from(run.hint));
        push_u32(&mut b, u32::try_from(run.glyphs.start).unwrap());
        push_u32(&mut b, u32::try_from(run.glyphs.end).unwrap());
        for v in run.transform.matrix {
            push_f32(&mut b, v);
        }
        for v in run.transform.translation {
            push_f32(&mut b, v);
        }
    }
    for v in [
        e.n_paths,
        e.n_path_segments,
        e.n_clips,
        e.n_open_clips,
        e.flags,
    ] {
        push_u32(&mut b, v);
    }
    Drawn(b)
}

/// Render `input` against `doc` **locally**: real `sync` (pulls converged
/// text out of the Automerge doc), layout, and rasterise — a pure
/// function of local state, never a frame consumed from a peer.
#[allow(
    clippy::cast_precision_loss,
    reason = "W and H are the fixed small test-canvas dimensions (480x120), exactly representable in f32"
)]
fn render_widget(mut input: TextInput, doc: &AutoCommit, presence: &PresenceCtx) -> LocalRender {
    let theme = Theme::dark();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();

    // The real sync pass: widget visual state is pulled from the CRDT.
    (&mut input as &mut dyn Widget).sync(doc, &ROOT);

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = (&mut input as &mut dyn Widget).build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W as f32),
                    height: Dimension::length(H as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    {
        let measure_map = node_map.clone();
        let tr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(W as f32),
                    height: AvailableSpace::Definite(H as f32),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    (&mut input as &mut dyn Widget).apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    (&mut input as &mut dyn Widget).draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        presence,
        &permission,
    );
    let mut renderer = OffscreenRenderer::new(W, H);
    LocalRender {
        drawn: drawn_of(&scene),
        pixels: renderer.render(&scene, Color::BLACK),
    }
}

fn render_bound(doc: &AutoCommit, bound: &ObjId, presence: &PresenceCtx) -> LocalRender {
    let mut input = TextInput::new(Vec::new())
        .with_text_role(tilo_ui::TextRole::LargeTitle)
        .with_plain_chrome();
    input.bind_text(bound.clone());
    render_widget(input, doc, presence)
}

/// Presence key is `Widget { path }`, so a remote caret published under
/// the same key paints into this widget's local render.
fn render_presence_probe(path: Vec<PathSegment>, presence: &PresenceCtx) -> LocalRender {
    let input = TextInput::new(path)
        .with_text_role(tilo_ui::TextRole::LargeTitle)
        .with_plain_chrome();
    let throwaway = AutoCommit::new();
    render_widget(input, &throwaway, presence)
}

#[track_caller]
fn assert_same_frame(a: &LocalRender, b: &LocalRender, what: &str) {
    assert_eq!(a.drawn, b.drawn, "{what}: the drawn scene must match");
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&a.pixels, &b.pixels),
        0,
        "{what}: the rasterised frames must match"
    );
    assert_eq!(
        non_background_pixels(&a.pixels),
        non_background_pixels(&b.pixels),
        "{what}: the same drawing must put ink on the same number of pixels"
    );
}

fn non_background_pixels(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4)
        .filter(|px| {
            u32::from(px[0])
                .saturating_add(u32::from(px[1]))
                .saturating_add(u32::from(px[2]))
                > 16
                && px[3] > 16
        })
        .count()
}

fn two_peers() -> (CollaborationManager, CollaborationManager) {
    let broker = TypedBroker::new();
    // Each returns subscribed, so fan-out traffic reaches both from here on.
    let a = broker.manager("alice");
    let b = broker.manager("bob");
    (a, b)
}

fn poll_until<T>(
    what: &str,
    mgr: &mut CollaborationManager,
    mut f: impl FnMut(&CollabEvent) -> Option<T>,
) -> T {
    tilo_test_env::wait_for(what, || mgr.poll().iter().find_map(&mut f))
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn peers_co_edit_converge_share_carets_and_gate_signed_edits() {
    // Case `peer_edit_converges_and_each_renders_the_shared_state_locally`.
    {
        let (mut a, mut b) = two_peers();

        let mut a_doc = AutoCommit::new();
        a_doc.set_actor(automerge::ActorId::from(b"alice".as_slice()));
        let a_text = a_doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
        a_doc.splice_text(&a_text, 0, 0, "SEED").unwrap();

        // Late-join handshake: A ships a full snapshot, B adopts it. This is
        // exactly the document snapshot path the App drives on join.
        a.send(CollabRequest::document_snapshot(a_doc.save()));
        a_doc.commit_edit("fixture", "text");
        let _ = a_doc.save_incremental(); // re-baseline incremental after full save

        let mut b_doc = AutoCommit::new();
        b_doc.set_actor(automerge::ActorId::from(b"bob".as_slice()));
        let adopted = poll_until("B must receive A's snapshot", &mut b, |ev| {
            match ev.document_mutation()? {
                Ok(collab_types::DocumentMutation::Snapshot(data)) => Some(data),
                _ => None,
            }
        });
        let mut incoming = AutoCommit::load(&adopted).unwrap();
        b_doc.merge(&mut incoming).unwrap();
        assert_eq!(text_of(&b_doc), "SEED", "B adopts A's seed text");

        let b_text = text_obj(&b_doc).expect("B's doc has the shared Text object");
        // The object id is global in Automerge, so B binds to the SAME object
        // A created — this is genuinely one shared CRDT object.
        assert_eq!(b_text, a_text, "both peers share one CRDT Text object id");

        let empty_presence = PresenceCtx::empty();
        let b_before = render_bound(&b_doc, &b_text, &empty_presence);
        assert!(
            non_background_pixels(&b_before.pixels) > 0,
            "B's pre-edit render must show the seeded text"
        );

        a_doc.splice_text(&a_text, 4, 0, "-EDIT").unwrap();
        a_doc.commit_edit("fixture", "text");
        let delta = a_doc.save_incremental();
        assert!(!delta.is_empty(), "the edit must produce a delta");
        a.send(CollabRequest::document_incremental(delta));

        let got = poll_until("B must receive A's incremental edit", &mut b, |ev| match ev
            .document_mutation()?
        {
            Ok(collab_types::DocumentMutation::Incremental(data)) => Some(data),
            _ => None,
        });
        b_doc.load_incremental(&got).unwrap();

        assert_eq!(
            text_of(&b_doc),
            "SEED-EDIT",
            "B's doc converges with A's edit over the real broker"
        );
        assert_eq!(text_of(&a_doc), text_of(&b_doc), "the two peers converge");

        let b_after = render_bound(&b_doc, &b_text, &empty_presence);
        let a_after = render_bound(&a_doc, &a_text, &empty_presence);

        // B re-rendered locally: its post-edit frame differs from its
        // pre-edit frame (the change is visible in B's OWN render).
        assert_ne!(
            b_after.drawn, b_before.drawn,
            "B must re-render locally to reflect the converged edit"
        );
        assert!(
            non_background_pixels(&b_after.pixels) > non_background_pixels(&b_before.pixels),
            "the added text must add drawn pixels in B's local render"
        );

        // The keystone assertion.
        assert_same_frame(
            &a_after,
            &b_after,
            "each peer's local render of the shared CRDT state; \
         co-edit is local-render convergence, not frame streaming",
        );

        drop(a);
        drop(b);
    }

    // Case `remote_caret_presence_round_trips_both_ways`.
    {
        let (mut a, mut b) = two_peers();

        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("text".into())],
        };

        a.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 7 },
        });
        let a_caret_on_b = poll_until(
            "B must receive A's caret presence attributed to alice",
            &mut b,
            |ev| match ev {
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    payload: PresencePayload::Caret { pos },
                    ..
                } if user_id == "alice" => Some(*pos),
                _ => None,
            },
        );
        assert_eq!(a_caret_on_b, 7);

        // B's caret moves -> A sees it, attributed to bob. Both directions
        // travel the same real presence fan-out.
        b.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 3 },
        });
        let b_caret_on_a = poll_until(
            "A must receive B's caret presence attributed to bob",
            &mut a,
            |ev| match ev {
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    payload: PresencePayload::Caret { pos },
                    ..
                } if user_id == "bob" => Some(*pos),
                _ => None,
            },
        );
        assert_eq!(b_caret_on_a, 3);

        // The remote caret paints into the local render: place alice's caret
        // into a presence context keyed to the same widget and prove the
        // peer-caret chrome draws ink that a no-presence render does not.
        let path = vec![PathSegment::Field("text".into())];
        let no_presence = PresenceCtx::empty();
        let baseline = render_presence_probe(path.clone(), &no_presence);

        let mut with_peer = PresenceCtx::empty();
        with_peer.set_caret(&key, UserId("alice".into()), 4);
        let with_caret = render_presence_probe(path, &with_peer);

        assert_ne!(
            baseline.drawn, with_caret.drawn,
            "a remote peer's caret must be drawn into the local render"
        );
        assert!(
            non_background_pixels(&with_caret.pixels) > non_background_pixels(&baseline.pixels),
            "the peer caret must add drawn pixels to the local render"
        );

        drop(a);
        drop(b);
    }

    // Case `signed_edit_is_permission_gated`.
    {
        // One channel owner underwrites two members: a writer (Editor) and a
        // viewer (Viewer, no write capability). Both target the same Note
        // artifact.
        let (owner_sk, owner_pub) = generate_keypair();
        let (writer_sk, writer_pub) = generate_keypair();
        let (viewer_sk, viewer_pub) = generate_keypair();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();

        let writer_att = make_attestation(&owner_sk, owner_pub, writer_pub, channel, Role::Editor);
        let viewer_att = make_attestation(&owner_sk, owner_pub, viewer_pub, channel, Role::Viewer);
        let writer_att_id = writer_att.attestation.id;
        let viewer_att_id = viewer_att.attestation.id;

        let mut state = ChannelState::default();
        state.authorised_signers.push(owner_pub);
        state.artifacts.insert(artifact, ArtifactKind::Note);
        state.register_attestation(writer_att);
        state.register_attestation(viewer_att);

        let mut peer_doc = AutoCommit::new();
        let t = peer_doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
        peer_doc.splice_text(&t, 0, 0, "base").unwrap();
        let bound = text_obj(&peer_doc).unwrap();
        let baseline_render = render_bound(&peer_doc, &bound, &PresenceCtx::empty());

        let mut writer_doc = AutoCommit::load(&peer_doc.save()).unwrap();
        writer_doc.set_actor(automerge::ActorId::from(b"writer".as_slice()));
        let wt = text_obj(&writer_doc).unwrap();
        writer_doc.splice_text(&wt, 4, 0, "-WROTE").unwrap();
        writer_doc.commit_edit("fixture", "text");
        let writer_delta = writer_doc.save_incremental();
        let writer_op = sign_delta(
            &writer_sk,
            writer_pub,
            writer_att_id,
            artifact,
            writer_delta,
            1,
        );

        // The viewer forges the identical intent but has no write capability.
        let mut viewer_doc = AutoCommit::load(&peer_doc.save()).unwrap();
        viewer_doc.set_actor(automerge::ActorId::from(b"viewer".as_slice()));
        let vt = text_obj(&viewer_doc).unwrap();
        viewer_doc.splice_text(&vt, 4, 0, "-HACKED").unwrap();
        viewer_doc.commit_edit("fixture", "text");
        let viewer_delta = viewer_doc.save_incremental();
        let viewer_op = sign_delta(
            &viewer_sk,
            viewer_pub,
            viewer_att_id,
            artifact,
            viewer_delta,
            1,
        );

        match validate_inbound_op(&viewer_op, &mut state) {
            Err(ValidationError::NotGranted(_)) => {}
            other => panic!("viewer without write cap must be rejected, got {other:?}"),
        }
        // ...so the peer doc and its render are untouched.
        assert_eq!(
            text_of(&peer_doc),
            "base",
            "a rejected op must not mutate the shared doc"
        );
        let again = render_bound(&peer_doc, &bound, &PresenceCtx::empty());
        assert_same_frame(&again, &baseline_render, "a rejected op's local render");

        // The writer's op validates and is applied via the same
        // `load_incremental` path the App uses for incremental document events.
        validate_inbound_op(&writer_op, &mut state).expect("writer op must validate");
        peer_doc.load_incremental(&writer_op.payload).unwrap();

        assert_eq!(
            text_of(&peer_doc),
            "base-WROTE",
            "the writer's validated edit converges into the peer doc"
        );
        let after_render = render_bound(&peer_doc, &bound, &PresenceCtx::empty());
        assert_ne!(
            after_render.drawn, baseline_render.drawn,
            "the writer's validated edit re-renders locally"
        );
        assert!(
            non_background_pixels(&after_render.pixels)
                > non_background_pixels(&baseline_render.pixels),
            "the writer's added text must put ink on more pixels"
        );
    }
}

// ---------------------------------------------------------------------
// Permission-gated signed co-edit: drives the real
// `collab_types::validate_inbound_op` pipeline, not a re-implementation.
// ---------------------------------------------------------------------

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Ed25519PublicKey, OpKind, Role, RoleAttestation, Signature, SignedOp, ValidationError,
    generate_keypair, sign_event, sign_op, validate_inbound_op,
};
use ed25519_dalek::SigningKey;
use std::time::{Duration as StdDuration, SystemTime};

fn make_attestation(
    issuer_sk: &SigningKey,
    issuer_pub: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pub,
        subject,
        claims: Vec::<Claim>::new(),
        valid_from: SystemTime::UNIX_EPOCH,
        valid_until: SystemTime::UNIX_EPOCH
            .checked_add(StdDuration::from_secs(1_000_000))
            .unwrap(),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = att.signing_bytes().unwrap();
    att.signature = sign_event(&sb, issuer_sk).unwrap();
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

/// Sign a Splice op whose payload is a real Automerge incremental delta
/// (the same shape the App carries in `SignedOp::payload`).
fn sign_delta(
    author_sk: &SigningKey,
    author_pub: Ed25519PublicKey,
    attestation_id: AttestationId,
    artifact: ArtifactId,
    delta: Vec<u8>,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        delta,
        artifact,
        author_pub,
        attestation_id,
        OpKind::Change,
        SystemTime::UNIX_EPOCH
            .checked_add(StdDuration::from_secs(10))
            .unwrap(),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}
