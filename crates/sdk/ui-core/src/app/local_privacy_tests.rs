//! A live local app must not discover ambient workspace capabilities.
//!
//! Socket tripwires prove that construction and local effects perform no
//! service I/O. They deliberately do not emulate a successful remote broker.
#![cfg(unix)]

use super::*;
use crate::widget::TaffyTreeExt;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const EFFECT_TYPE: &str = "test.private-navigation";
const CHILD_ENV: &str = "TILO_LOCAL_APP_PRIVACY_TEST_CHILD";

struct LocalEffectProbe {
    operations: Rc<Cell<usize>>,
    restores: Rc<Cell<usize>>,
    captured: Rc<RefCell<Option<crate::WakeHandle>>>,
}

impl Widget for LocalEffectProbe {
    crate::provides!(
        dyn crate::widget::Continuable,
        dyn crate::resources::PublishesResources
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        *self.captured.borrow_mut() = Some(crate::WakeHandle::new());
        taffy.widget_leaf(taffy::Style::default(), nodes, self)
    }

    fn name(&self) -> &'static str {
        "local-effect-probe"
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[EFFECT_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::host_operation::OperationRequest { kind, type_id, .. }) = op.unscoped()
        else {
            return Ok(None);
        };
        assert!(matches!(
            kind,
            tilo_actions::OperationKind::Effect | tilo_actions::OperationKind::View
        ));
        assert_eq!(type_id, EFFECT_TYPE);
        self.operations.set(self.operations.get().saturating_add(1));
        self.captured.borrow().as_ref().unwrap().wake();
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::resources::PublishesResources for LocalEffectProbe {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new("private-tab", "test.tab", "Private tab")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "navigate",
                        "Navigate",
                        tilo_actions::OperationKind::Effect,
                        EFFECT_TYPE,
                        &(),
                    )
                    .unwrap(),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "inspect",
                        "Inspect private tab",
                        tilo_actions::OperationKind::View,
                        EFFECT_TYPE,
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                ),
        ]
    }
}

impl crate::widget::Continuable for LocalEffectProbe {
    fn continuation_metadata(
        &self,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<crate::collaboration::ContinuationMetadata> {
        None
    }

    fn restore_continuation_metadata(
        &mut self,
        _: &crate::collaboration::ContinuationMetadata,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> bool {
        self.restores.set(self.restores.get().saturating_add(1));
        true
    }
}

fn assert_local_app_behavior() {
    let mut scope = crate::WakeScope::new();
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    scope
        .bind(move |event| {
            wake_tx.send(event).unwrap();
        })
        .unwrap();
    let operations = Rc::new(Cell::new(0));
    let restores = Rc::new(Cell::new(0));
    let captured = Rc::new(RefCell::new(None));
    let root = LocalEffectProbe {
        operations: operations.clone(),
        restores: restores.clone(),
        captured: captured.clone(),
    };
    let mut app = App::from_scoped_parts_local(scope, State::new(), Box::new(root));
    app.init_layout();
    let author = app.collaboration.local_author.0.clone();
    let address = tilo_actions::ActionAddress::new("private-tab", "navigate");
    let result = app
        .invoke_resource_action(ResourceInvocation::new(&address, &author))
        .unwrap();
    assert!(matches!(
        result,
        crate::widget::ResourceOperationStart::Completed(_)
    ));
    assert_eq!(operations.get(), 1, "local Effect must actually execute");
    let wake = tilo_test_env::recv("the local effect's wake", &wake_rx);
    app.needs_sync = false;
    assert!(
        app.accept_wake(&wake),
        "callback capture must retain this live host"
    );
    assert!(app.needs_sync);

    // Accessibility must attribute private View actions to this app's
    // ephemeral participant, never the ambient workspace user.
    app.dispatch_a11y_action(
        crate::accesskit_bridge::resource_action_node_id("private-tab", "inspect"),
        &crate::a11y::Action::Activate,
    );
    assert_eq!(operations.get(), 2, "AX must execute the local View action");
    assert!(app.accept_wake(&wake_rx.try_recv().unwrap()));

    let mut foreign = crate::WakeScope::new();
    let (foreign_tx, foreign_rx) = std::sync::mpsc::channel();
    foreign
        .bind(move |event| {
            foreign_tx.send(event).unwrap();
        })
        .unwrap();
    foreign.handle().wake();
    assert!(!app.accept_wake(&foreign_rx.try_recv().unwrap()));

    assert_eq!(
        restores.get(),
        0,
        "ambient continuation must never reach the private root"
    );
    assert!(app.continuation.pending_restore.is_none());
    assert!(app.continuation.pending_view.is_none());
    assert!(!app.collaboration.manager.is_connected());
    assert!(app.collaboration.manager.primary_route().is_none());
    assert!(app.floating.client.is_none());
    assert!(app.presence.broadcast_scope.is_none());
    assert_ne!(author, "ambient-workspace-user");
    assert!(!app.presence_ctx.is_own_identity("ambient-workspace-user"));
    assert_eq!(
        app.presence_ctx
            .pointer_owner_for(&PresenceKey::surface_pointer("test.ambient-browser")),
        crate::presence::PointerOwner::OtherSurface,
        "private app must not claim its ambient package's shared surface"
    );
    assert!(
        app.accessibility.a11y.high_contrast,
        "ordinary local accessibility settings remain enabled"
    );
    assert_eq!(app.text_scale(), 1.25);

    let second = App::from_scoped_parts_local(
        crate::WakeScope::new(),
        State::new(),
        Box::new(crate::widget::Column::new()),
    );
    assert_ne!(
        author, second.collaboration.local_author.0,
        "each local app needs an ephemeral identity"
    );
    let callback_wake = captured.borrow().as_ref().unwrap().clone();
    drop(app);
    assert!(
        !callback_wake.is_active(),
        "closing the host must revoke its callbacks"
    );
    callback_wake.wake();
    assert!(wake_rx.try_recv().is_err());
}

#[test]
#[cfg(unix)]
fn local_app_ignores_ambient_services_and_runs_effects() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    if std::env::var_os(CHILD_ENV).is_some() {
        assert_local_app_behavior();
        return;
    }

    let scratch = tempfile::tempdir().unwrap();
    let services = [
        "TILO_SVC_IDENTITY_FD",
        "TILO_SVC_COLLAB_FD",
        "TILO_SVC_SHELL-UI_FD",
    ];
    let pairs = services.map(|_| UnixStream::pair().unwrap());
    // A regression must fail promptly, without touching a real identity store
    // or waiting for a real broker. A discovery request receives this refusal.
    runtime_identity::wire::FRAMING
        .write_value(
            &mut &pairs[0].1,
            &runtime_identity::wire::Response::Error("private-app service tripwire".into()),
        )
        .unwrap();
    pairs[0].1.shutdown(std::net::Shutdown::Write).unwrap();
    let inherited = pairs.each_ref().map(|(client, _)| client.as_raw_fd());
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "app::local_privacy_tests::local_app_ignores_ambient_services_and_runs_effects",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD_ENV, "1")
        .env("XDG_CONFIG_HOME", scratch.path().join("config"))
        .env("XDG_DATA_HOME", scratch.path().join("data"))
        .env("XDG_CACHE_HOME", scratch.path().join("cache"))
        .env("XDG_STATE_HOME", scratch.path().join("state"))
        .env("TILO_IDENTITY_ROOT", scratch.path().join("identity"))
        .env("TILO_PACKAGE_ID", "test.ambient-browser")
        .env("TILO_WORKSPACE_ID", "1c9f7a99-40cb-4a63-a562-3958d1810e65")
        .env("TILO_USER_ID", "ambient-workspace-user")
        .env("TILO_CONTINUITY_RESUME_HEX", "010203")
        .env("TILO_CONTINUITY_RESUME_SCHEMA", "42")
        .env("TILO_A11Y_HIGH_CONTRAST", "1")
        .env("TILO_A11Y_TEXT_SCALE", "1.25");
    for (service, fd) in services.into_iter().zip(inherited) {
        command.env(service, fd.to_string());
    }
    // SAFETY: only async-signal-safe fcntl calls run between fork and exec.
    // Descriptor inheritance changes only in the child, never in other tests.
    unsafe {
        command.pre_exec(move || {
            for fd in inherited {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    let mut contacted = Vec::new();
    for (service, (client, mut server)) in services.into_iter().zip(pairs) {
        drop(client);
        let mut bytes = Vec::new();
        if let Err(error) = server.read_to_end(&mut bytes) {
            // Linux may reset a socket closed with our refusal still unread.
            // Captured request bytes remain evidence even after that reset.
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
        if !bytes.is_empty() {
            contacted.push((service, bytes.len()));
        }
    }
    assert!(
        contacted.is_empty(),
        "private app contacted ambient services: {contacted:?}; child stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "isolated local app test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest exits 0 when `--exact` selects nothing; require the child ran.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
        "the child ran no test: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
