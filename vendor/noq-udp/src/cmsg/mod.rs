use std::{
    ffi::{c_int, c_uchar},
    ptr,
};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

pub(crate) use imp::Aligned;

/// Helper to encode a series of control messages (native "cmsgs") to a buffer for use in `sendmsg`
//  like API.
///
/// The operation must be "finished" for the native msghdr to be usable, either by calling `finish`
/// explicitly or by dropping the `Encoder`.
pub(crate) struct Encoder<'a, M: MsgHdr> {
    hdr: &'a mut M,
    cmsg: Option<&'a mut M::ControlMessage>,
    len: usize,
}

impl<'a, M: MsgHdr> Encoder<'a, M> {
    /// # Safety
    /// - `hdr` must contain a suitably aligned pointer to a big enough buffer to hold control messages
    ///   bytes. All bytes of this buffer can be safely written.
    /// - The `Encoder` must be dropped before `hdr` is passed to a system call, and must not be leaked.
    pub(crate) unsafe fn new(hdr: &'a mut M) -> Self {
        Self {
            cmsg: unsafe { hdr.cmsg_first_hdr().as_mut() },
            hdr,
            len: 0,
        }
    }

    /// Append a control message to the buffer.
    ///
    /// # Panics
    /// - If insufficient buffer space remains.
    /// - If `T` has stricter alignment requirements than `M::ControlMessage`
    pub(crate) fn push<T: Copy>(&mut self, level: c_int, ty: c_int, value: T) {
        assert!(align_of::<T>() <= align_of::<M::ControlMessage>());
        let space = M::ControlMessage::cmsg_space(size_of_val(&value));
        assert!(
            self.hdr.control_len() >= self.len + space,
            "control message buffer too small. Required: {}, Available: {}",
            self.len + space,
            self.hdr.control_len()
        );
        let cmsg = self.cmsg.take().expect("no control buffer space remaining");
        cmsg.set(level, ty, M::ControlMessage::cmsg_len(size_of_val(&value)));
        unsafe {
            ptr::write(cmsg.cmsg_data() as *const T as *mut T, value);
        }
        self.len += space;
        self.cmsg = unsafe { self.hdr.cmsg_nxt_hdr(cmsg).as_mut() };
    }

    /// Finishes appending control messages to the buffer
    pub(crate) fn finish(self) {
        // Delegates to the `Drop` impl
    }
}

// Statically guarantees that the encoding operation is "finished" before the control buffer is read
// by `sendmsg` like API.
impl<M: MsgHdr> Drop for Encoder<'_, M> {
    fn drop(&mut self) {
        self.hdr.set_control_len(self.len as _);
    }
}

/// # Safety
///
/// `cmsg` must refer to a native cmsg containing a payload of type `T`
pub(crate) unsafe fn decode<T: Copy, C: CMsgHdr>(cmsg: &impl CMsgHdr) -> T {
    debug_assert_eq!(cmsg.len(), C::cmsg_len(size_of::<T>()));
    // TILO PATCH: a payload need not be aligned for `T`. musl's `cmsghdr` is
    // 4-byte aligned, so the `timespec` of `SCM_TIMESTAMPNS`, which Linux
    // turns on for every socket, sits at a 4-byte boundary; the upstream
    // assertion that `T` is no more aligned than the header panicked on the
    // first packet a static musl build received. Read it unaligned instead.
    unsafe { ptr::read_unaligned(cmsg.cmsg_data() as *const T) }
}

pub(crate) struct Iter<'a, M: MsgHdr> {
    hdr: &'a M,
    cmsg: Option<&'a M::ControlMessage>,
}

impl<'a, M: MsgHdr> Iter<'a, M> {
    /// # Safety
    ///
    /// `hdr` must hold a pointer to memory outliving `'a` which can be soundly read for the
    /// lifetime of the constructed `Iter` and contains a buffer of native cmsgs, i.e. is aligned
    //  for native `cmsghdr`, is fully initialized, and has correct internal links.
    pub(crate) unsafe fn new(hdr: &'a M) -> Self {
        Self {
            hdr,
            cmsg: unsafe { hdr.cmsg_first_hdr().as_ref() },
        }
    }
}

impl<'a, M: MsgHdr> Iterator for Iter<'a, M> {
    type Item = &'a M::ControlMessage;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.cmsg.take()?;
        self.cmsg = unsafe { self.hdr.cmsg_nxt_hdr(current).as_ref() };

        #[cfg(apple_fast)]
        {
            // On MacOS < 14 CMSG_NXTHDR might continuously return a zeroed cmsg. In
            // such case, return `None` instead, thus indicating the end of
            // the cmsghdr chain.
            if current.len() < size_of::<M::ControlMessage>() {
                return None;
            }
        }

        Some(current)
    }
}

// Helper traits for native types for control messages
pub(crate) trait MsgHdr {
    type ControlMessage: CMsgHdr;

    fn cmsg_first_hdr(&self) -> *mut Self::ControlMessage;

    fn cmsg_nxt_hdr(&self, cmsg: &Self::ControlMessage) -> *mut Self::ControlMessage;

    /// Sets the number of control messages added to this `struct msghdr`.
    ///
    /// Note that this is a destructive operation and should only be done as a finalisation
    /// step.
    fn set_control_len(&mut self, len: usize);

    fn control_len(&self) -> usize;
}

pub(crate) trait CMsgHdr {
    fn cmsg_len(length: usize) -> usize;

    fn cmsg_space(length: usize) -> usize;

    fn cmsg_data(&self) -> *mut c_uchar;

    fn set(&mut self, level: c_int, ty: c_int, len: usize);

    fn len(&self) -> usize;
}

#[cfg(unix)]
pub(crate) const LEN: usize = 96;

// TILO PATCH: the decoder reads a payload at whatever alignment the header
// leaves it, as musl's 4-byte-aligned `cmsghdr` does for a `timespec`.
#[cfg(all(test, unix))]
mod tilo_unaligned_tests {
    use super::{CMsgHdr, decode};
    use libc::{c_int, c_uchar, timespec};

    /// A header whose payload sits 4 bytes into an 8-byte-aligned buffer.
    struct FourAligned {
        buffer: std::cell::UnsafeCell<[u64; 4]>,
    }

    impl CMsgHdr for FourAligned {
        fn cmsg_len(length: usize) -> usize {
            length
        }
        fn cmsg_space(length: usize) -> usize {
            length
        }
        fn cmsg_data(&self) -> *mut c_uchar {
            // SAFETY: 4 bytes into a 32-byte buffer stays inside it.
            unsafe { self.buffer.get().cast::<c_uchar>().add(4) }
        }
        fn set(&mut self, _level: c_int, _ty: c_int, _len: usize) {}
        fn len(&self) -> usize {
            std::mem::size_of::<timespec>()
        }
    }

    #[test]
    fn a_timespec_at_a_four_byte_boundary_is_read_whole() {
        let header = FourAligned { buffer: std::cell::UnsafeCell::new([0; 4]) };
        let sent = timespec { tv_sec: 1_794_000_000, tv_nsec: 123_456_789 };
        // SAFETY: the payload spot holds room for a `timespec`.
        unsafe { std::ptr::write_unaligned(header.cmsg_data().cast::<timespec>(), sent) };
        // SAFETY: `header` carries a `timespec` payload.
        let read = unsafe { decode::<timespec, FourAligned>(&header) };
        assert_eq!((read.tv_sec, read.tv_nsec), (sent.tv_sec, sent.tv_nsec));
    }
}
