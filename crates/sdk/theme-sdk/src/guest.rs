//! The guest side of a painter set loaded from wasm: one buffer the host
//! writes a request into and reads the answer from, and one call. Reached
//! through [`crate::export_painter!`].
//!
//! The host asks for a buffer of the request's length
//! (`tilo_theme_buffer(len) -> address`), writes the request there, and
//! calls `tilo_theme_call(len) -> address << 32 | answer length`. The
//! module imports nothing: it has no clock, no I/O and no host to ask.

use std::cell::RefCell;

use crate::PartPainter;
use crate::wire::{self, Request};

thread_local! {
    static BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn address(buffer: &[u8]) -> u32 {
    u32::try_from(buffer.as_ptr().addr()).unwrap_or(0)
}

/// Make the buffer `len` bytes long and say where it is.
#[must_use]
pub fn buffer(len: u32) -> u32 {
    BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        buffer.clear();
        buffer.resize(usize::try_from(len).unwrap_or(0), 0);
        address(&buffer)
    })
}

/// Answer the request in the buffer's first `len` bytes with `painter`,
/// leaving the answer in the buffer.
#[must_use]
pub fn call(painter: &dyn PartPainter, len: u32) -> u64 {
    BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        let len = usize::try_from(len).unwrap_or(0).min(buffer.len());
        let answer = match buffer.get(..len).and_then(wire::request) {
            Some(Request::Paint(request)) => wire::paint_answer(
                painter
                    .paint(
                        request.part,
                        request.size,
                        request.state,
                        request.progress,
                        &request.tokens,
                    )
                    .as_ref(),
            ),
            Some(Request::Transition { part, from, to }) => {
                wire::transition_answer(painter.transition(part, from, to))
            }
            None => Vec::new(),
        };
        *buffer = answer;
        (u64::from(address(&buffer)) << 32) | u64::try_from(buffer.len()).unwrap_or(0)
    })
}

/// Export `painter` (a `static` or a constant expression of a type that
/// implements [`crate::PartPainter`]) as this wasm module's painter set.
#[macro_export]
macro_rules! export_painter {
    ($painter:expr) => {
        #[allow(unsafe_code, reason = "an export's symbol name is its ABI")]
        #[unsafe(no_mangle)]
        pub extern "C" fn tilo_theme_buffer(len: u32) -> u32 {
            $crate::guest::buffer(len)
        }

        #[allow(unsafe_code, reason = "an export's symbol name is its ABI")]
        #[unsafe(no_mangle)]
        pub extern "C" fn tilo_theme_call(len: u32) -> u64 {
            $crate::guest::call(&$painter, len)
        }
    };
}
