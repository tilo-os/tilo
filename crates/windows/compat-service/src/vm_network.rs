//! The filtered egress of a Windows appliance presentation a desktop adopted.
//!
//! A presentation's guest was given an endpoint and a token at launch, and its launch keeps them
//! in its keeper's directory ([`windows_vm::GuestNetworkBinding::persist`]). The listener those
//! serve lived in the desktop that started the presentation, so it went with that desktop when
//! the desktop restarted in place. This serves them again, on the same endpoint with the same
//! token, for as long as the launch lives: the guest reconnects and reaches what the workspace
//! grants it now.
//!
//! Nothing here fails open. A launch without the file, a file that does not read, and a grant
//! that no longer allows anything all leave the guest with no listener: its proxy then has
//! nowhere to connect, rather than an unfiltered route. A grant taken back drops the listener and
//! the tunnels it carried.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use windows_vm::GuestNetworkBinding;

/// What each launch's listener was last served with.
struct Held {
    binding: GuestNetworkBinding,
    allowed: Vec<String>,
    _proxy: windows_vm::GuestNetworkProxy,
}

/// The listeners this desktop serves for the Windows appliance presentations it adopted, by the
/// keeper directory of each launch.
#[derive(Default)]
pub struct VmNetworkLeases {
    held: Mutex<BTreeMap<PathBuf, Held>>,
}

impl VmNetworkLeases {
    /// Serve `wanted` — each adopted launch's keeper directory and the names its workspace grants
    /// it now — and nothing else: a launch that is gone, or whose grant was taken back, has no
    /// listener.
    pub fn adopt(&self, wanted: Vec<(PathBuf, Vec<String>)>) {
        self.adopt_bound(wanted, true);
    }

    /// [`Self::adopt`], with whether only the guest may connect: a test listens on loopback,
    /// where the guest's address never is.
    fn adopt_bound(&self, wanted: Vec<(PathBuf, Vec<String>)>, require_guest_peer: bool) {
        let Ok(mut held) = self.held.lock() else {
            return;
        };
        held.retain(|dir, _| wanted.iter().any(|(at, _)| at == dir));
        for (dir, allowed) in wanted {
            if allowed.is_empty() {
                // Taken back: dropping the listener closes it and every tunnel it carried.
                held.remove(&dir);
                continue;
            }
            let Ok(binding) = GuestNetworkBinding::read(&dir) else {
                held.remove(&dir);
                continue;
            };
            if held.get(&dir).is_some_and(|held| {
                held.binding == binding
                    && held.allowed.len() == allowed.len()
                    && held.allowed.iter().all(|name| allowed.contains(name))
            }) {
                continue;
            }
            // A policy that changed cuts what the old one allowed: the listener is served again
            // under the same endpoint and token, with the grants as they are now.
            held.remove(&dir);
            match windows_vm::GuestNetworkProxy::start_at_with_token(
                binding.endpoint,
                &allowed,
                require_guest_peer,
                binding.token.clone(),
            ) {
                Ok(proxy) => {
                    held.insert(
                        dir,
                        Held {
                            binding,
                            allowed,
                            _proxy: proxy,
                        },
                    );
                }
                Err(error) => tracing::warn!(
                    dir = %dir.display(),
                    %error,
                    "a kept Windows appliance program's network stays offline"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    use super::*;

    /// The endpoint and token a launch was given, kept in a directory of its own, with the
    /// listener that served it gone: what a desktop finds after it restarts in place.
    fn kept_launch(allowed: &[&str]) -> (tempfile::TempDir, GuestNetworkBinding) {
        let allowed: Vec<String> = allowed.iter().map(|name| (*name).to_owned()).collect();
        let proxy = windows_vm::GuestNetworkProxy::start_at(
            "127.0.0.1:0".parse().unwrap(),
            &allowed,
            false,
        )
        .expect("the launch's listener");
        let binding = proxy.binding().clone();
        drop(proxy);
        // The desktop that held it is gone: its listener goes too before the next one binds it.
        tilo_test_env::wait_until("the old desktop's listener is closed", || {
            TcpStream::connect(binding.endpoint).is_err()
        });
        let directory = tempfile::tempdir().expect("a keeper's directory");
        binding
            .persist(directory.path())
            .expect("the binding is kept");
        (directory, binding)
    }

    fn connect(binding: &GuestNetworkBinding, port: u16) -> TcpStream {
        let mut stream = TcpStream::connect(binding.endpoint).expect("the listener is up again");
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("a bound");
        write!(
            stream,
            "CONNECT localhost:{port} HTTP/1.1\r\nProxy-Authorization: Bearer {}\r\n\r\n",
            binding.token
        )
        .expect("the guest's request");
        stream
    }

    fn response(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("an answer");
            head.push(byte[0]);
        }
        String::from_utf8(head).expect("an HTTP answer")
    }

    #[test]
    fn an_adopted_launchs_egress_is_served_again_until_its_grant_is_taken_back() {
        let service = TcpListener::bind("127.0.0.1:0").expect("a local service");
        let port = service.local_addr().expect("its address").port();
        let (directory, binding) = kept_launch(&["localhost"]);
        let dir = directory.path().to_path_buf();
        let leases = VmNetworkLeases::default();
        leases.adopt_bound(vec![(dir, vec!["localhost".into()])], false);

        let mut stream = connect(&binding, port);
        assert!(
            response(&mut stream).starts_with("HTTP/1.1 200"),
            "the guest reaches its granted name through the new desktop"
        );
        let (mut upstream, _) = service.accept().expect("the tunnel reaches it");
        stream.write_all(b"ping").expect("through the tunnel");
        let mut echoed = [0_u8; 4];
        upstream.read_exact(&mut echoed).expect("both ways");
        assert_eq!(&echoed, b"ping");

        // Taken back: the listener and the tunnel it carried go, and the guest has no route.
        leases.adopt_bound(Vec::new(), false);
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                ) => {}
            result => panic!("the tunnel survived the grant taken back: {result:?}"),
        }
        tilo_test_env::wait_until("its listener is closed", || {
            TcpStream::connect(binding.endpoint).is_err()
        });
    }

    #[test]
    fn a_launch_whose_file_does_not_read_gets_no_network() {
        let directory = tempfile::tempdir().expect("a keeper's directory");
        std::fs::write(
            directory
                .path()
                .join(launcher::egress_proxy::APPLIANCE_NETWORK),
            b"not a binding",
        )
        .expect("a broken file");
        let leases = VmNetworkLeases::default();
        leases.adopt_bound(
            vec![(directory.path().to_path_buf(), vec!["example.test".into()])],
            false,
        );

        // A file whose token the guest could never have presented refuses the lease rather than
        // serving it to anyone.
        let address = TcpListener::bind("127.0.0.1:0")
            .expect("an address")
            .local_addr()
            .expect("its address");
        let binding = GuestNetworkBinding {
            endpoint: address,
            token: "short".into(),
        };
        binding.persist(directory.path()).expect("the file");
        leases.adopt_bound(
            vec![(directory.path().to_path_buf(), vec!["example.test".into()])],
            false,
        );
        assert!(
            TcpStream::connect(address).is_err(),
            "a lease whose token is not the guest's is not served"
        );
    }
}
