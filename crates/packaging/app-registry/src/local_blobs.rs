//! Payload blobs from a registry directory on this machine.
//!
//! A directory registry keeps the payload blobs of the packages it reviewed
//! under `payload-blobs/`, each named by its BLAKE3 (see
//! [`registry_client::local`]). A blob may be one object or a chunk list
//! (`.chunks`) whose chunks are objects beside it. The directory is not
//! trusted: every object is checked against its name, and a chunked blob
//! reaches the sink only chunk by verified chunk.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tilo_app_package::chunks::{
    CHUNKS_SUFFIX, ChunkList, ChunkRef, ChunkedReader, MAX_CHUNK_LIST_BYTES, MAX_OBJECT_BYTES,
};
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink, HashingReader, OpenBlob};

/// A registry directory serving payload blobs at `<root>/payload-blobs/`.
#[derive(Clone, Debug)]
pub struct LocalBlobs {
    root: PathBuf,
}

impl LocalBlobs {
    /// Blobs in `root`'s registry layout.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root.join("payload-blobs")
    }

    /// The object holding `name`'s bytes: the plain name a registry server
    /// writes, or the `.blob` name `tilo-app-package split` writes.
    fn open_object(&self, name: &str) -> io::Result<Option<std::fs::File>> {
        let plain = self.blobs_dir().join(name);
        let suffixed = self.blobs_dir().join(format!("{name}.blob"));
        for file in [plain, suffixed] {
            match std::fs::File::open(file) {
                Ok(file) => return Ok(Some(file)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    fn open_blob(&self, hash: &BlobHash) -> io::Result<Option<OpenBlob<'_>>> {
        let digest = hash.to_hex();
        if let Some(file) = self.open_object(&digest)? {
            let len = file.metadata()?.len();
            if len > MAX_OBJECT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("payload blob {hash} exceeds the object ceiling"),
                ));
            }
            return Ok(Some(OpenBlob {
                len,
                reader: Box::new(io::BufReader::new(file)),
            }));
        }
        let list_name = format!("{digest}{CHUNKS_SUFFIX}");
        let Some(file) = self.open_object(&list_name)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(MAX_CHUNK_LIST_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let list = ChunkList::decode(&bytes, hash)?;
        let len = list.len;
        let blob = *hash;
        let reader = ChunkedReader::new(list, move |chunk: &ChunkRef| {
            let file = self.open_object(&chunk.hash.to_hex())?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("chunk {} of {blob} is missing", chunk.hash),
                )
            })?;
            let mut bytes = Vec::new();
            file.take(chunk.len).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        Ok(Some(OpenBlob {
            len,
            reader: Box::new(reader),
        }))
    }
}

impl BlobFetcher for LocalBlobs {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> io::Result<()> {
        for hash in hashes {
            let Some(blob) = self.open_blob(hash)? else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "registry directory {} has no payload blob {hash}",
                        self.root.display()
                    ),
                ));
            };
            let mut reader = HashingReader::new(blob.reader);
            sink.accept(hash, blob.len, &mut reader)?;
            if reader.finish() != (*hash, blob.len) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("payload object {hash} does not match its name"),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn local_objects_are_read_whole_or_as_verified_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("payload-blobs")).unwrap();

        let whole = b"one payload object";
        let whole_hash = BlobHash::of(whole);
        std::fs::write(root.join("payload-blobs").join(whole_hash.to_hex()), whole).unwrap();

        let chunked = b"0123456789abcdefghijklmn";
        let chunked_hash = BlobHash::of(chunked);
        let (first, second) = chunked.split_at(12);
        assert_eq!(first.len(), second.len());
        let first_hash = BlobHash::of(first);
        let second_hash = BlobHash::of(second);
        std::fs::write(root.join("payload-blobs").join(first_hash.to_hex()), first).unwrap();
        std::fs::write(
            root.join("payload-blobs").join(second_hash.to_hex()),
            second,
        )
        .unwrap();
        let list = ChunkList::new(
            chunked_hash,
            chunked.len() as u64,
            first.len() as u64,
            vec![
                ChunkRef {
                    hash: first_hash,
                    len: first.len() as u64,
                },
                ChunkRef {
                    hash: second_hash,
                    len: second.len() as u64,
                },
            ],
        );
        std::fs::write(
            root.join("payload-blobs")
                .join(format!("{chunked_hash}{CHUNKS_SUFFIX}")),
            list.encode(),
        )
        .unwrap();

        let blobs = LocalBlobs::new(root);
        let mut got = Collect::default();
        blobs.fetch(&[whole_hash, chunked_hash], &mut got).unwrap();
        assert_eq!(got.0.get(&whole_hash).unwrap(), whole);
        assert_eq!(got.0.get(&chunked_hash).unwrap(), chunked);

        assert!(
            blobs
                .fetch(&[BlobHash::of(b"absent")], &mut Discard)
                .is_err(),
            "a missing payload blob fails rather than installing a partial package"
        );

        std::fs::write(
            root.join("payload-blobs").join(whole_hash.to_hex()),
            b"tampered",
        )
        .unwrap();
        let mut got = Collect::default();
        assert!(
            blobs.fetch(&[whole_hash], &mut got).is_err(),
            "bytes that do not hash to their name fail the fetch"
        );
        assert!(
            got.0.is_empty(),
            "a sink that verifies names never keeps tampered bytes"
        );
    }

    #[derive(Default)]
    struct Collect(BTreeMap<BlobHash, Vec<u8>>);

    impl BlobSink for Collect {
        fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> io::Result<()> {
            let mut body = Vec::new();
            let mut hashing = HashingReader::new(bytes);
            hashing.read_to_end(&mut body)?;
            if hashing.finish() != (*hash, len) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("payload object {hash} does not match its name"),
                ));
            }
            self.0.insert(*hash, body);
            Ok(())
        }
    }

    #[derive(Default)]
    struct Discard;

    impl BlobSink for Discard {
        fn accept(&mut self, _hash: &BlobHash, _len: u64, bytes: &mut dyn Read) -> io::Result<()> {
            io::copy(bytes, &mut io::sink()).map(drop)
        }
    }
}
