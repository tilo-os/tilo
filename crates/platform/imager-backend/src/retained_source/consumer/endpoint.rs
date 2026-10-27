//! Closed descriptor handoff used by the authenticated consumer bootstrap.
//! Transport receipt is not person proof or source authority.
use std::{fs::File, io};

#[cfg(target_os = "linux")]
#[path = "endpoint/native.rs"]
mod native;

/// Fixed endpoint roles in the existing private bootstrap protocol.
#[derive(Clone, Copy, Debug)]
pub enum EndpointRole {
    PersonProof,
    Consumer,
    Retirement,
}
impl EndpointRole {
    pub const fn tag(self) -> u8 {
        match self {
            Self::PersonProof => 71,
            Self::Consumer => 72,
            Self::Retirement => 73,
        }
    }
}

/// An owned, checked Unix stream received over the held bootstrap channel.
/// There is no raw-fd, path, serialized or caller-selected-tag constructor.
pub struct ConsumerEndpoint {
    file: File,
}
impl ConsumerEndpoint {
    /// Validate the descriptor before acknowledging its fixed transport role.
    /// The caller must independently authenticate the endpoint's actual peer.
    pub fn receive(channel: &File, role: EndpointRole) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            native::receive(channel, role).map(|file| Self { file })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (channel, role);
            Err(super::super::Unsupported.into())
        }
    }

    /// Transfer this exact validated channel into the person/consumer verifier.
    pub fn into_file(self) -> File {
        self.file
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_refuses_before_touching_transport() {
        let channel = tempfile::tempfile().unwrap();
        for role in [
            EndpointRole::PersonProof,
            EndpointRole::Consumer,
            EndpointRole::Retirement,
        ] {
            let error = ConsumerEndpoint::receive(&channel, role).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(
                error
                    .get_ref()
                    .unwrap()
                    .is::<super::super::super::Unsupported>()
            );
        }
        assert_eq!(channel.metadata().unwrap().len(), 0);
    }
}
