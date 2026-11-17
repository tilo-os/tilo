//! First use away from Linux. What a Tilo installation carries over on first use
//! (a setup prepared on another computer, a portable home, its applications and
//! browsers, the files of the system it replaced) only exists where Tilo is the
//! operating system. On other systems the onboarding view's state for it is
//! always empty. These types have no values, so code holding an
//! `Option` of one compiles everywhere and sees `None` here.

/// No value of this type exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Never {}

/// The parts a carried-over setup brings, each a transfer the person may still
/// have to finish: a portable home, its applications or its browsers.
macro_rules! carried_part {
    ($declaration:ty) => {
        pub struct Pending(super::Never);

        #[allow(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            clippy::needless_pass_by_value,
            reason = "the same calls as the Linux part, which no value here can reach"
        )]
        impl Pending {
            pub fn open(
                _home: std::path::PathBuf,
                _identity: std::path::PathBuf,
                declaration: $declaration,
            ) -> std::io::Result<Self> {
                match declaration.0 {}
            }
            pub fn receive_prepared_password(
                &mut self,
                _digest: [u8; 32],
                _password: zeroize::Zeroizing<Vec<u8>>,
            ) -> std::io::Result<()> {
                match self.0 {}
            }
            pub fn continue_automatic(&mut self) -> bool {
                match self.0 {}
            }
            pub fn deferred(&self) -> bool {
                match self.0 {}
            }
            pub fn operation(&self) -> &str {
                match self.0 {}
            }
            pub fn pending(&self) -> bool {
                match self.0 {}
            }
            pub fn busy(&self) -> bool {
                match self.0 {}
            }
            pub fn needs_password(&self) -> bool {
                match self.0 {}
            }
            pub fn status(&self) -> serde_json::Value {
                match self.0 {}
            }
            pub fn next_action(&self) -> &'static str {
                match self.0 {}
            }
            pub fn invoke(&mut self, _op: &str, _password: &[u8]) -> std::io::Result<()> {
                match self.0 {}
            }
            pub fn poll(&mut self) -> bool {
                match self.0 {}
            }
            pub fn verified_projection(&self) -> Option<&std::path::Path> {
                match self.0 {}
            }
        }
    };
}

pub mod portable_home {
    carried_part!(super::prepared::PortableHome);
}

pub mod portable_applications {
    carried_part!(super::prepared::PortableApplications);
}

pub mod browser {
    carried_part!(super::prepared::BrowserDeclaration);
}

/// A setup prepared on another computer for this one.
pub mod prepared {
    pub struct PortableHome(pub(super) super::Never);
    pub struct PortableApplications(pub(super) super::Never);
    pub struct BrowserDeclaration(pub(super) super::Never);
}

/// Bringing over the files of the system Tilo replaced.
pub mod migration {
    pub struct Migration(super::Never);
    pub struct OriginalCopyRequest(super::Never);

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the same calls as the Linux migration, which no value here can reach"
    )]
    impl Migration {
        pub fn pending(&self) -> bool {
            match self.0 {}
        }
        pub fn status(&self) -> serde_json::Value {
            match self.0 {}
        }
        pub fn busy(&self) -> bool {
            match self.0 {}
        }
        pub fn next_action(&self) -> &'static str {
            match self.0 {}
        }
        pub fn invoke(&mut self, _op: &str, _selected: Option<&str>) -> std::io::Result<()> {
            match self.0 {}
        }
        pub fn take_access_request(&mut self) -> Option<super::retained::Request> {
            match self.0 {}
        }
        pub fn take_access_release(&mut self) -> Option<String> {
            match self.0 {}
        }
        pub fn access_outcome(&mut self, _operation: &str, _available: bool) {
            match self.0 {}
        }
        pub fn take_original_copy_request(&mut self) -> Option<OriginalCopyRequest> {
            match self.0 {}
        }
        pub fn poll(&mut self) -> bool {
            match self.0 {}
        }
    }
}

/// The files a person kept from the system Tilo replaced.
pub mod retained {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Request(super::Never);
}
