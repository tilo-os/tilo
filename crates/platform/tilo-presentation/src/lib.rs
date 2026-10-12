//! Platform-owned observations for native input and exact buffer presentation.
//! Missing protocol support returns false while still presenting the buffer.
pub mod clock;
mod damage;
pub mod text_input;

pub use damage::DamageRect;
#[cfg(all(feature = "gpu-damage", not(target_arch = "wasm32")))]
pub use damage::set_next_present_damage;

use std::time::Duration;
use winit::{event::WindowEvent, window::Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feedback {
    Presented {
        clock_id: u32,
        timestamp: Duration,
        refresh: Duration,
        sequence: u64,
        flags: u32,
    },
    Discarded,
}

pub const fn supported() -> bool {
    cfg!(target_os = "linux")
}

pub fn key_stamp(event: &WindowEvent) -> Option<(u32, u32)> {
    #[cfg(target_os = "linux")]
    if let WindowEvent::KeyboardInput { event, .. } = event {
        use winit::platform::wayland::KeyEventExtWayland;
        return event.wayland_serial_time();
    }
    let _ = event;
    None
}

pub fn present(
    window: &Window,
    observer: Box<dyn FnOnce(Feedback) + Send>,
    present: impl FnOnce(),
) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::{WaylandPresentation, WindowExtWayland};
        window.present_with_feedback(
            Box::new(move |result| {
                observer(match result {
                    WaylandPresentation::Presented {
                        clock_id,
                        timestamp,
                        refresh,
                        sequence,
                        flags,
                    } => Feedback::Presented {
                        clock_id,
                        timestamp,
                        refresh,
                        sequence,
                        flags,
                    },
                    WaylandPresentation::Discarded => Feedback::Discarded,
                });
            }),
            present,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, observer);
        present();
        false
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod wayland_tests {
    use std::time::Duration;
    use winit::platform::wayland::WaylandPresentation;

    #[test]
    fn standard_wayland_dispatch_decoder_preserves_kernel_clock_and_sequence() {
        assert_eq!(
            WaylandPresentation::from_protocol(
                Some(1),
                (2, 7, 123_456),
                16_666_667,
                (3, 9),
                Some(7)
            ),
            WaylandPresentation::Presented {
                clock_id: 1,
                timestamp: Duration::new((2u64 << 32) | 7, 123_456),
                refresh: Duration::from_nanos(16_666_667),
                sequence: (3u64 << 32) | 9,
                flags: 7,
            }
        );
        for (clock, nanos, flags) in [
            (None, 0, Some(7)),
            (Some(1), 1_000_000_000, Some(7)),
            (Some(1), 0, None),
        ] {
            assert_eq!(
                WaylandPresentation::from_protocol(clock, (0, 1, nanos), 16_666_667, (0, 1), flags),
                WaylandPresentation::Discarded
            );
        }
    }
}
