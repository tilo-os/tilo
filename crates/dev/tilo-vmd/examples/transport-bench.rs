//! Same-artifact first-transfer benchmark; isolated sockets, roots and record apply.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed benchmark setup or protocol operation must fail visibly"
)]
use sha2::{Digest as _, Sha256};
use std::fs::File;
use std::io::{BufReader, Read as _, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;
use tilo_vm_protocol::{Client, LayerImport, LayerSource, Provenance, binary};

struct Count<W> {
    inner: W,
    bytes: Arc<AtomicU64>,
}
impl<W: Write> Write for Count<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let sent = self.inner.write(bytes)?;
        self.bytes.fetch_add(sent as u64, Ordering::Relaxed);
        Ok(sent)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
fn run(archive: &Path, builder_binary: bool, guest_binary: bool) -> serde_json::Value {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    let (client_socket, server_socket) = UnixStream::pair().unwrap();
    let server = std::thread::spawn(move || {
        let mut daemon = tilo_vmd::Daemon::new(
            &path,
            Box::new(tilo_vmd::apply::RecordingApply::default()),
            None,
            tilo_vm_protocol::BaseInfo::default(),
        );
        tilo_vmd::server::serve(
            &mut daemon,
            BufReader::new(server_socket.try_clone().unwrap()),
            server_socket,
        )
        .unwrap();
    });
    let guest_bytes = Arc::new(AtomicU64::new(0));
    let mut client = Client::new(
        BufReader::new(client_socket.try_clone().unwrap()),
        Count {
            inner: client_socket,
            bytes: guest_bytes.clone(),
        },
    );
    client.enable_binary_uploads();
    client.hello("isolated-transport-benchmark").unwrap();
    let builder_bytes = Arc::new(AtomicU64::new(0));
    let (builder_read, builder_write) = UnixStream::pair().unwrap();
    let artifact = archive.to_owned();
    let count = builder_bytes.clone();
    let timer = Instant::now();
    let builder = std::thread::spawn(move || {
        let mut output = Count {
            inner: builder_write,
            bytes: count,
        };
        let packed = tilo_vm::builderlayer::Packed {
            component: "shell".into(),
            binary: builder_binary,
            ..Default::default()
        };
        writeln!(
            output,
            "{}{}",
            tilo_vm::builderlayer::BEGIN,
            serde_json::to_string(&packed).unwrap()
        )
        .unwrap();
        let mut input = File::open(artifact).unwrap();
        if builder_binary {
            let mut frames = binary::Encoder::new(output);
            std::io::copy(&mut input, &mut frames).unwrap();
            frames.finish().unwrap();
        } else {
            let mut frames = tilo_vm::builderlayer::Lines::new(&mut output);
            std::io::copy(&mut input, &mut frames).unwrap();
            let (sha, bytes) = frames.finish().unwrap();
            writeln!(output, "{}{sha} {bytes}", tilo_vm::builderlayer::END).unwrap();
        }
    });
    let initial_guest_bytes = guest_bytes.load(Ordering::Relaxed);
    let mut stream = tilo_vm::builderlayer::Stream::new(BufReader::new(builder_read));
    let packed = stream.header(&mut std::io::sink()).unwrap();
    assert_eq!(packed.binary, builder_binary);
    let upload = if guest_binary {
        client.upload(stream)
    } else {
        client.upload_json_lines(stream)
    }
    .unwrap();
    builder.join().unwrap();
    let transfer_seconds = timer.elapsed().as_secs_f64();
    let guest_wire_bytes = guest_bytes
        .load(Ordering::Relaxed)
        .saturating_sub(initial_guest_bytes);
    let import_timer = Instant::now();
    let imported = client
        .call(LayerImport {
            source: LayerSource::Upload(upload),
            provenance: Box::new(Provenance::local("shell")),
        })
        .unwrap();
    let import_seconds = import_timer.elapsed().as_secs_f64();
    drop(client);
    server.join().unwrap();
    serde_json::json!({"mode":if builder_binary {"binary-both-legs"} else if guest_binary {"legacy-builder-binary-upload"} else {"legacy-double-base64"},"transfer_seconds":transfer_seconds,"import_seconds":import_seconds,"builder_wire_bytes":builder_bytes.load(Ordering::Relaxed),"guest_wire_bytes":guest_wire_bytes,"canonical_layer_hash":imported.hash})
}
fn main() {
    let artifact = std::env::args_os()
        .nth(1)
        .expect("supply a representative artifact path; absence is a failure");
    let mut source = File::open(&artifact).expect("representative artifact must exist");
    let mut sha = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let size = source.read(&mut buffer).unwrap();
        if size == 0 {
            break;
        }
        sha.update(buffer.get(..size).unwrap());
    }
    let supplied_archive = std::env::args()
        .nth(2)
        .is_some_and(|arg| arg == "--archive");
    let archive = tempfile::NamedTempFile::new().unwrap();
    if !supplied_archive {
        let mut tar = tar::Builder::new(archive.reopen().unwrap());
        tar.append_path_with_name(&artifact, "usr/bin/shell")
            .unwrap();
        tar.finish().unwrap();
    }
    let archive_path = if supplied_archive {
        Path::new(&artifact)
    } else {
        archive.path()
    };
    let size = std::fs::metadata(archive_path).unwrap().len();
    assert!(size > 0 && size <= tilo_vm_protocol::binary::MAX_BYTES);
    let old = run(archive_path, false, false);
    let new = run(archive_path, false, true);
    let binary_both = run(archive_path, true, true);
    assert_eq!(
        old.get("canonical_layer_hash").unwrap(),
        new.get("canonical_layer_hash").unwrap()
    );
    assert_eq!(
        new.get("canonical_layer_hash"),
        binary_both.get("canonical_layer_hash")
    );
    println!(
        "{}",
        serde_json::json!({"scope":"isolated Unix sockets; no live VM or SSH latency claim","artifact_sha256":hex::encode(sha.finalize()),"artifact_kind":if supplied_archive { "archive" } else { "executable" },"archive_bytes":size,"old":old,"new":new,"binary_both":binary_both})
    );
}
