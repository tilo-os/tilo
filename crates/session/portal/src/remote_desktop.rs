//! `org.freedesktop.impl.portal.RemoteDesktop`: an app (a remote-control
//! or collaboration client) driving the keyboard and pointer, and
//! optionally seeing the screen, once the person allowed it.
//!
//! `CreateSession` makes the session; `SelectDevices` records the devices
//! the app wants (and a restore key it kept); `ScreenCast.SelectSources`
//! may add a share; `Start` asks the person once for all of it, a
//! question of kind [`REMOTE_DESKTOP_QUESTION`]. From then on the
//! `Notify*` calls, or an EIS connection ([`crate::eis`]), reach the
//! session's [`InputInjector`] for the devices allowed. A started session
//! is published so the person sees who drives the machine and may end it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};

use crate::reply::{Ended, OrFail, Reply, Results, answer, ensure};
use crate::restore::{Grant, restore_key};
use crate::screencast::{Ask, remember, share, streams_value};
use crate::session::{SessionKind, Sessions};
use crate::{Portal, option};

/// The kind of the question that allows a remote desktop.
pub const REMOTE_DESKTOP_QUESTION: &str = "portal.remote-desktop";
const CONTROL_PREFIX: &str = "portal:control:";

/// `AvailableDeviceTypes` bits.
pub mod device_type {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    /// Touch has no delivery path yet, so it is never offered.
    pub const ALL: u32 = KEYBOARD | POINTER;
}

/// One input event an app drives the session with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RemoteInput {
    PointerMotion {
        dx: f64,
        dy: f64,
    },
    /// Absolute, in the stream `stream`'s coordinates.
    PointerMotionAbsolute {
        stream: u32,
        x: f64,
        y: f64,
    },
    /// An evdev button code.
    PointerButton {
        button: i32,
        pressed: bool,
    },
    PointerAxis {
        dx: f64,
        dy: f64,
    },
    /// `axis` 0 vertical, 1 horizontal.
    PointerAxisDiscrete {
        axis: u32,
        steps: i32,
    },
    /// An evdev key code.
    KeyboardKeycode {
        keycode: i32,
        pressed: bool,
    },
    KeyboardKeysym {
        keysym: i32,
        pressed: bool,
    },
}

impl RemoteInput {
    /// The device it needs.
    #[must_use]
    pub const fn device(&self) -> u32 {
        match self {
            Self::KeyboardKeycode { .. } | Self::KeyboardKeysym { .. } => device_type::KEYBOARD,
            _ => device_type::POINTER,
        }
    }
}

/// The session's seat, as remote input reaches it.
pub trait InputInjector: Send + Sync {
    /// Whether it can take input at all.
    fn available(&self) -> bool;

    /// Deliver `input` to the seat as if a device sent it; whether it was
    /// taken.
    fn inject(&self, input: RemoteInput) -> bool;
}

/// A session no app may drive.
pub struct NoInput;

impl InputInjector for NoInput {
    fn available(&self) -> bool {
        false
    }

    fn inject(&self, _input: RemoteInput) -> bool {
        false
    }
}

/// What ending a remote desktop offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.remote-control")]
pub enum ControlEffect {
    /// Stop the app controlling this computer
    #[effect(own_device, idempotent)]
    End,
}

#[derive(Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the portal's lifecycle flags"
)]
struct Remote {
    devices: u32,
    selected: bool,
    persist_mode: u32,
    restore: Option<Grant>,
    starting: bool,
    started: bool,
    clipboard: bool,
    /// Read by the EIS server before every event: an ended session's
    /// connection injects nothing more.
    live: Arc<AtomicBool>,
    eis: bool,
}

/// The remote desktop sessions, by session path.
#[derive(Clone, Default)]
pub struct Remotes {
    remotes: Arc<Mutex<HashMap<String, Remote>>>,
}

impl Remotes {
    pub(crate) fn follow(&self, sessions: &Sessions) {
        let remotes = self.clone();
        sessions.on_end(move |path| {
            let removed = remotes.remotes.lock_recover().remove(path);
            if let Some(remote) = removed {
                remote.live.store(false, Ordering::Release);
            }
        });
    }

    /// Mark session `path` starting; what it selected.
    fn begin(&self, path: &str) -> Result<Remote, Ended> {
        let mut remotes = self.remotes.lock_recover();
        let remote = remotes.get_mut(path).or_fail()?;
        if !remote.selected || remote.starting || remote.started {
            return Err(Ended::Failed);
        }
        remote.starting = true;
        let remote = remote.clone();
        drop(remotes);
        Ok(remote)
    }

    /// Session `path`'s start ended, `started` or not; whether it still
    /// lives.
    fn finish(&self, path: &str, started: bool) -> bool {
        let mut remotes = self.remotes.lock_recover();
        let Some(remote) = remotes.get_mut(path) else {
            return false;
        };
        remote.starting = false;
        remote.started = started;
        remote.live.store(started, Ordering::Release);
        drop(remotes);
        true
    }

    /// Session `path`'s one EIS connection: whether it lives, and its
    /// devices; `None` before its start or once connected.
    fn connect_eis(&self, path: &str) -> Option<(Arc<AtomicBool>, u32)> {
        let mut remotes = self.remotes.lock_recover();
        let remote = remotes
            .get_mut(path)
            .filter(|remote| remote.started && !remote.eis)?;
        remote.eis = true;
        let connected = (Arc::clone(&remote.live), remote.devices);
        drop(remotes);
        Some(connected)
    }

    /// Whether session `path` is started and allows `devices`.
    fn allows(&self, path: &str, devices: u32) -> bool {
        self.remotes
            .lock_recover()
            .get(path)
            .is_some_and(|remote| remote.started && remote.devices & devices != 0)
    }

    /// Ask for the clipboard on session `path`, before it started.
    pub(crate) fn request_clipboard(&self, path: &str) -> bool {
        self.remotes
            .lock_recover()
            .get_mut(path)
            .is_some_and(|remote| {
                if remote.started || remote.starting {
                    return false;
                }
                remote.clipboard = true;
                true
            })
    }

    /// Whether session `path` is started with the clipboard.
    pub(crate) fn clipboard(&self, path: &str) -> bool {
        self.remotes
            .lock_recover()
            .get(path)
            .is_some_and(|remote| remote.started && remote.clipboard)
    }

    /// The started sessions with the clipboard.
    pub(crate) fn clipboard_sessions(&self) -> Vec<String> {
        self.remotes
            .lock_recover()
            .iter()
            .filter(|(_, remote)| remote.started && remote.clipboard)
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn started(&self) -> Vec<(String, u32)> {
        let mut started: Vec<_> = self
            .remotes
            .lock_recover()
            .iter()
            .filter(|(_, remote)| remote.started)
            .map(|(path, remote)| (path.clone(), remote.devices))
            .collect();
        started.sort();
        started
    }
}

fn devices_label(devices: u32) -> String {
    [
        (device_type::KEYBOARD, "keyboard"),
        (device_type::POINTER, "pointer"),
    ]
    .iter()
    .filter(|(bit, _)| devices & bit != 0)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// The sessions that control this computer's input: started remote
/// desktops and input captures, each published with an action ending it.
#[derive(Clone)]
pub struct Controls {
    pub(crate) sessions: Sessions,
    pub(crate) remotes: Remotes,
    pub(crate) captures: crate::input_capture::InputCaptures,
    pub(crate) casts: crate::screencast::Casts,
}

impl Controls {
    /// Every controlling session: its path, app and what it controls.
    fn all(&self) -> Vec<(String, String, String)> {
        let remotes = self
            .remotes
            .started()
            .into_iter()
            .map(|(path, devices)| (path, devices_label(devices)));
        let captures = self
            .captures
            .started()
            .into_iter()
            .map(|path| (path, "input-capture".to_owned()));
        remotes
            .chain(captures)
            .filter_map(|(path, what)| {
                let app_id = self.sessions.get(&path)?.caller.app_id;
                Some((path, app_id, what))
            })
            .collect()
    }
}

impl crate::Published for Controls {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.all()
            .into_iter()
            .map(|(path, app_id, what)| {
                offer::<ControlEffect>(ResourceSpec::new(
                    format!("{CONTROL_PREFIX}{path}"),
                    "portal.remote-control",
                    app_id,
                ))
                .with_presentation(ResourcePresentation {
                    detail: Some(what),
                    badge: None,
                    preview: None,
                })
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let path = resource.strip_prefix(CONTROL_PREFIX)?;
        let (_, app_id, what) = self.all().into_iter().find(|(live, _, _)| live == path)?;
        Some(serde_json::json!({
            "app_id": app_id,
            "controls": what,
            "streams": self.casts.nodes(path).len(),
        }))
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let path = resource.strip_prefix(CONTROL_PREFIX)?;
        Some(
            decode::<ControlEffect>(published, resource, action, arguments).and_then(|_| {
                self.sessions
                    .end(path)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(|| {
                        DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action))
                    })
            }),
        )
    }
}

/// The `org.freedesktop.impl.portal.RemoteDesktop` interface.
pub struct RemoteDesktop {
    portal: Portal,
}

impl RemoteDesktop {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    /// Deliver `input` for the sender of a `Notify*` call when its session
    /// is started and allows the device.
    fn notify(
        &self,
        header: &zbus::message::Header<'_>,
        session: &ObjectPath<'_>,
        input: RemoteInput,
    ) {
        let path = session.as_str();
        let owned = header.sender().is_some_and(|sender| {
            self.portal
                .sessions
                .get(path)
                .is_some_and(|live| live.owner == sender.as_str())
        });
        if owned && self.portal.remotes.allows(path, input.device()) {
            self.portal.input.inject(input);
        } else {
            tracing::debug!(?input, "remote input of no started session dropped");
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktop {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        if self.portal.input.available() {
            device_type::ALL
        } else {
            0
        }
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            ensure(self.portal.input.available())?;
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            let path = session_handle.as_str();
            self.portal
                .sessions
                .create(
                    connection,
                    path,
                    auth.owner.as_str(),
                    auth.caller,
                    SessionKind::RemoteDesktop,
                )
                .await?;
            self.portal
                .remotes
                .remotes
                .lock_recover()
                .insert(path.to_owned(), Remote::default());
            Ok(Results::new())
        })
        .await
    }

    async fn select_devices(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            let path = session_handle.as_str();
            ensure(
                self.portal
                    .sessions
                    .owned(path, auth.owner.as_str(), &auth.caller)
                    == Some(SessionKind::RemoteDesktop),
            )?;
            let persist_mode = option::<u32>(&options, "persist_mode").unwrap_or(0);
            ensure(persist_mode <= 2)?;
            let restore = restore_key(&options)
                .and_then(|key| self.portal.grants.get(&key, &auth.caller.app_id))
                .filter(|grant| grant.devices != 0);
            let mut devices = restore.as_ref().map_or_else(
                || option::<u32>(&options, "types").unwrap_or(device_type::ALL),
                |grant| grant.devices,
            ) & device_type::ALL;
            if devices == 0 {
                devices = device_type::ALL;
            }
            let selected = self
                .portal
                .remotes
                .remotes
                .lock_recover()
                .get_mut(path)
                .is_some_and(|remote| {
                    if remote.starting || remote.started {
                        return false;
                    }
                    remote.devices = devices;
                    remote.selected = true;
                    remote.persist_mode = persist_mode;
                    remote.restore = restore;
                    true
                });
            ensure(selected)?;
            Ok(Results::new())
        })
        .await
    }

    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            ensure(self.portal.input.available())?;
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            let path = session_handle.as_str();
            ensure(
                self.portal
                    .sessions
                    .owned(path, auth.owner.as_str(), &auth.caller)
                    == Some(SessionKind::RemoteDesktop),
            )?;
            let remote = self.portal.remotes.begin(path)?;
            let mut selection = self.portal.casts.selection(path);
            if let Some(selection) = &mut selection {
                selection.persist_mode = remote.persist_mode;
                ensure(self.portal.casts.begin(path))?;
            }
            let mut restore = remote.restore.clone();
            if let Some(grant) = &mut restore {
                grant.persist_mode = grant.persist_mode.max(remote.persist_mode.min(1));
            }
            let request = handle.as_str();
            let shared = self
                .portal
                .requests
                .run(
                    connection,
                    request,
                    auth.owner.as_str(),
                    share(
                        &self.portal,
                        Ask {
                            path,
                            request,
                            caller: &auth.caller,
                            selection: selection.as_ref(),
                            restore: restore.as_ref(),
                            kind: REMOTE_DESKTOP_QUESTION,
                            devices: remote.devices,
                        },
                    ),
                )
                .await;
            let mut shared = shared.inspect_err(|_| {
                self.portal.casts.finish(path, None);
                self.portal.remotes.finish(path, false);
            })?;
            if remote.restore.is_none() {
                shared.persist_mode = shared.persist_mode.min(remote.persist_mode);
            }
            let nodes: Vec<u32> = shared.streams.iter().map(|(_, s)| s.node_id).collect();
            let started = self.portal.remotes.finish(path, true);
            if !started
                || (selection.is_some() && !self.portal.casts.finish(path, Some(nodes.clone())))
            {
                for node in nodes {
                    self.portal.streams.stop(node);
                }
                return Err(Ended::Cancelled);
            }
            self.portal.sessions.changed();
            let mut results = Results::new();
            results.insert("devices".into(), OwnedValue::from(remote.devices));
            results.insert(
                "clipboard_enabled".into(),
                OwnedValue::from(remote.clipboard),
            );
            if !shared.streams.is_empty() {
                results.insert("streams".into(), streams_value(&shared.streams).or_fail()?);
            }
            let cursor = selection.map_or(0, |selection| selection.cursor_mode);
            remember(
                &self.portal,
                &auth.caller,
                &shared,
                cursor,
                remote.devices,
                &mut results,
            )?;
            Ok(results)
        })
        .await
    }

    /// The EIS transport: a socket whose events reach the seat like the
    /// `Notify*` calls, for the devices allowed.
    async fn connect_to_eis(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedFd> {
        let path = session_handle.as_str();
        let owned = self
            .portal
            .authenticate_session(&header, path, &app_id)
            .await
            .is_ok_and(|(_, session)| session.kind == SessionKind::RemoteDesktop);
        let connected = owned
            .then(|| self.portal.remotes.connect_eis(path))
            .flatten();
        let (live, devices) = connected.ok_or_else(|| {
            zbus::fdo::Error::AccessDenied(
                "the session is not started, already connected, or not this caller's".into(),
            )
        })?;
        crate::eis::serve_sender(live, devices, Arc::clone(&self.portal.input))
            .map(OwnedFd::from)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_pointer_motion(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::PointerMotion { dx, dy },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_pointer_motion_absolute(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::PointerMotionAbsolute { stream, x, y },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_pointer_button(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::PointerButton {
                button,
                pressed: state == 1,
            },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_pointer_axis(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::PointerAxis { dx, dy },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_pointer_axis_discrete(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::PointerAxisDiscrete { axis, steps },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_keyboard_keycode(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::KeyboardKeycode {
                keycode,
                pressed: state == 1,
            },
        );
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "zbus hands arguments over owned"
    )]
    fn notify_keyboard_keysym(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keysym: i32,
        state: u32,
    ) {
        self.notify(
            &header,
            &session_handle,
            RemoteInput::KeyboardKeysym {
                keysym,
                pressed: state == 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_started_session_drives_and_only_its_devices() {
        let remotes = Remotes::default();
        let path = "/org/freedesktop/portal/desktop/session/1_2/r";
        remotes.remotes.lock_recover().insert(
            path.into(),
            Remote {
                devices: device_type::POINTER,
                selected: true,
                ..Remote::default()
            },
        );
        assert!(!remotes.allows(path, device_type::POINTER), "not started");
        assert!(
            remotes.request_clipboard(path),
            "the clipboard is asked for before"
        );
        remotes
            .remotes
            .lock_recover()
            .get_mut(path)
            .expect("live")
            .started = true;
        assert!(remotes.allows(path, device_type::POINTER));
        assert!(!remotes.allows(path, device_type::KEYBOARD));
        assert!(
            !remotes.request_clipboard(path),
            "and never after the start"
        );
        assert!(remotes.clipboard(path));
        assert_eq!(
            RemoteInput::KeyboardKeysym {
                keysym: 0x61,
                pressed: true
            }
            .device(),
            device_type::KEYBOARD
        );
        assert_eq!(devices_label(device_type::ALL), "keyboard, pointer");
    }
}
