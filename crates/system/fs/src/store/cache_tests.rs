//! The hot blob cache is bounded by bytes, and streamed imports hold and
//! cache nothing whole.

use super::*;

fn distinct(len: usize, seed: u64) -> Bytes {
    let mut bytes = vec![0x5a; len];
    for (byte, seeded) in bytes.iter_mut().zip(seed.to_le_bytes()) {
        *byte = seeded;
    }
    Bytes::from(bytes)
}

fn pieces_of(
    content: &[u8],
    piece: usize,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static {
    let pieces: Vec<std::io::Result<Bytes>> = content
        .chunks(piece)
        .map(|part| Ok(Bytes::copy_from_slice(part)))
        .collect();
    futures_util::stream::iter(pieces)
}

#[tokio::test]
async fn the_blob_cache_is_bounded_by_bytes_and_keeps_small_hot_entries() {
    let store = ContentStore::memory();
    let per_budget = BLOB_CACHE_BYTES / MAX_CACHED_BLOB as u64;
    let cold = |seed: u64| {
        let store = &store;
        async move {
            let hash = store.ingest(distinct(MAX_CACHED_BLOB, seed)).await.unwrap();
            assert!(store.local(hash).await.is_some());
        }
    };
    for seed in 0..per_budget / 2 {
        cold(seed).await;
    }
    let hot_bytes = Bytes::from_static(b"a manifest everybody reads");
    let hot = store.ingest(hot_bytes.clone()).await.unwrap();
    assert!(store.is_cached(hot), "small blobs are cached on ingest");
    for _ in 0..10 {
        assert_eq!(store.local(hot).await.unwrap(), hot_bytes);
    }

    let large = distinct(MAX_CACHED_BLOB + 1, u64::MAX);
    let large_hash = store.ingest(large.clone()).await.unwrap();
    assert!(!store.is_cached(large_hash));
    assert_eq!(store.local(large_hash).await.unwrap(), large);
    assert!(!store.is_cached(large_hash));

    let between = per_budget * 3 / 2;
    for seed in per_budget / 2..per_budget / 2 + 3 * per_budget {
        cold(seed).await;
        if seed % between == 0 {
            assert!(
                store.is_cached(hot),
                "a run of cold large reads flushed a small hot entry"
            );
            assert_eq!(store.local(hot).await.unwrap(), hot_bytes);
        }
    }
    let cached = store.cached_bytes();
    assert!(
        cached <= BLOB_CACHE_BYTES,
        "the cache holds {cached} B over its {BLOB_CACHE_BYTES} B budget"
    );
    assert!(
        cached > 0,
        "the flood must reach the cache for this bound to mean anything"
    );
    assert!(
        store.is_cached(hot),
        "a run of cold large reads flushed a small hot entry"
    );
}

#[tokio::test]
async fn streamed_ingest_stores_whole_content_and_caches_none_of_it() {
    let store = ContentStore::memory();
    let content: Vec<u8> = (0..300_000_u32)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    let (hash, len) = store
        .ingest_stream(pieces_of(&content, 64 * 1024))
        .await
        .unwrap();
    assert_eq!(hash, Hash::new(&content));
    assert_eq!(len, content.len() as u64);
    assert!(!store.is_cached(hash));
    assert_eq!(store.local(hash).await.unwrap().as_ref(), content);
}

#[tokio::test]
async fn an_abandoned_streamed_ingest_stores_nothing_and_leaves_no_staging_file() {
    let directory = tempfile::tempdir().unwrap();
    let content = vec![5_u8; 64 * 64 * 1024];
    {
        let store = ContentStore::open(directory.path()).await.unwrap();
        let pieces = pieces_of(&content, 64 * 1024).chain(futures_util::stream::iter([Err(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "abandoned"),
        )]));
        assert!(store.ingest_stream(pieces).await.is_err());
        assert!(!store.has(Hash::new(&content)).await);
        assert!(
            store.blob_hashes().await.unwrap().is_empty(),
            "nothing of the abandoned import was stored"
        );
        let staging = std::fs::read_dir(directory.path().join("files/data"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".staging"))
            .count();
        assert_eq!(staging, 0, "the abandoned import left its staging file");
        store.shutdown().await.unwrap();
    }
}
