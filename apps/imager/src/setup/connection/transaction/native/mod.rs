//! Native evidence stays private, move-only and process-local.
#[cfg(target_os = "linux")]
pub(super) mod linux;

// Shared wire validation is deliberately std-only so its adversarial traces run
// on the development host as well as in the real Linux transport.
const FRAME_BYTES: usize = 40;
const MAX_FRAME: usize = 4096;
const MAX_GUARDS: u32 = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Code {
    Hello = 1,
    Acquire = 2,
    Check = 3,
    Shutdown = 4,
    Acquired = 5,
    Healthy = 6,
    Revoked = 7,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    epoch: [u8; 16],
    request: u64,
    guard: u32,
    code: Code,
}
impl Frame {
    fn encode(self) -> [u8; FRAME_BYTES] {
        let mut bytes = [0; FRAME_BYTES];
        bytes[..4].copy_from_slice(b"QLS1");
        bytes[4] = self.code as u8;
        bytes[8..24].copy_from_slice(&self.epoch);
        bytes[24..32].copy_from_slice(&self.request.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.guard.to_le_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != FRAME_BYTES
            || bytes.get(..4) != Some(b"QLS1")
            || bytes.get(5..8) != Some(&[0; 3])
            || bytes.get(36..40) != Some(&[0; 4])
        {
            return Err(());
        }
        let code = match bytes.get(4) {
            Some(1) => Code::Hello,
            Some(2) => Code::Acquire,
            Some(3) => Code::Check,
            Some(4) => Code::Shutdown,
            Some(5) => Code::Acquired,
            Some(6) => Code::Healthy,
            Some(7) => Code::Revoked,
            _ => return Err(()),
        };
        Ok(Self {
            epoch: bytes.get(8..24).ok_or(())?.try_into().map_err(|_| ())?,
            request: u64::from_le_bytes(bytes.get(24..32).ok_or(())?.try_into().map_err(|_| ())?),
            guard: u32::from_le_bytes(bytes.get(32..36).ok_or(())?.try_into().map_err(|_| ())?),
            code,
        })
    }
}
struct Admission {
    epoch: [u8; 16],
    next: u64,
    guards: u32,
    pending: Option<Frame>,
    revoked: bool,
}
impl Admission {
    fn new(epoch: [u8; 16]) -> Self {
        Self {
            epoch,
            next: 1,
            guards: 0,
            pending: None,
            revoked: false,
        }
    }
    fn revoke(&mut self) {
        self.revoked = true;
        self.pending = None;
    }
    fn request(&mut self, code: Code) -> Result<Frame, ()> {
        if self.revoked
            || self.pending.is_some()
            || self.next == u64::MAX
            || !matches!(code, Code::Acquire | Code::Check | Code::Shutdown)
            || (code == Code::Acquire && self.guards >= MAX_GUARDS)
        {
            self.revoke();
            return Err(());
        }
        let frame = Frame {
            epoch: self.epoch,
            request: self.next,
            guard: if code == Code::Acquire {
                self.guards + 1
            } else {
                0
            },
            code,
        };
        self.pending = Some(frame);
        self.next += 1;
        Ok(frame)
    }
    fn accept(&mut self, reply: Frame) -> Result<(), ()> {
        let valid = self.pending.is_some_and(|request| {
            !self.revoked
                && request.epoch == reply.epoch
                && request.request == reply.request
                && request.guard == reply.guard
                && reply.code
                    == if request.code == Code::Acquire {
                        Code::Acquired
                    } else {
                        Code::Healthy
                    }
        });
        if !valid {
            self.revoke();
            return Err(());
        }
        if reply.code == Code::Acquired {
            self.guards += 1;
        }
        self.pending = None;
        Ok(())
    }
}
// A receiver retains this reservation through asynchronous reaping. A stalled
// old child therefore prevents an unbounded series of new receivers/reap tasks.
struct ReceiverReservation(&'static std::sync::atomic::AtomicBool);
impl ReceiverReservation {
    fn acquire(slot: &'static std::sync::atomic::AtomicBool) -> Result<Self, ()> {
        slot.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| ())?;
        Ok(Self(slot))
    }
}
impl Drop for ReceiverReservation {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receiver_slot_remains_owned_until_cleanup_releases_it() {
        static SLOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let retained = ReceiverReservation::acquire(&SLOT).unwrap();
        assert!(ReceiverReservation::acquire(&SLOT).is_err());
        drop(retained);
        assert!(ReceiverReservation::acquire(&SLOT).is_ok());
    }
    fn acquired(request: Frame) -> Frame {
        Frame {
            code: Code::Acquired,
            ..request
        }
    }
    #[test]
    fn wire_roundtrip_and_reject_noncanonical_frames() {
        let frame = Frame {
            epoch: [2; 16],
            request: 17,
            guard: 1,
            code: Code::Acquire,
        };
        assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
        for index in [0, 5, 6, 7, 36, 37, 38, 39] {
            let mut bytes = frame.encode();
            bytes[index] ^= 1;
            assert!(Frame::decode(&bytes).is_err());
        }
        for len in [0, 39, 41, MAX_FRAME, MAX_FRAME + 1] {
            assert!(Frame::decode(&vec![0; len]).is_err());
        }
    }
    #[test]
    fn fifth_guard_and_concurrent_request_revoke_epoch() {
        let mut state = Admission::new([1; 16]);
        for _ in 0..MAX_GUARDS {
            let req = state.request(Code::Acquire).unwrap();
            state.accept(acquired(req)).unwrap();
        }
        assert!(state.request(Code::Acquire).is_err());
        assert!(state.request(Code::Check).is_err());
        let mut state = Admission::new([2; 16]);
        state.request(Code::Check).unwrap();
        assert!(state.request(Code::Check).is_err());
    }
    #[test]
    fn stale_epoch_wrong_guard_and_wrong_sequence_are_permanent() {
        for field in 0..3 {
            let mut state = Admission::new([1; 16]);
            let req = state.request(Code::Acquire).unwrap();
            let mut reply = acquired(req);
            match field {
                0 => reply.epoch[0] = 2,
                1 => reply.guard += 1,
                _ => reply.request += 1,
            }
            assert!(state.accept(reply).is_err());
            assert!(state.accept(acquired(req)).is_err());
            assert!(state.request(Code::Check).is_err());
        }
    }
    #[test]
    fn queued_success_cannot_undo_break_or_cancel() {
        let mut state = Admission::new([1; 16]);
        let req = state.request(Code::Acquire).unwrap();
        state.revoke();
        assert!(state.accept(acquired(req)).is_err());
        assert!(state.request(Code::Check).is_err());
    }
    #[test]
    fn sequence_exhaustion_refuses_instead_of_wrapping() {
        let mut state = Admission::new([1; 16]);
        state.next = u64::MAX;
        assert!(state.request(Code::Check).is_err());
    }
}
