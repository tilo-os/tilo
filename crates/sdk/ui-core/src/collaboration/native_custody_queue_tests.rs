// Live SDK queue/Capnp correlation proof; native owner policy is qualified separately.
const NATIVE_CUSTODY_REFERENCE: [u8; 32] = [23; 32];

struct NativeCustodyProbe {
    source: crate::invocation_scope::NativeInvocationSource,
    envelope: Vec<u8>,
    changes: Vec<[u8; 32]>,
    mutation: DocumentMutation,
    total: usize,
    body: Vec<u8>,
    opens: usize,
    native_sends: usize,
    ordinary_sends: usize,
}

impl NativeCustodyProbe {
    fn accept(
        &mut self,
        bytes: &[u8],
        references: Result<capnp::data_list::Reader<'_>, capnp::Error>,
    ) -> Result<(), capnp::Error> {
        let refs = references?;
        if refs.is_empty() {
            self.ordinary_sends += 1;
            return Err(capnp::Error::failed(
                "native custody cannot use ordinary send".into(),
            ));
        }
        self.source.check().map_err(capnp::Error::failed)?;
        if refs.len() != 1
            || refs.get(0)? != NATIVE_CUSTODY_REFERENCE.as_slice()
            || self.total == 0
            || self.body.len() != self.total
        {
            return Err(capnp::Error::failed(
                "native custody reference is incomplete or changed".into(),
            ));
        }
        let body: runtime_collab::document_contribution::DocumentContributionBody =
            postcard::from_bytes(&self.body).map_err(|e| capnp::Error::failed(e.to_string()))?;
        let op: SignedOp =
            postcard::from_bytes(bytes).map_err(|e| capnp::Error::failed(e.to_string()))?;
        collab_types::verify_op(&op).map_err(|e| capnp::Error::failed(e.to_string()))?;
        let mutation = DocumentMutation::decode(&op.payload)
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        let envelope: tilo_actions::ActionEnvelope = serde_json::from_slice(&body.envelope)
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        if body.signed_op != bytes
            || body.envelope != self.envelope
            || body.changes != self.changes
            || mutation != self.mutation
            || envelope
                .context
                .as_ref()
                .and_then(|context| context.source_invocation)
                != Some(self.source.request().invocation)
            || op.artifact
                != collab_types::document_artifact_id(
                    &self.source.route().app_id,
                    &self.source.route().cluster_id.to_string(),
                    self.source.route().record.as_deref().unwrap_or(""),
                )
        {
            return Err(capnp::Error::failed(
                "native custody changed original UUID, range or operation".into(),
            ));
        }
        self.native_sends += 1;
        Ok(())
    }
}

struct NativeCustodyWriter(SharedCaptures);

impl runtime_collab::collab_capnp::document_contribution_writer::Server for NativeCustodyWriter {
    fn write(
        self: Rc<Self>,
        params: runtime_collab::collab_capnp::document_contribution_writer::WriteParams,
        _: runtime_collab::collab_capnp::document_contribution_writer::WriteResults,
    ) -> Promise<(), capnp::Error> {
        let result = (|| {
            let request = params.get()?;
            let offset = usize::try_from(request.get_offset())
                .map_err(|_| capnp::Error::failed("fixture offset overflow".into()))?;
            let bytes = request.get_chunk()?;
            let mut captures = self
                .0
                .lock()
                .map_err(|_| capnp::Error::failed("fixture upload lock poisoned".into()))?;
            let probe = captures
                .native_custody
                .as_mut()
                .ok_or_else(|| capnp::Error::failed("fixture source disappeared".into()))?;
            probe.source.check().map_err(capnp::Error::failed)?;
            if offset != probe.body.len()
                || bytes.is_empty()
                || bytes.len() > runtime_collab::document_contribution::MAX_CHUNK_BYTES
                || offset
                    .checked_add(bytes.len())
                    .is_none_or(|end| end > probe.total)
            {
                return Err(capnp::Error::failed("fixture upload range changed".into()));
            }
            probe.body.extend_from_slice(bytes);
            Ok(())
        })();
        Promise::from(result)
    }
}

struct NativeCustodyIdentity(ed25519_dalek::SigningKey);

impl SdkCertifiedIdentity for NativeCustodyIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        Err(std::io::Error::other("committed native fixture must not ask for MLS keys").into())
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([0; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        unsigned: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        use ed25519_dalek::Signer as _;
        let op: SignedOp = postcard::from_bytes(unsigned)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let bytes = op.signing_bytes().map_err(std::io::Error::other)?;
        Ok(self.0.sign(&bytes).to_bytes())
    }
}

#[test]
fn native_custody_queue_preserves_original_through_owner_await_and_terminal_barrier() {
    use crate::invocation_scope::{
        NativeMessageStreamLifetime,
        native_message::tests::{RecordFixture, delivered_source},
    };
    // 0: saved, 1: original stream retired before publication, 2: uncertain owner send.
    for mode in 0..3 {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = TokioLocalSet::new();
        let mut lifetime = Some(NativeMessageStreamLifetime::new());
        let fixture = local.block_on(&rt, async {
            RecordFixture::new(delivered_source(lifetime.as_ref().unwrap()).await)
        });
        let committed_history = fixture
            .store
            .with_checked(&fixture.checkout.id, |doc| doc.save())
            .unwrap();
        let executed = fixture.apply(false).unwrap();
        let contribution = fixture.capture.clone().finish().unwrap();
        let mutation = fixture
            .store
            .with_checked(&fixture.checkout.id, |doc| {
                DocumentMutation::Incremental(doc.save_after(&executed.base_change_hashes()))
            })
            .unwrap();
        let route = contribution.source().route().clone();
        let wire_id = contribution.source().request().wire_id;
        let invocation = contribution.source().request().invocation;
        let weak = Arc::downgrade(&contribution);
        let captures = Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(committed_history),
            invocation_fixture_enabled: true,
            native_custody: Some(NativeCustodyProbe {
                source: contribution.source().clone(),
                envelope: contribution.envelope().to_vec(),
                changes: contribution.changes().to_vec(),
                mutation: mutation.clone(),
                total: 0,
                body: vec![],
                opens: 0,
                native_sends: 0,
                ordinary_sends: 0,
            }),
            ..Captures::default()
        }));
        let (entered, observed) = mpsc::channel();
        let (release, waiting) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: waiting,
            });
        let (fd, captures, thread) = stand_up_fake_broker(FakeBroker {
            captures,
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        });
        let mut manager = CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            route.app_id.clone(),
            route.cluster_id,
            route.record.clone(),
            None,
            Some(SdkJoinIdentity {
                presentation: vec![],
                participant_kind: "person",
                signer: SdkDocumentSigner::Certified(Arc::new(NativeCustodyIdentity(
                    ed25519_dalek::SigningKey::from_bytes(&[42; 32]),
                ))),
            }),
            "person",
        )
        .unwrap();
        // Package-primary source custody names the original app route, not a
        // secondary record route. Apply its actual owner-delivered capture.
        let mut initial = None;
        tilo_test_env::wait_until("the exact native primary history capture", || {
            for event in manager.poll() {
                if let CollabEvent::CommittedDocument {
                    route: observed,
                    read,
                } = event
                {
                    assert_eq!(observed, route);
                    assert!(initial.replace(read).is_none());
                }
            }
            initial.is_some()
        });
        let read = initial.unwrap();
        read.with_mutation(|mutation| {
            fixture
                .store
                .merge_remote_document(&fixture.checkout, mutation.clone())
        })
        .unwrap()
        .unwrap();
        read.applied();
        tilo_test_env::wait_until("the applied exact primary history to become ready", || {
            broker_state(&manager)
                .document_reads
                .publishable(&route.channel_id())
        });
        drop(fixture); // No retained fixture Arc may make custody-release assertions tautological.
        assert!(manager.send(CollabRequest::NativeExecutedContribution { contribution }));
        // This marker establishes that source staging actually ran, without publishing a document.
        assert_eq!(
            manager
                .begin_document_publication_barrier()
                .unwrap()
                .wait(Some(tilo_test_env::hang_deadline())),
            Ok(())
        );
        assert!(
            weak.strong_count() > 0,
            "actual staged source is retained before its document"
        );
        assert_eq!(captures.lock().unwrap().terminal_updates, 0);
        assert_eq!(
            captures
                .lock()
                .unwrap()
                .native_custody
                .as_ref()
                .unwrap()
                .opens,
            0
        );
        if mode == 1 {
            drop(lifetime.take());
        }
        assert!(manager.send(CollabRequest::Document {
            mutation,
            authorization: None
        }));
        let terminal_submitted = manager.send(CollabRequest::UpdateInvocation {
            id: wire_id,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: Default::default(),
            },
        });
        let barrier = manager.begin_document_publication_barrier();
        if mode == 1 {
            // The staged source and Document submission were observed above.
            // Retirement may close the worker before it accepts the following
            // terminal/barrier; this case claims refusal, not an unseen await.
            assert!(
                barrier
                    .and_then(|receipt| receipt.wait(Some(tilo_test_env::hang_deadline())))
                    .is_err()
            );
            let capture = captures.lock().unwrap();
            let probe = capture.native_custody.as_ref().unwrap();
            assert_eq!(probe.opens, 0);
            assert_eq!(probe.native_sends, 0);
            assert_eq!(probe.ordinary_sends, 0);
            assert!(capture.signed_ops.is_empty());
            assert_eq!(capture.terminal_updates, 0);
            drop(capture);
            drop(release);
        } else {
            assert!(terminal_submitted);
            let barrier = barrier.unwrap();
            tilo_test_env::wait_until("the native owner send RPC", || {
                broker_state(&manager).publication_result().unwrap();
                match observed.try_recv() {
                    Ok(()) => true,
                    Err(mpsc::TryRecvError::Empty) => false,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("native owner send closed before its await")
                    }
                }
            });
            assert_eq!(barrier.try_complete(), Ok(None));
            assert_eq!(
                captures.lock().unwrap().terminal_updates,
                0,
                "terminal cannot overtake the actual owner await"
            );
            assert!(
                weak.strong_count() > 0,
                "actual SDK queue retains original source through await"
            );
            release
                .send(if mode == 0 {
                    Ok(())
                } else {
                    Err("uncertain fixture owner save".into())
                })
                .unwrap();
            let result = barrier.wait(Some(tilo_test_env::hang_deadline()));
            assert_eq!(result.is_ok(), mode == 0);
            let capture = captures.lock().unwrap();
            let probe = capture.native_custody.as_ref().unwrap();
            assert_eq!(probe.opens, 1, "one original writer; no replacement upload");
            assert_eq!(
                probe.native_sends, 1,
                "one exact signed operation; no replay"
            );
            assert_eq!(
                probe.ordinary_sends, 0,
                "native custody never falls back to ordinary send"
            );
            assert_eq!(probe.body.len(), probe.total);
            assert!(
                !probe.body.is_empty(),
                "owner retains the exact uploaded original after uncertain reply"
            );
            assert_eq!(capture.signed_ops.len(), 1);
            assert_eq!(capture.terminal_updates, usize::from(mode == 0));
            if mode == 0 {
                assert_eq!(capture.invocation_updates.len(), 1);
                assert_eq!(capture.invocation_updates[0].0, wire_id);
                assert!(matches!(&capture.invocation_updates[0].1,
                    tilo_actions::InvocationUpdate::Succeeded { invocation: actual, .. }
                    if *actual == invocation));
            } else {
                assert!(capture.invocation_updates.is_empty());
            }
            drop(capture);
            if mode == 0 {
                tilo_test_env::wait_until("saved source released by its exact terminal", || {
                    weak.strong_count() == 0
                });
            }
        }
        if mode != 0 {
            assert!(!manager.is_connected());
            assert!(
                !manager.send(CollabRequest::document_incremental(vec![1])),
                "unresolved worker cannot publish a replacement or bypass source refusal"
            );
            assert_eq!(captures.lock().unwrap().terminal_updates, 0);
        }
        drop(manager);
        thread.join().unwrap();
    }
}
