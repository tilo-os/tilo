//! `wp_cursor_shape_v1`: the cursor the app names is drawn by the desktop, on the desktop's own
//! pointer, with the serial the app passed through.
//!
//! The app's pointer objects belong to its own `wl_seat`; each one's shape is held and set on the
//! desktop's pointer, whose enter serial the app received verbatim. A new desktop is given the
//! held shape when its pointer enters the window, since the app has no reason to ask again.
//! A tablet tool's shape is set on the desktop's shape device for that tool, with the serial of
//! the tool's proximity the app received verbatim.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::{GlobalId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    self as client_device, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::cursor_shape::v1::server::wp_cursor_shape_device_v1 as server_device;
use wayland_protocols::wp::cursor_shape::v1::server::wp_cursor_shape_manager_v1 as server_manager;
use wayland_protocols::wp::tablet::zv2::server::zwp_tablet_tool_v2::ZwpTabletToolV2;

use crate::state::State;

/// Which object of the app a shape device belongs to.
pub(crate) struct DeviceData {
    pointer: Option<WlPointer>,
    tablet_tool: Option<ZwpTabletToolV2>,
}

impl State {
    /// Offer the app the cursor-shape manager once a desktop offers it, at no newer a version.
    pub(crate) fn offer_cursor_shape(&mut self, version: u32) {
        if self.cursor_shape_global.is_none() {
            self.cursor_shape_global = Some(
                self.dh
                    .create_global::<Self, server_manager::WpCursorShapeManagerV1, ()>(
                        version.min(2),
                        (),
                    ),
            );
        }
    }

    /// The app named a cursor shape for one of its pointers: hold it and set it on the desktop's
    /// pointer with the serial the app used.
    fn app_cursor_shape(&mut self, pointer: &WlPointer, serial: u32, shape: server_device::Shape) {
        self.cursor_shapes.insert(pointer.id(), u32::from(shape));
        self.send_cursor_shape(pointer, serial, shape);
    }

    fn send_cursor_shape(&mut self, pointer: &WlPointer, serial: u32, shape: server_device::Shape) {
        let Some((manager, qh, up_pointer)) = self.up.as_ref().and_then(|up| {
            up.cursor_shape
                .clone()
                .map(|manager| (manager, up.qh.clone(), up.pointer.clone()))
        }) else {
            return;
        };
        let key = pointer.id().protocol_id();
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.cursor_shape_devices.entry(key)
        {
            let Some(up_pointer) = up_pointer else {
                return;
            };
            entry.insert(manager.get_pointer(&up_pointer, &qh, ()));
        }
        let Ok(shape) = client_device::Shape::try_from(u32::from(shape)) else {
            return;
        };
        if let Some(device) = self.cursor_shape_devices.get(&key) {
            device.set_shape(serial, shape);
        }
    }

    /// The app named a cursor shape for one of its tablets tools: set it on the desktop's shape
    /// device for that tool, with the serial the app passed on.
    fn app_tablet_tool_shape(
        &mut self,
        tool: &ZwpTabletToolV2,
        serial: u32,
        shape: server_device::Shape,
    ) {
        let Some(up_tool) = self.tablets.up_tool(&tool.id()) else {
            return;
        };
        let Some((manager, qh)) = self.up.as_ref().and_then(|up| {
            up.cursor_shape
                .clone()
                .map(|manager| (manager, up.qh.clone()))
        }) else {
            return;
        };
        let key = tool.id().protocol_id();
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.cursor_shape_devices.entry(key)
        {
            entry.insert(manager.get_tablet_tool_v2(&up_tool, &qh, ()));
        }
        let Ok(shape) = client_device::Shape::try_from(u32::from(shape)) else {
            return;
        };
        if let Some(device) = self.cursor_shape_devices.get(&key) {
            device.set_shape(serial, shape);
        }
    }

    /// The desktop's pointer entered a window: the app's pointer gets the enter serial and, if the
    /// app named a shape earlier, the shape it named.
    pub(crate) fn reapply_cursor_shape(&mut self, pointer: &WlPointer, serial: u32) {
        let Some(shape) = self.cursor_shapes.get(&pointer.id()).copied() else {
            return;
        };
        let Ok(shape) = server_device::Shape::try_from(shape) else {
            return;
        };
        self.send_cursor_shape(pointer, serial, shape);
    }

    /// The desktop went away: its devices went with it, the app's shapes stay.
    pub(crate) fn hold_cursor_shape(&mut self) {
        self.cursor_shape_devices.clear();
    }
}

impl GlobalDispatch<server_manager::WpCursorShapeManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<server_manager::WpCursorShapeManagerV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<server_manager::WpCursorShapeManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &server_manager::WpCursorShapeManagerV1,
        request: server_manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            server_manager::Request::GetPointer {
                cursor_shape_device,
                pointer,
            } => {
                data_init.init(
                    cursor_shape_device,
                    DeviceData {
                        pointer: Some(pointer),
                        tablet_tool: None,
                    },
                );
            }
            server_manager::Request::GetTabletToolV2 {
                cursor_shape_device,
                tablet_tool,
            } => {
                data_init.init(
                    cursor_shape_device,
                    DeviceData {
                        pointer: None,
                        tablet_tool: Some(tablet_tool),
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<server_device::WpCursorShapeDeviceV1, DeviceData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &server_device::WpCursorShapeDeviceV1,
        request: server_device::Request,
        data: &DeviceData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let server_device::Request::SetShape {
            serial,
            shape: smithay::reexports::wayland_server::WEnum::Value(shape),
        } = request
        {
            if let Some(pointer) = &data.pointer {
                state.app_cursor_shape(pointer, serial, shape);
            } else if let Some(tool) = &data.tablet_tool {
                state.app_tablet_tool_shape(tool, serial, shape);
            }
        }
    }
}

wayland_client::delegate_noop!(State: ignore WpCursorShapeManagerV1);
wayland_client::delegate_noop!(State: ignore WpCursorShapeDeviceV1);

/// The app's last cursor shape per pointer, and the desktop's device for each while attached.
pub(crate) type Shapes = HashMap<ObjectId, u32>;
pub(crate) type Devices = HashMap<u32, WpCursorShapeDeviceV1>;
pub(crate) type Global = GlobalId;
