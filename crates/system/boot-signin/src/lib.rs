//! The sign-in screen the initramfs shows when the system volume carries
//! `tilo-signin` tokens (docs/security/sign-in.md §3.2): the person picks
//! themselves, types their PIN or password, and `tilo-auth boot-unlock` opens
//! the volume through their keyslot and leaves the one-time handoff for the
//! session. "Can't sign in" ends it, and `tilo-root-discover` continues with
//! the prompts every encrypted install has.

pub mod auth;
pub mod flow;
pub mod people;
pub mod relay;
pub mod screen;
pub mod unlock_prompt;

#[cfg(test)]
mod test_support {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// Tests that write a stand-in program and run it take turns: a program
    /// written while another test's child is between fork and exec is still
    /// open for writing in that child, and running it fails (ETXTBSY).
    pub fn serial() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
