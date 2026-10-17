//! The identity service feeds the frontend's shared avatar painter.
use std::sync::Arc;
use tilo_identity_host::provisioning::IdentityStorage;

struct NoSigningWorkspace;
impl tilo_identity_host::broker::WorkspaceMembership for NoSigningWorkspace {
    fn with_workspace(
        &self,
        _room: &tilo_desktop_model::RoomId,
        _read: &mut dyn FnMut(&tilo_desktop_model::Workspace),
    ) {
    }
}

fn paint_default_avatar(key: &str, theme: &ui::Theme, text: &mut ui::TextRenderer) -> vello::Scene {
    use vello::kurbo::{Affine, Rect};
    let mut scene = vello::Scene::new();
    ui::widget::AvatarFace::new("Ada", key).paint(
        &mut ui::paint::PaintCtx::new(&mut scene, Affine::IDENTITY, text, theme),
        Rect::new(16.0, 16.0, 64.0, 64.0),
    );
    scene
}

#[test]
fn negotiated_real_broker_photo_reaches_default_shared_avatar_paint() {
    use std::os::fd::AsRawFd as _;
    let mut env = tilo_test_env::ScopedEnv::new();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let system = temp.path().join("system");
    env.set("XDG_CONFIG_HOME", &config)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(runtime_identity::FILE_SECRETS_ENV, "1");
    let identity = config.join("tilo/identity");
    let paths = tilo_user_identity::IdentityPaths::under(system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identity,
        &paths,
        IdentityStorage::Files,
    )
    .unwrap();
    let photo = runtime_identity::ProfilePhoto::new(1, 1, vec![25, 90, 210, 255]).unwrap();
    let metadata = runtime_identity::update_identity_presentation(
        &identity,
        "primary",
        "Ada",
        [1, 2, 3],
        Some(photo.clone()),
    )
    .unwrap();
    let room = tilo_desktop_model::RoomId::new();
    let socket = temp.path().join("identity.sock");
    let _broker = tilo_identity_host::broker::spawn(
        socket.clone(),
        Arc::new(NoSigningWorkspace),
        Arc::new(()),
        Arc::new(()),
    )
    .unwrap();
    let mut client = std::os::unix::net::UnixStream::connect(socket).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    client
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let handshake = runtime_identity::wire::PresentationHandshake {
        handshake: runtime_identity::wire::Handshake {
            workspace_id: room.to_string(),
            app_id: "so.tilo.imager".into(),
            surface: None,
        },
        presentation_version: runtime_identity::wire::PRESENTATION_VERSION,
    };
    runtime_identity::wire::HANDSHAKE_FRAMING
        .write(&mut client, &serde_json::to_vec(&handshake).unwrap())
        .unwrap();
    env.set("TILO_SVC_IDENTITY_FD", client.as_raw_fd().to_string());
    // No injected SDK identity source or explicit image: the production
    // painter obtains presentation from the actual inherited service.
    let delivered = runtime_identity::load_bound_metadata().unwrap();
    assert_eq!(delivered.group_public_key, metadata.group_public_key);
    assert_eq!(delivered.profile_photo, Some(photo));
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::new();
    let scene = paint_default_avatar(&hex::encode(metadata.group_public_key), &theme, &mut text);
    assert_eq!(
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| **tag == vello_encoding::DrawTag::IMAGE)
            .count(),
        1
    );
    let mut renderer = ui::offscreen::OffscreenRenderer::new(80, 80);
    let pixels = renderer.render(&scene, theme.tokens.colors.surfaces.content);
    let center = (40 * 80 + 40) * 4;
    for (actual, expected) in pixels[center..center + 4].iter().zip([25_u8, 90, 210, 255]) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "the actual broker photo must be painted"
        );
    }
    for wrong_key in [
        hex::encode([7_u8; 32]),
        "Ada".into(),
        "ada@example.org".into(),
    ] {
        let scene = paint_default_avatar(&wrong_key, &theme, &mut text);
        assert!(
            !scene
                .encoding()
                .draw_tags
                .contains(&vello_encoding::DrawTag::IMAGE)
        );
    }
}
