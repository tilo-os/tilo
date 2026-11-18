//! Which ways a machine may enroll, declared once per image.
//!
//! Every Tilo device enrolls the same way: a machine arrives with a token the
//! person made for it, shows a short code a device of theirs approves, or is
//! paired over the local network by a device of theirs that finds it. What
//! differs between images is only which of those they offer, and that is
//! declared in one file in the image ([`DECLARATION`]), one method per line:
//!
//! ```text
//! # Tilo Server
//! token
//! code
//! ```
//!
//! A machine without the file offers every method, as a development machine
//! does. The enrolling program reads it once and refuses a method the image
//! does not offer, with the methods it does offer named.
//!
//! `token` and `code` are the machine's own ways in: it is handed a token, or
//! it waits showing a code. `local-network` is the other side: one of the
//! person's devices finds the machine on the network and approves it. A
//! machine that offers neither of its own ways in cannot enroll at all.

use std::fmt;
use std::io;
use std::path::Path;

/// The file an image declares its enrollment methods in, beside its
/// `/usr/lib/tilo/image-flavour`.
pub const DECLARATION: &str = "/usr/lib/tilo/enrollment-methods";

/// One way a machine enrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// A single-use, expiring token the person made for this machine.
    Token,
    /// A short code the machine shows, approved on another device of the
    /// person's.
    Code,
    /// One of the person's devices finds the machine on the local network
    /// and pairs it.
    LocalNetwork,
}

impl Method {
    pub const ALL: [Self; 3] = [Self::Token, Self::Code, Self::LocalNetwork];

    /// The name the declaration and the command line use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Code => "code",
            Self::LocalNetwork => "local-network",
        }
    }

    /// The method a declaration line or a command-line value names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.name() == name)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The methods one image offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Methods {
    token: bool,
    code: bool,
    local_network: bool,
}

impl Methods {
    /// Every method: what a machine with no declaration offers.
    pub const ALL: Self = Self {
        token: true,
        code: true,
        local_network: true,
    };
    /// What a machine that declares nothing offers.
    pub const NONE: Self = Self {
        token: false,
        code: false,
        local_network: false,
    };

    #[must_use]
    pub const fn offers(self, method: Method) -> bool {
        match method {
            Method::Token => self.token,
            Method::Code => self.code,
            Method::LocalNetwork => self.local_network,
        }
    }

    /// The names this image offers, for a refusal to read.
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        Method::ALL
            .into_iter()
            .filter(|method| self.offers(*method))
            .map(Method::name)
            .collect()
    }

    /// Read a declaration: one method name per line, `#` comments and blank
    /// lines ignored. An unknown word is refused rather than skipped, so a
    /// misspelt method cannot silently widen what an image offers.
    ///
    /// # Errors
    ///
    /// A line that names no method.
    pub fn parse(declaration: &str) -> Result<Self, UnknownMethod> {
        let mut methods = Self::NONE;
        for line in declaration.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let method = Method::parse(line).ok_or_else(|| UnknownMethod {
                name: line.to_owned(),
            })?;
            methods = match method {
                Method::Token => Self {
                    token: true,
                    ..methods
                },
                Method::Code => Self {
                    code: true,
                    ..methods
                },
                Method::LocalNetwork => Self {
                    local_network: true,
                    ..methods
                },
            };
        }
        Ok(methods)
    }

    /// The methods the image at `path` declares. A machine without a
    /// declaration offers every method.
    ///
    /// # Errors
    ///
    /// The declaration cannot be read, or names a method no build has.
    pub fn of_image(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(declaration) => Self::parse(&declaration)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::ALL),
            Err(error) => Err(error),
        }
    }

    /// The method to enroll with: the one asked for, or without one a token
    /// when this machine was handed one and a code otherwise.
    ///
    /// # Errors
    ///
    /// The image does not offer the method that would be used.
    pub fn choose(self, requested: Option<Method>, token_given: bool) -> Result<Method, Refused> {
        let method = requested.unwrap_or(if token_given {
            Method::Token
        } else {
            Method::Code
        });
        if self.offers(method) {
            Ok(method)
        } else {
            Err(Refused {
                method,
                offers: self,
            })
        }
    }

    /// `Ok` when this image offers `method`.
    ///
    /// # Errors
    ///
    /// The image does not offer it.
    pub fn ensure(self, method: Method) -> Result<(), Refused> {
        if self.offers(method) {
            Ok(())
        } else {
            Err(Refused {
                method,
                offers: self,
            })
        }
    }
}

/// A declaration named something no build enrolls with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownMethod {
    pub name: String,
}

impl fmt::Display for UnknownMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {:?} is not an enrollment method ({} )",
            DECLARATION,
            self.name,
            Method::ALL
                .into_iter()
                .map(Method::name)
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

impl std::error::Error for UnknownMethod {}

/// This image does not offer the method enrollment would use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub method: Method,
    pub offers: Methods,
}

impl fmt::Display for Refused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let offers = self.offers.names();
        write!(
            formatter,
            "this image does not offer {} enrollment",
            self.method
        )?;
        if offers.is_empty() {
            formatter.write_str("; it offers none")
        } else {
            write!(formatter, "; it offers {}", offers.join(", "))
        }
    }
}

impl std::error::Error for Refused {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_names_the_methods_an_image_offers() {
        let server = Methods::parse("# Tilo Server\ntoken\ncode\n").expect("declaration");
        assert_eq!(server.names(), ["token", "code"]);
        assert!(server.offers(Method::Token));
        assert!(!server.offers(Method::LocalNetwork));

        let desktop = Methods::parse("token\ncode\nlocal-network\n").expect("declaration");
        assert_eq!(desktop, Methods::ALL);
        assert_eq!(Methods::parse("").expect("empty"), Methods::NONE);
        assert_eq!(
            Methods::parse("code # only a code").expect("inline comment"),
            Methods {
                code: true,
                ..Methods::NONE
            }
        );
    }

    #[test]
    fn a_misspelt_method_is_refused_rather_than_skipped() {
        let error = Methods::parse("token\nlocal_network\n").expect_err("no such method");
        assert_eq!(error.name, "local_network");
        assert!(error.to_string().contains("local-network"), "{error}");
    }

    #[test]
    fn choosing_prefers_the_given_token_then_a_code() {
        assert_eq!(
            Methods::ALL.choose(None, true).expect("token"),
            Method::Token
        );
        assert_eq!(
            Methods::ALL.choose(None, false).expect("code"),
            Method::Code
        );
        assert_eq!(
            Methods::ALL.choose(Some(Method::LocalNetwork), true),
            Ok(Method::LocalNetwork)
        );
    }

    #[test]
    fn a_method_the_image_does_not_offer_is_refused_with_what_it_does() {
        let server = Methods::parse("token\ncode\n").expect("declaration");
        assert_eq!(server.ensure(Method::Token), Ok(()));
        let refusal = server
            .ensure(Method::LocalNetwork)
            .expect_err("the server image pairs no local network");
        assert_eq!(refusal.method, Method::LocalNetwork);
        let said = refusal.to_string();
        assert!(said.contains("does not offer local-network"), "{said}");
        assert!(said.contains("token, code"), "{said}");
        let refused = Methods::parse("token\n")
            .expect("declaration")
            .choose(None, false)
            .expect_err("a machine with no token needs a code");
        assert_eq!(refused.method, Method::Code);
    }

    #[test]
    fn a_machine_without_a_declaration_offers_every_method() {
        let missing = Path::new("/nonexistent/tilo/enrollment-methods");
        assert_eq!(Methods::of_image(missing).expect("absent"), Methods::ALL);
    }
}
