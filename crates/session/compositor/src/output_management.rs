//! `zwlr_output_manager_v1`: display configuration for external tools
//! (`wlr-randr`, `kanshi`, `way-displays`).
//!
//! The global changes what every display shows, so only clients the session
//! trusts see it (as with session lock). Each bound manager gets a head per
//! output and a mode object per mode; [`OutputManagement::publish`] keeps
//! them current after every change. A tool's `apply` (or `test`) becomes a
//! list of [`OutputChange`]s the backend validates and applies
//! ([`crate::Backend::configure`]); the configuration then succeeds or fails.

use std::collections::HashMap;
use std::sync::Mutex;

use smithay::output::{Mode, Output};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::Transform;

use crate::output::{ModeRequest, OutputChange};
use crate::state::{ClientState, State};

/// The protocol version offered.
const VERSION: u32 = 4;

/// What a head shows, as last told to clients.
#[derive(Clone, Debug, PartialEq)]
struct Head {
    name: String,
    description: String,
    make: String,
    model: String,
    physical_size: (i32, i32),
    modes: Vec<Mode>,
    preferred: Option<Mode>,
    current: Option<Mode>,
    position: (i32, i32),
    scale: f64,
    transform: Transform,
    enabled: bool,
}

impl Head {
    fn of(output: &Output, enabled: bool) -> Self {
        let properties = output.physical_properties();
        let location = output.current_location();
        Self {
            name: output.name(),
            description: output.description(),
            make: properties.make,
            model: properties.model,
            physical_size: (properties.size.w, properties.size.h),
            modes: output.modes(),
            preferred: output.preferred_mode(),
            current: output.current_mode(),
            position: (location.x, location.y),
            scale: output.current_scale().fractional_scale(),
            transform: output.current_transform(),
            enabled,
        }
    }
}

/// One output's head and mode objects for one manager.
struct BoundHead {
    head: ZwlrOutputHeadV1,
    modes: Vec<(Mode, ZwlrOutputModeV1)>,
    shown: Head,
}

struct BoundManager {
    manager: ZwlrOutputManagerV1,
    heads: HashMap<String, BoundHead>,
}

/// A configuration waiting for the backend.
pub(crate) struct Pending {
    pub(crate) configuration: ZwlrOutputConfigurationV1,
    pub(crate) changes: Vec<OutputChange>,
    /// Only check it (`test`), do not apply it.
    pub(crate) test: bool,
}

/// The global's state: bound managers and configurations to apply.
pub(crate) struct OutputManagement {
    managers: Vec<BoundManager>,
    heads: Vec<Head>,
    serial: u32,
    pending: Vec<Pending>,
}

/// A head object's output.
pub struct HeadData {
    name: String,
}

/// A mode object's mode.
pub struct ModeData {
    mode: Mode,
}

/// A configuration under construction: the serial it was made against and
/// the heads it names.
pub struct ConfigurationData {
    serial: u32,
    changes: Mutex<Option<Vec<OutputChange>>>,
}

/// A configuration head: which configuration and output it changes.
pub struct ConfigurationHeadData {
    configuration: ZwlrOutputConfigurationV1,
    name: String,
}

impl OutputManagement {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, ZwlrOutputManagerV1, ()>(VERSION, ());
        Self {
            managers: Vec::new(),
            heads: Vec::new(),
            serial: 0,
            pending: Vec::new(),
        }
    }

    /// The configurations clients applied or tested since the last call.
    pub(crate) fn take_pending(&mut self) -> Vec<Pending> {
        std::mem::take(&mut self.pending)
    }

    /// Tell every manager about outputs that came, went or changed.
    pub(crate) fn publish<'a>(
        &mut self,
        display: &DisplayHandle,
        outputs: impl Iterator<Item = &'a (Output, bool)>,
    ) {
        let heads: Vec<Head> = outputs
            .map(|(output, enabled)| Head::of(output, *enabled))
            .collect();
        if heads == self.heads {
            return;
        }
        self.heads = heads;
        self.serial = self.serial.wrapping_add(1);
        for bound in &mut self.managers {
            sync_manager(display, bound, &self.heads, self.serial);
        }
    }

    fn bind(&mut self, display: &DisplayHandle, manager: ZwlrOutputManagerV1) {
        let mut bound = BoundManager {
            manager,
            heads: HashMap::new(),
        };
        sync_manager(display, &mut bound, &self.heads, self.serial);
        self.managers.push(bound);
    }

    fn forget(&mut self, manager: &ZwlrOutputManagerV1) {
        self.managers.retain(|bound| &bound.manager != manager);
    }
}

/// Bring one manager's heads in line with `heads`, then send `done`.
fn sync_manager(display: &DisplayHandle, bound: &mut BoundManager, heads: &[Head], serial: u32) {
    let gone: Vec<String> = bound
        .heads
        .keys()
        .filter(|name| !heads.iter().any(|head| &head.name == *name))
        .cloned()
        .collect();
    for name in gone {
        if let Some(old) = bound.heads.remove(&name) {
            for (_, mode) in &old.modes {
                mode.finished();
            }
            old.head.finished();
        }
    }
    for head in heads {
        match bound.heads.get_mut(&head.name) {
            Some(existing) => update_head(display, &bound.manager, existing, head),
            None => {
                if let Some(new) = new_head(display, &bound.manager, head) {
                    bound.heads.insert(head.name.clone(), new);
                }
            }
        }
    }
    bound.manager.done(serial);
}

fn new_head(
    display: &DisplayHandle,
    manager: &ZwlrOutputManagerV1,
    head: &Head,
) -> Option<BoundHead> {
    let client = manager.client()?;
    let object = client
        .create_resource::<ZwlrOutputHeadV1, HeadData, State>(
            display,
            manager.version(),
            HeadData {
                name: head.name.clone(),
            },
        )
        .ok()?;
    manager.head(&object);
    object.name(head.name.clone());
    object.description(head.description.clone());
    if head.physical_size.0 > 0 && head.physical_size.1 > 0 {
        object.physical_size(head.physical_size.0, head.physical_size.1);
    }
    if object.version() >= 2 {
        object.make(head.make.clone());
        object.model(head.model.clone());
    }
    let mut bound = BoundHead {
        head: object,
        modes: Vec::new(),
        shown: head.clone(),
    };
    for mode in &head.modes {
        add_mode(
            display,
            &client,
            &mut bound,
            *mode,
            head.preferred == Some(*mode),
        );
    }
    send_state(&bound, head);
    Some(bound)
}

fn add_mode(
    display: &DisplayHandle,
    client: &Client,
    bound: &mut BoundHead,
    mode: Mode,
    preferred: bool,
) {
    let Ok(object) = client.create_resource::<ZwlrOutputModeV1, ModeData, State>(
        display,
        bound.head.version(),
        ModeData { mode },
    ) else {
        return;
    };
    bound.head.mode(&object);
    object.size(mode.size.w, mode.size.h);
    if mode.refresh > 0 {
        object.refresh(mode.refresh);
    }
    if preferred {
        object.preferred();
    }
    bound.modes.push((mode, object));
}

fn update_head(
    display: &DisplayHandle,
    manager: &ZwlrOutputManagerV1,
    bound: &mut BoundHead,
    head: &Head,
) {
    if bound.shown == *head {
        return;
    }
    if let Some(client) = manager.client() {
        for mode in &head.modes {
            if !bound.modes.iter().any(|(known, _)| known == mode) {
                add_mode(
                    display,
                    &client,
                    bound,
                    *mode,
                    head.preferred == Some(*mode),
                );
            }
        }
    }
    bound.modes.retain(|(mode, object)| {
        let kept = head.modes.contains(mode);
        if !kept {
            object.finished();
        }
        kept
    });
    send_state(bound, head);
    bound.shown = head.clone();
}

/// The head's changing properties: enabled, and while it is, mode,
/// position, transform, scale.
fn send_state(bound: &BoundHead, head: &Head) {
    let object = &bound.head;
    object.enabled(i32::from(head.enabled));
    if !head.enabled {
        return;
    }
    if let Some((_, mode)) = bound
        .modes
        .iter()
        .find(|(mode, _)| Some(*mode) == head.current)
    {
        object.current_mode(mode);
    }
    object.position(head.position.0, head.position.1);
    object.transform(head.transform.into());
    object.scale(head.scale);
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        state.output_management.bind(display, manager);
    }

    fn can_view(client: Client, _data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(ClientState::trusted)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    ConfigurationData {
                        serial,
                        changes: Mutex::new(Some(Vec::new())),
                    },
                );
            }
            zwlr_output_manager_v1::Request::Stop => {
                state.output_management.forget(manager);
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ZwlrOutputManagerV1, _data: &()) {
        state.output_management.forget(manager);
    }
}

impl Dispatch<ZwlrOutputHeadV1, HeadData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _head: &ZwlrOutputHeadV1,
        _request: zwlr_output_head_v1::Request,
        _data: &HeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `release` only: the object goes away with no answer.
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        _request: zwlr_output_mode_v1::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ConfigurationData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        configuration: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &ConfigurationData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(mut changes) = data.changes.lock() else {
            return;
        };
        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, head } => {
                let Some(name) = head.data::<HeadData>().map(|head| head.name.clone()) else {
                    return;
                };
                let Some(list) = changes.as_mut() else {
                    configuration.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration already applied",
                    );
                    return;
                };
                if list.iter().any(|change| change.name == name) {
                    configuration.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "head configured twice",
                    );
                    return;
                }
                list.push(OutputChange::enable(name.clone()));
                data_init.init(
                    id,
                    ConfigurationHeadData {
                        configuration: configuration.clone(),
                        name,
                    },
                );
            }
            zwlr_output_configuration_v1::Request::DisableHead { head } => {
                let Some(name) = head.data::<HeadData>().map(|head| head.name.clone()) else {
                    return;
                };
                let Some(list) = changes.as_mut() else {
                    configuration.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration already applied",
                    );
                    return;
                };
                if list.iter().any(|change| change.name == name) {
                    configuration.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "head configured twice",
                    );
                    return;
                }
                list.push(OutputChange::disable(name));
            }
            zwlr_output_configuration_v1::Request::Apply
            | zwlr_output_configuration_v1::Request::Test => {
                let test = matches!(request, zwlr_output_configuration_v1::Request::Test);
                let Some(list) = changes.take() else {
                    configuration.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration already applied",
                    );
                    return;
                };
                if data.serial != state.output_management.serial {
                    configuration.cancelled();
                    return;
                }
                state.output_management.pending.push(Pending {
                    configuration: configuration.clone(),
                    changes: list,
                    test,
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        head: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(configuration) = data.configuration.data::<ConfigurationData>() else {
            return;
        };
        let Ok(mut changes) = configuration.changes.lock() else {
            return;
        };
        let Some(change) = changes
            .as_mut()
            .and_then(|list| list.iter_mut().find(|change| change.name == data.name))
        else {
            return;
        };
        match request {
            zwlr_output_configuration_head_v1::Request::SetMode { mode } => {
                if let Some(mode) = mode.data::<ModeData>() {
                    change.mode = Some(ModeRequest {
                        size: (mode.mode.size.w, mode.mode.size.h),
                        refresh_mhz: Some(mode.mode.refresh),
                    });
                }
            }
            zwlr_output_configuration_head_v1::Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if width <= 0 || height <= 0 {
                    head.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidCustomMode,
                        "custom mode without pixels",
                    );
                    return;
                }
                change.mode = Some(ModeRequest {
                    size: (width, height),
                    refresh_mhz: (refresh > 0).then_some(refresh),
                });
            }
            zwlr_output_configuration_head_v1::Request::SetPosition { x, y } => {
                change.position = Some((x, y));
            }
            zwlr_output_configuration_head_v1::Request::SetTransform { transform } => {
                match transform {
                    WEnum::Value(transform) => change.transform = Some(transform.into()),
                    WEnum::Unknown(_) => head.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidTransform,
                        "unknown transform",
                    ),
                }
            }
            zwlr_output_configuration_head_v1::Request::SetScale { scale } => {
                if scale.is_finite() && scale > 0.0 {
                    change.scale = Some(scale);
                } else {
                    head.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidScale,
                        "scale must be positive",
                    );
                }
            }
            _ => {}
        }
    }
}
