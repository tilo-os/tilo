//! The person's lockdown policy while the session runs: flipping a switch stops what is capturing
//! and silences what is playing, without asking any app to restart.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use media::audio_output::{Factory, Observation, Output};
use tilo_portal::capture_broker::{
    CaptureBrokerError, CaptureKind, CaptureSessionBinding, LocalCaptureHandle,
};
use tilo_portal::lockdown::{LockdownBackend, LockdownPolicy};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_test_env::PrivateBus;

/// An output route that needs no device: what a player would open.
struct Silent(Arc<AtomicUsize>);

impl Output for Silent {
    fn write(&mut self, _samples: &[i16]) -> Result<Observation, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Observation::default())
    }
    fn observation(&mut self) -> Result<Observation, String> {
        Ok(Observation::default())
    }
    fn cached_observation(&self) -> Observation {
        Observation::default()
    }
    fn cached_error(&self) -> Option<String> {
        None
    }
    fn pause(&mut self, _paused: bool) -> Result<(), String> {
        Ok(())
    }
    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn volume(&mut self, _percent: u8) -> Result<(), String> {
        Ok(())
    }
    fn close(&mut self) {}
}

struct Route(Arc<AtomicUsize>);

impl Factory for Route {
    fn open(&self) -> Result<Box<dyn Output>, String> {
        Ok(Box::new(Silent(Arc::clone(&self.0))))
    }
}

#[test]
fn locking_down_the_camera_and_sound_stops_them_while_the_session_runs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    media::audio_output::install(Arc::new(Route(Arc::clone(&writes))));
    let policy = LockdownBackend::for_session();
    policy
        .set_policy(LockdownPolicy {
            disable_camera: true,
            disable_sound_output: true,
            ..LockdownPolicy::default()
        })
        .expect("the person's policy");

    let bus = PrivateBus::start();
    let wake = tilo_service::LoopWake::default();
    let (side, _link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = DesktopServices::start(
        &Buses {
            session: Bus::Address(bus.address().to_owned()),
            system: Bus::None,
        },
        &wake.handle(),
        Arc::new(tilo_portal::NoCapture),
        &side,
        None,
    )
    .expect("the desktop services");

    // Nothing plays while the switch is on, though the route is served.
    assert!(
        media::audio_output::open().is_err(),
        "no sound opens while the person turned it off"
    );
    let portal = desktop.portal().expect("the portal serves");
    let captures = portal.captures();
    let binding = CaptureSessionBinding::new("so.tilo.meet", "room", "/session/1", "device")
        .expect("binding");
    {
        let mut broker = captures.broker().lock().unwrap();
        broker
            .register_source(
                "camera-0",
                "Built-in camera",
                CaptureKind::Camera,
                LocalCaptureHandle::SystemDefault(CaptureKind::Camera),
            )
            .expect("a camera to capture from");
        assert_eq!(
            broker.authorize(
                &binding,
                "camera-0",
                CaptureKind::Camera,
                std::time::Instant::now()
            ),
            Err(CaptureBrokerError::Disabled),
            "the camera is not handed out while the person turned it off"
        );
        drop(broker);
    }

    // Turning the switches off again in the running session lets both back in.
    policy
        .set_policy(LockdownPolicy::default())
        .expect("the person's policy");
    assert!(
        media::audio_output::open().is_ok(),
        "sound plays again once the person allows it"
    );
    assert!(
        captures
            .broker()
            .lock()
            .unwrap()
            .authorize(
                &binding,
                "camera-0",
                CaptureKind::Camera,
                std::time::Instant::now()
            )
            .is_ok(),
        "the camera is handed out again"
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        0,
        "nothing was played meanwhile"
    );
}
