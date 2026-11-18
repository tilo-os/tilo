//! A pseudo-terminal master whose size and foreground process group are asked of the OS.
//!
//! A terminal that shows a session someone else opened receives the master's descriptor and
//! knows nothing else of the process: the window's size, and where the guest's process group
//! stands, are `ioctl`s on that descriptor.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, RawFd};

/// The shape of a terminal: its rows and columns, and its pixels where the OS names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WinSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// A PTY master over a descriptor this process owns.
#[derive(Debug)]
pub struct Master {
    file: File,
}

impl Master {
    #[must_use]
    pub fn new(file: File) -> Self {
        Self { file }
    }

    /// The size the terminal last had.
    ///
    /// # Errors
    ///
    /// The descriptor is not a PTY, or the OS refused the call.
    pub fn get_size(&self) -> io::Result<WinSize> {
        // SAFETY: `winsize` is fully written by the ioctl; the file is a live pty master.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let rc = unsafe { libc::ioctl(self.raw_fd(), libc::TIOCGWINSZ, &mut ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(WinSize {
            rows: ws.ws_row,
            cols: ws.ws_col,
            pixel_width: ws.ws_xpixel,
            pixel_height: ws.ws_ypixel,
        })
    }

    /// Resize the terminal, so its guest reflows.
    ///
    /// # Errors
    ///
    /// The descriptor is not a PTY, or the OS refused the call.
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        let ws = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        // SAFETY: `ws` lives for the call, and the file is a live pty master.
        let rc = unsafe { libc::ioctl(self.raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The process group the guest leads, when it leads one.
    #[must_use]
    pub fn process_group_leader(&self) -> Option<i32> {
        let mut pgrp: i32 = -1;
        // SAFETY: `TIOCGPGRP` writes one `pid_t`; the file is a pty master.
        let rc = unsafe { libc::ioctl(self.raw_fd(), libc::TIOCGPGRP, &mut pgrp) };
        (rc == 0 && pgrp > 0).then_some(pgrp)
    }

    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Another handle on the same master.
    ///
    /// # Errors
    ///
    /// The OS refused to duplicate it.
    pub fn try_clone(&self) -> io::Result<File> {
        self.file.try_clone()
    }
}
