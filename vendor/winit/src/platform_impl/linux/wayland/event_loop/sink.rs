//! An event loop's sink to deliver events from the Wayland event callbacks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::vec::Drain;

use crate::event::{DeviceEvent, DeviceId as RootDeviceId, Event, WindowEvent};
use crate::platform_impl::platform::DeviceId as PlatformDeviceId;
use crate::window::WindowId as RootWindowId;

use super::{DeviceId, WindowId};

/// An event loop's sink to deliver events from the Wayland event callbacks
/// to the winit's user.
#[derive(Default)]
pub struct EventSink {
    pub window_events: Vec<Event<()>>,
    /// The compositor's serial of each input event (a key, a button, a touch going down or up),
    /// by how many events were queued once its own were: it is delivered once they are.
    input_serials: Vec<(usize, u32)>,
}

/// The serial of the newest input event the compositor sent that the application has been
/// handed, with 1 << 32 set once there is one.
static DELIVERED_INPUT_SERIAL: AtomicU64 = AtomicU64::new(0);

/// The serial of the newest input event the application has been handed: see
/// [`crate::platform::wayland::delivered_input_serial`].
pub fn delivered_input_serial() -> Option<u32> {
    let delivered = DELIVERED_INPUT_SERIAL.load(Ordering::Acquire);
    (delivered >> 32 != 0).then_some(delivered as u32)
}

fn deliver_input_serial(serial: u32) {
    DELIVERED_INPUT_SERIAL.store((1 << 32) | u64::from(serial), Ordering::Release);
}

impl EventSink {
    pub fn new() -> Self {
        Default::default()
    }

    /// Return `true` if there're pending events.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.window_events.is_empty()
    }

    /// Add new device event to a queue.
    #[inline]
    pub fn push_device_event(&mut self, event: DeviceEvent, device_id: DeviceId) {
        self.window_events.push(Event::DeviceEvent {
            event,
            device_id: RootDeviceId(PlatformDeviceId::Wayland(device_id)),
        });
    }

    /// Add new window event to a queue.
    #[inline]
    pub fn push_window_event(&mut self, event: WindowEvent, window_id: WindowId) {
        self.window_events.push(Event::WindowEvent {
            event,
            window_id: RootWindowId(window_id),
        });
    }

    /// The input event with the compositor's `serial` was queued (or dropped, having no
    /// window): it counts as delivered once the events queued so far are.
    #[inline]
    pub fn push_input_serial(&mut self, serial: u32) {
        self.input_serials.push((self.window_events.len(), serial));
    }

    #[inline]
    pub fn append(&mut self, other: &mut Self) {
        let queued = self.window_events.len();
        self.input_serials.extend(
            other
                .input_serials
                .drain(..)
                .map(|(before, serial)| (before + queued, serial)),
        );
        self.window_events.append(&mut other.window_events);
    }

    /// Hand each queued event to `deliver`, oldest first, counting the input serials of the
    /// events handed so far as delivered before each.
    pub fn deliver(&mut self, mut deliver: impl FnMut(Event<()>)) {
        let mut serials = std::mem::take(&mut self.input_serials)
            .into_iter()
            .peekable();
        for (index, event) in self.window_events.drain(..).enumerate() {
            while let Some((_, serial)) = serials.next_if(|(before, _)| *before <= index) {
                deliver_input_serial(serial);
            }
            deliver(event);
        }
        for (_, serial) in serials {
            deliver_input_serial(serial);
        }
    }

    #[inline]
    pub fn drain(&mut self) -> Drain<'_, Event<()>> {
        self.window_events.drain(..)
    }
}
