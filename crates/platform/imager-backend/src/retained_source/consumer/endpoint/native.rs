use super::EndpointRole;
use runtime_portal::fd_transport;
use std::{
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
};

pub(super) fn receive(channel: &File, role: EndpointRole) -> io::Result<File> {
    let refused = || io::Error::from(io::ErrorKind::PermissionDenied);
    let (bytes, descriptor) = fd_transport::recv_with_fd(channel.as_fd())?;
    if bytes != [role.tag()] {
        return Err(refused());
    }
    let endpoint = UnixStream::from(descriptor.ok_or_else(refused)?);
    // Reject non-stream/non-Unix descriptors before their transport ACK.
    let mut kind: libc::c_int = 0;
    let mut size =
        libc::socklen_t::try_from(std::mem::size_of_val(&kind)).map_err(io::Error::other)?;
    if unsafe {
        libc::getsockopt(
            endpoint.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut size,
        )
    } != 0
        || kind != libc::SOCK_STREAM
    {
        return Err(refused());
    }
    endpoint.peer_addr()?;
    let mut writer = channel;
    writer.write_all(&[role.tag()])?;
    Ok(File::from(std::os::fd::OwnedFd::from(endpoint)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn channel(stream: UnixStream) -> File {
        File::from(std::os::fd::OwnedFd::from(stream))
    }

    #[test]
    fn validated_endpoint_preserves_exact_stream_and_acknowledges_role() {
        for role in [
            EndpointRole::PersonProof,
            EndpointRole::Consumer,
            EndpointRole::Retirement,
        ] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            let (endpoint, mut peer) = UnixStream::pair().unwrap();
            fd_transport::send_with_fd(sender.as_fd(), &[role.tag()], endpoint.as_fd()).unwrap();
            let mut transport = receive(&channel(receiver), role).unwrap();
            let mut ack = [0];
            sender.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [role.tag()]);
            peer.write_all(b"live").unwrap();
            let mut bytes = [0; 4];
            transport.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"live");
        }
    }

    #[test]
    fn datagram_and_non_unix_stream_are_not_admitted_as_endpoints() {
        use std::net::{TcpListener, TcpStream};
        use std::os::unix::net::UnixDatagram;

        let (datagram, _peer) = UnixDatagram::pair().unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_accepted, _) = listener.accept().unwrap();
        for descriptor in [datagram.as_fd(), tcp.as_fd()] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            fd_transport::send_with_fd(sender.as_fd(), &[71], descriptor).unwrap();
            let receiver = channel(receiver);
            assert!(receive(&receiver, EndpointRole::PersonProof).is_err());
            sender.set_nonblocking(true).unwrap();
            assert_eq!(
                sender.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn wrong_role_missing_descriptor_and_non_socket_never_receive_ack() {
        for fixture in 0..3 {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            let (endpoint, _peer) = UnixStream::pair().unwrap();
            let file = tempfile::tempfile().unwrap();
            match fixture {
                0 => fd_transport::send_with_fd(sender.as_fd(), &[72], endpoint.as_fd()).unwrap(),
                1 => sender.write_all(&[71]).unwrap(),
                _ => fd_transport::send_with_fd(sender.as_fd(), &[71], file.as_fd()).unwrap(),
            }
            let receiver = channel(receiver);
            assert!(receive(&receiver, EndpointRole::PersonProof).is_err());
            sender.set_nonblocking(true).unwrap();
            assert_eq!(
                sender.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }
}
