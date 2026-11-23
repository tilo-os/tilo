//! A service's address, in the one spelling a connection keeps it in.

use tilo_net::Url;

use super::wire::{Destinations, check_destination, is_non_public_literal};

/// The longest address taken.
const MAX_ADDRESS_BYTES: usize = 2_048;

/// Why an address is not one a connection is kept for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotAnAddress {
    /// It does not read as an address at all.
    Unreadable,
    /// It is not an `https` address.
    NotHttps,
    /// It carries a name and a password, or a query: what is typed there is
    /// kept, shown and sent as part of the address, so a key has no place
    /// in it.
    CarriesSecret,
    /// It is an address this server does not call.
    NotCalled,
}

impl NotAnAddress {
    /// The system's words for it.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Unreadable => "A service's address reads like https://service.example/path",
            Self::NotHttps => "A service's address begins with https://",
            Self::CarriesSecret => {
                "A service's address carries no name, password or query: sign in to the service instead"
            }
            Self::NotCalled => "That address is not called from here",
        }
    }
}

/// The address of a service a person named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Address {
    url: Url,
}

impl Address {
    /// The address `input` names, when it is one this server keeps a
    /// connection for under `destinations`: `https`, with a site's name,
    /// and nothing in it but that name, a port and a path. Where the
    /// server's operator lets it call its own network, an address there may
    /// be plain `http`, and only there.
    pub(crate) fn parse(input: &str, destinations: Destinations) -> Result<Self, NotAnAddress> {
        let text = input.trim();
        if text.is_empty()
            || text.len() > MAX_ADDRESS_BYTES
            || text
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(NotAnAddress::Unreadable);
        }
        let mut url = Url::parse(text).map_err(|_| NotAnAddress::Unreadable)?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(NotAnAddress::NotHttps);
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
            return Err(NotAnAddress::CarriesSecret);
        }
        url.set_fragment(None);
        if url.scheme() == "http"
            && !(destinations == Destinations::Any && is_non_public_literal(&url))
        {
            return Err(NotAnAddress::NotHttps);
        }
        check_destination(&url, destinations).map_err(|_| NotAnAddress::NotCalled)?;
        Ok(Self { url })
    }

    /// The address as kept and shown.
    pub(crate) fn as_str(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) const fn url(&self) -> &Url {
        &self.url
    }
}

/// The name or the literal address `url` is reached by.
pub(crate) fn reached_by(url: &Url) -> Option<&str> {
    url.host_str()
}

/// The site `url` names, with its port when it names one: what the system
/// calls a service or its issuer by, whatever they call themselves.
pub(crate) fn site_of(url: &Url) -> String {
    match (reached_by(url), url.port()) {
        (Some(site), Some(port)) => format!("{site}:{port}"),
        (Some(site), None) => site.to_owned(),
        (None, _) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address is kept in one spelling, and what could carry a key, or
    /// reach where a server does not call, is no address.
    #[test]
    fn an_address_is_https_with_nothing_in_it_but_a_site_a_port_and_a_path() {
        let kept =
            Address::parse("  HTTPS://Orchard.Example/mcp#top ", Destinations::Public).unwrap();
        assert_eq!(kept.as_str(), "https://orchard.example/mcp");
        assert_eq!(site_of(kept.url()), "orchard.example");
        let bare = Address::parse("https://orchard.example:8443", Destinations::Public).unwrap();
        assert_eq!(bare.as_str(), "https://orchard.example:8443/");
        assert_eq!(site_of(bare.url()), "orchard.example:8443");

        for (address, why) in [
            ("", NotAnAddress::Unreadable),
            ("orchard.example/mcp", NotAnAddress::Unreadable),
            ("https://orchard.example/a b", NotAnAddress::Unreadable),
            ("https://orchard.example/\u{0}", NotAnAddress::Unreadable),
            ("ftp://orchard.example/mcp", NotAnAddress::NotHttps),
            ("http://orchard.example/mcp", NotAnAddress::NotHttps),
            (
                "https://ada:secret@orchard.example/mcp",
                NotAnAddress::CarriesSecret,
            ),
            (
                "https://ada@orchard.example/mcp",
                NotAnAddress::CarriesSecret,
            ),
            (
                "https://orchard.example/mcp?key=secret",
                NotAnAddress::CarriesSecret,
            ),
            ("https://orchard.example/mcp?", NotAnAddress::CarriesSecret),
            ("https://127.0.0.1/mcp", NotAnAddress::NotCalled),
            ("https://10.0.0.7:8443/mcp", NotAnAddress::NotCalled),
            ("https://[::1]/mcp", NotAnAddress::NotCalled),
            ("https://169.254.169.254/latest", NotAnAddress::NotCalled),
            ("https://[::ffff:10.0.0.7]/mcp", NotAnAddress::NotCalled),
            ("http://127.0.0.1:8000/mcp", NotAnAddress::NotHttps),
        ] {
            assert_eq!(
                Address::parse(address, Destinations::Public),
                Err(why),
                "{address:?}"
            );
        }
        let long = format!("https://orchard.example/{}", "a".repeat(MAX_ADDRESS_BYTES));
        assert_eq!(
            Address::parse(&long, Destinations::Public),
            Err(NotAnAddress::Unreadable)
        );
    }

    /// Where the operator lets the server call its own network, an address
    /// there may be plain; a name never is, and an outside address never is.
    #[test]
    fn only_the_servers_own_network_is_called_without_tls_and_only_when_let() {
        let inside = Address::parse("http://127.0.0.1:8000/mcp", Destinations::Any).unwrap();
        assert_eq!(inside.as_str(), "http://127.0.0.1:8000/mcp");
        for ok in [
            "https://10.0.0.7/mcp",
            "http://[::1]:8000/mcp",
            "http://[::ffff:127.0.0.1]:8000/mcp",
            "http://127.0.0.1./mcp",
            "http://2130706433:8000/mcp",
            "http://0x7f.1:8000/mcp",
        ] {
            assert!(Address::parse(ok, Destinations::Any).is_ok(), "{ok}");
        }
        for address in [
            "http://orchard.example/mcp",
            "http://localhost:8000/mcp",
            "http://93.184.216.34/mcp",
            "http://1.1.1.1./mcp",
            "http://[::ffff:1.1.1.1]/mcp",
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert_eq!(
                Address::parse(address, Destinations::Any),
                Err(NotAnAddress::NotHttps),
                "{address}"
            );
            assert_eq!(
                Address::parse(address, Destinations::Public),
                Err(NotAnAddress::NotHttps),
                "{address} (public)"
            );
        }
    }
}
