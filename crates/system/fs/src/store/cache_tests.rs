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
    // The cache counts how often entries are read once it is half full.
    for seed in 0..per_budget / 2 {
        cold(seed).await;
    }
    let hot_bytes = Bytes::from_static(b"a manifest everybody reads");
    let hot = store.ingest(hot_bytes.clone()).await.unwrap();
    assert!(store.is_cached(hot), "small blobs are cached on ingest");
    for _ in 0..10 {
        assert_eq!(store.local(hot).await.unwrap(), hot_bytes);
    }

    // One blob above the per-entry cap is neither cached on ingest nor on read.
    let large = distinct(MAX_CACHED_BLOB + 1, u64::MAX);
    let large_hash = store.ingest(large.clone()).await.unwrap();
    assert!(!store.is_cached(large_hash));
    assert_eq!(store.local(large_hash).await.unwrap(), large);
    assert!(!store.is_cached(large_hash));

    // Three times the budget of cold blobs at the cap, each read once, with
    // the hot entry read only after more than a whole budget of them: a
    // cache that kept the most recent bytes would have dropped it by then.
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
    let mut tags = tests_tag_names(store.store()).await;
    tags.sort();
    assert_eq!(
        tags,
        [owned_blob_tag(hash, BlobFormat::Raw).into_bytes()],
        "the import is owned like any ingest, with no temporary tag left"
    );
}

#[tokio::test]
async fn an_abandoned_streamed_ingest_stores_nothing_and_leaves_no_spill() {
    let directory = tempfile::tempdir().unwrap();
    // iroh-blobs forwards up to 32 chunks before its store consumes them.
    // Cross that bound before abandoning: a three-chunk fixture can report
    // its producer error before the store has created any spill to clean up.
    let content = vec![5_u8; 64 * 64 * 1024];
    {
        let store = ContentStore::open(directory.path()).await.unwrap();
        let pieces = pieces_of(&content, 64 * 1024).chain(futures_util::stream::iter([Err(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "abandoned"),
        )]));
        assert!(store.ingest_stream(pieces).await.is_err());
        assert!(!store.has(Hash::new(&content)).await);
        assert!(tests_tag_names(store.store()).await.is_empty());
        assert!(
            store
                .store()
                .blobs()
                .list()
                .hashes()
                .await
                .unwrap()
                .is_empty(),
            "nothing of the abandoned import was stored"
        );
        let spilled = std::fs::read_dir(directory.path().join("temp"))
            .unwrap()
            .count();
        assert!(
            spilled > 0,
            "the store no longer leaves a spill behind; drop remove_import_spills"
        );
        store.shutdown().await.unwrap();
    }
    let store = ContentStore::open(directory.path()).await.unwrap();
    assert_eq!(
        std::fs::read_dir(directory.path().join("temp"))
            .unwrap()
            .count(),
        0,
        "reopening the store removes earlier runs' spills"
    );
    store.shutdown().await.unwrap();
}

async fn tests_tag_names(store: &Store) -> Vec<Vec<u8>> {
    let mut tags = store.tags().list().await.unwrap();
    let mut names = Vec::new();
    while let Some(tag) = tags.next().await {
        names.push(tag.unwrap().name.as_ref().to_vec());
    }
    names
}
