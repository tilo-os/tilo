//! The boot backend: the greeter as the sign-in screen of the initramfs,
//! a child of `tilo-boot-signin` (docs/security/sign-in.md §3.4).
//!
//! Beside the greetd backend, which checks the secret itself, this one only
//! shows what the supervisor sends over its socket ([`tilo_boot_signin::link`])
//! and answers with the person picked and the secret typed. The supervisor
//! keeps every decision: who may sign in, the tries left, whether the volume
//! opened. Messages arrive on a thread and reach the screen as external
//! events, as the greetd worker's updates do.

use std::io;
use std::sync::Arc;

use zeroize::Zeroizing;

pub use tilo_boot_signin::flow::Note;
pub use tilo_boot_signin::link::{FromGreeter, Link, Power, ToGreeter};
pub use tilo_boot_signin::people::{Person, SecretKind};

/// What opens an external payload that carries a supervisor's message.
const PREFIX: &str = "tilo-boot\u{1f}";

/// A supervisor's message as the external event the screen receives.
pub fn encode(message: &ToGreeter) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(message).unwrap_or_default()
    )
}

/// The supervisor's message in an external payload, or `None` for any other
/// payload.
pub fn decode(payload: &str) -> Option<ToGreeter> {
    serde_json::from_str(payload.strip_prefix(PREFIX)?).ok()
}

/// The greeter's end of the socket.
#[derive(Clone)]
pub struct BootLink(Arc<Link>);

impl BootLink {
    pub fn new(link: Link) -> Self {
        Self(Arc::new(link))
    }

    /// The supervisor's next message, waiting for it; for the start, before
    /// the screen runs.
    ///
    /// # Errors
    ///
    /// When the supervisor is gone.
    pub fn recv(&self) -> io::Result<ToGreeter> {
        self.0.recv_from_supervisor()
    }

    /// Read the supervisor's messages on a thread: each goes to `post` as an
    /// external payload ([`encode`]), and `ended` runs once it is gone.
    pub fn listen(
        &self,
        post: impl Fn(String) + Send + 'static,
        ended: impl FnOnce() + Send + 'static,
    ) {
        let link = Arc::clone(&self.0);
        let spawned = std::thread::Builder::new()
            .name("boot-link".into())
            .spawn(move || {
                while let Ok(message) = link.recv_from_supervisor() {
                    post(encode(&message));
                }
                ended();
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "the boot sign-in cannot listen to its supervisor");
        }
    }

    /// `person` (an index into the people sent) typed `secret`. It is wiped
    /// here once sent.
    ///
    /// # Errors
    ///
    /// When the supervisor is gone.
    pub fn sign_in(&self, person: usize, secret: Zeroizing<String>) -> io::Result<()> {
        self.0
            .send_to_supervisor(&FromGreeter::Secret { person, secret })
    }

    /// "Can't sign in".
    ///
    /// # Errors
    ///
    /// When the supervisor is gone.
    pub fn cant_sign_in(&self) -> io::Result<()> {
        self.0.send_to_supervisor(&FromGreeter::CantSignIn)
    }

    /// The power menu's choice, which the supervisor carries out.
    ///
    /// # Errors
    ///
    /// When the supervisor is gone.
    pub fn power(&self, power: Power) -> io::Result<()> {
        self.0.send_to_supervisor(&FromGreeter::Power(power))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supervisor_message_crosses_as_an_external_payload() {
        let message = ToGreeter::Ask {
            person: 1,
            note: Note::Wait { seconds: 120 },
            several: true,
        };
        assert_eq!(decode(&encode(&message)), Some(message));
        assert_eq!(decode("A"), None);
        assert_eq!(decode("tilo-boot\u{1f}{"), None);
    }

    #[test]
    fn the_link_reaches_the_supervisor_and_ends_when_it_goes() {
        let (supervisor, greeter) = Link::pair().unwrap();
        let link = BootLink::new(greeter);
        let (sender, received) = std::sync::mpsc::channel();
        let ended = sender.clone();
        link.listen(
            move |payload| sender.send(Some(payload)).unwrap(),
            move || ended.send(None).unwrap(),
        );
        supervisor.send_to_greeter(&ToGreeter::Checking).unwrap();
        assert_eq!(
            received.recv().unwrap().as_deref().and_then(decode),
            Some(ToGreeter::Checking)
        );
        link.sign_in(0, Zeroizing::new("2468".into())).unwrap();
        assert_eq!(
            supervisor.recv_from_greeter().unwrap(),
            FromGreeter::Secret {
                person: 0,
                secret: Zeroizing::new("2468".into())
            }
        );
        drop(supervisor);
        assert_eq!(received.recv().unwrap(), None);
    }
}
