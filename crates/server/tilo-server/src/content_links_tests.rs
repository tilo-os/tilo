use std::io::Read as _;

use tilo_actions::remote::{ContentLinkAsk, ContentLinkFile};
use tilo_content::ContentStore;

use tilo_actions::remote::viewable;

use super::{ContentLinks, ROUTE, Refusal, Sink, inline_disposition, prepare, stream};

/// Every byte a link sends, kept.
#[derive(Default)]
struct Kept(Vec<u8>, usize);

impl Sink for Kept {
    fn send(&mut self, bytes: Vec<u8>) -> bool {
        self.1 = self.1.max(bytes.len());
        self.0.extend_from_slice(&bytes);
        true
    }
}

/// `bytes` kept as the folder mount keeps a large file: its chunks, and the
/// manifest listing them, whose hash is the one its node names.
fn chunked(store: &ContentStore, bytes: &[u8]) -> String {
    let (manifest, _) = fs_content::build(bytes, None);
    for (index, chunk) in manifest.chunks().iter().enumerate() {
        let range = manifest
            .chunk_range(index)
            .expect("each chunk has its range");
        let start = usize::try_from(range.start).unwrap();
        let len = usize::try_from(chunk.len).unwrap();
        let piece = bytes.get(start..start.saturating_add(len)).unwrap();
        let kept = store.put(piece).unwrap();
        assert_eq!(
            kept,
            hex::encode(chunk.hash),
            "a chunk is kept under its hash"
        );
    }
    store.put(&manifest.encode()).unwrap()
}

fn served(store: &ContentStore, ask: &ContentLinkAsk) -> Vec<u8> {
    let streaming = prepare(store, ask).expect("its files are kept");
    let mut kept = Kept::default();
    stream(store, &streaming, &mut kept).expect("its files stream");
    assert_eq!(
        kept.0.len() as u64,
        streaming.length,
        "the length said up front is what went"
    );
    assert!(kept.1 <= super::BUFFER, "no piece larger than a buffer");
    kept.0
}

fn file(path: &str, hash: &str) -> ContentLinkFile {
    ContentLinkFile {
        path: path.into(),
        hash: hash.into(),
    }
}

/// A file goes as its own bytes, whether kept whole or as the chunks of a
/// manifest; a folder goes as a zip a file manager opens, every file under
/// its path with its bytes.
#[test]
fn a_link_serves_files_whole_or_chunked_and_folders_as_a_zip() {
    let store = ContentStore::memory();
    let small = b"# Plan\n".to_vec();
    let small_hash = store.put(&small).unwrap();
    // Past the mount's file-backed size: several chunks.
    let large: Vec<u8> = (0..3_000_000_u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0])
        .collect();
    let large_hash = chunked(&store, &large);
    let empty_hash = store.put(b"").unwrap();

    let one = |path: &str, hash: &str| ContentLinkAsk {
        name: path.into(),
        files: vec![file(path, hash)],
        zip: false,
    };
    assert_eq!(served(&store, &one("plan.md", &small_hash)), small);
    assert_eq!(
        served(&store, &one("big.bin", &large_hash)),
        large,
        "a chunked file reads as its bytes"
    );

    let folder = ContentLinkAsk {
        name: "Plans.zip".into(),
        files: vec![
            file("Plans/plan.md", &small_hash),
            file("Plans/2026/big.bin", &large_hash),
            file("Plans/empty.txt", &empty_hash),
            file("Plans/naïve résumé.md", &small_hash),
        ],
        zip: true,
    };
    let bytes = served(&store, &folder);
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a zip a reader opens");
    assert_eq!(archive.len(), 4);
    for (path, want) in [
        ("Plans/plan.md", &small),
        ("Plans/2026/big.bin", &large),
        ("Plans/empty.txt", &Vec::new()),
        ("Plans/naïve résumé.md", &small),
    ] {
        let mut member = archive.by_name(path).expect("each file under its path");
        let mut read = Vec::new();
        member
            .read_to_end(&mut read)
            .expect("its bytes read back, checksum and all");
        assert_eq!(&read, want, "{path}");
    }

    let nothing = ContentLinkAsk {
        files: Vec::new(),
        ..folder.clone()
    };
    let bytes = served(&store, &nothing);
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .unwrap()
            .len(),
        0
    );

    let gone = ContentLinkAsk {
        files: vec![file("Plans/lost.md", &"cd".repeat(32))],
        ..folder
    };
    assert_eq!(
        prepare(&store, &gone).err(),
        Some(Refusal::Missing("Plans/lost.md".into())),
        "a file the host does not keep is said, not sent short"
    );
}

/// A link is found by the token its path names, below the route, and by
/// nothing else.
#[test]
fn a_link_is_found_by_its_token_alone() {
    let links = ContentLinks::default();
    let store = ContentStore::memory();
    let hash = store.put(b"x").unwrap();
    let ask = ContentLinkAsk {
        name: "Q3 plan (final).md".into(),
        files: vec![file("Q3 plan (final).md", &hash)],
        zip: false,
    };
    let link = links.mint(store, ask.clone());
    let route = link
        .path
        .strip_prefix(&format!("{}{ROUTE}", tilo_server_protocol::web::API_PREFIX))
        .expect("below the content route");
    let (token, name) = route.split_once('/').expect("a token, then the name");
    assert_eq!(token.len(), 64);
    assert_eq!(
        name, "Q3%20plan%20%28final%29.md",
        "the name, as one segment"
    );
    assert_eq!(links.grant(route).map(|grant| grant.ask), Some(ask));
    assert!(links.grant(&format!("{}/x", "00".repeat(32))).is_none());
    assert!(links.grant("not-a-token/x").is_none());
    assert_eq!(
        super::disposition("naïve \"q\".md"),
        "attachment; filename=\"na_ve _q_.md\"; filename*=UTF-8''na%C3%AFve%20%22q%22.md"
    );
}

#[test]
fn a_link_shows_in_a_tab_only_what_a_browser_plays_or_shows_safely() {
    assert_eq!(viewable("walkthrough.MP4"), Some("video/mp4"));
    assert_eq!(viewable("voice.m4a"), Some("audio/mp4"));
    assert_eq!(viewable("scan.png"), Some("image/png"));
    for saved in [
        "page.html",
        "logo.svg",
        "script.js",
        "report.pdf",
        "archive.zip",
        "no-extension",
    ] {
        assert_eq!(viewable(saved), None, "{saved} is saved, never shown");
    }
    assert!(
        inline_disposition("walkthrough.mp4").starts_with("inline; filename=\"walkthrough.mp4\"")
    );
}
