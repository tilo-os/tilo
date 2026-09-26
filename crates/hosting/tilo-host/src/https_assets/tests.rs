use super::*;
use crate::https::tests::asset_test_lease;

#[tokio::test]
async fn app_navigation_can_fallback_but_api_and_missing_assets_cannot() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("index.html"), b"app").unwrap();
    std::fs::write(directory.path().join("main.js"), b"runtime").unwrap();
    let root = AssetRoot::open(directory.path()).unwrap();
    assert!(
        root.resolve("/workspace/one", true, asset_test_lease())
            .await
            .is_ok()
    );
    assert!(
        root.resolve("/main.js", false, asset_test_lease())
            .await
            .is_ok()
    );
    for path in [
        "/peer/v3/missing",
        "/qos-peer/status",
        "/api/missing",
        "/missing.js",
        "/.secret",
        "/../secret",
        "/%2e%2e/secret",
        "//secret",
    ] {
        assert!(
            root.resolve(path, true, asset_test_lease()).await.is_err(),
            "{path}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_root_rejects_symlink_escape_and_directory_listing() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("secret.js"), b"do not serve").unwrap();
    std::fs::write(directory.path().join("normal.js"), b"ordinary asset").unwrap();
    std::os::unix::fs::symlink(external.path(), directory.path().join("escape")).unwrap();
    std::os::unix::fs::symlink(
        directory.path().join("normal.js"),
        directory.path().join("final.js"),
    )
    .unwrap();
    std::fs::create_dir(directory.path().join("empty.js")).unwrap();
    let root = AssetRoot::open(directory.path()).unwrap();
    let ordinary = root
        .resolve("/normal.js", false, asset_test_lease())
        .await
        .unwrap();
    assert_eq!(ordinary.length, 14);
    assert_eq!(ordinary.content_type, "text/javascript; charset=utf-8");
    assert!(
        root.resolve("/escape/secret.js", false, asset_test_lease())
            .await
            .is_err()
    );
    assert!(
        root.resolve("/final.js", false, asset_test_lease())
            .await
            .is_err()
    );
    assert!(
        root.resolve("/empty.js", false, asset_test_lease())
            .await
            .is_err()
    );
}

async fn resolve_encoded(root: &AssetRoot, path: &str) -> Asset {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let asset = root.resolve(path, false, asset_test_lease()).await.unwrap();
        if asset.encoded.is_some() || std::time::Instant::now() > deadline {
            return asset;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn immutable_assets_gain_validator_and_brotli_variant_without_blocking() {
    let directory = tempfile::tempdir().unwrap();
    let script = "export const value = 'repeated product bytes';\n".repeat(2048);
    std::fs::write(directory.path().join("main.js"), &script).unwrap();
    std::fs::write(directory.path().join("tiny.js"), b"x").unwrap();
    let root = AssetRoot::open(directory.path()).unwrap();
    // The first request is served before any encoding exists.
    let first = root
        .resolve("/main.js", false, asset_test_lease())
        .await
        .unwrap();
    assert_eq!(first.length, script.len() as u64);
    let asset = resolve_encoded(&root, "/main.js").await;
    let encoded = asset.encoded.expect("background validator");
    assert_eq!(encoded.tag.len(), 32);
    let compressed = encoded.brotli.expect("compressible script variant");
    assert!(compressed.len() < script.len() / 10);
    let mut plain = Vec::new();
    brotli::BrotliDecompress(&mut &compressed[..], &mut plain).unwrap();
    assert_eq!(plain, script.as_bytes());
    // Tiny files keep a validator but no compressed variant.
    let tiny = resolve_encoded(&root, "/tiny.js")
        .await
        .encoded
        .expect("validator");
    assert!(tiny.brotli.is_none());
}

#[tokio::test]
async fn asset_response_revalidates_and_negotiates_brotli() {
    let directory = tempfile::tempdir().unwrap();
    let script = "console.log('immutable runtime module');\n".repeat(4096);
    std::fs::write(directory.path().join("main.js"), &script).unwrap();
    let root = AssetRoot::open(directory.path()).unwrap();
    let _ = resolve_encoded(&root, "/main.js").await;
    let request = |headers: &[(&str, &str)]| {
        let mut builder = hyper::Request::get("/main.js");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap().into_parts().0
    };
    let serve = |parts: &hyper::http::request::Parts| {
        let root = root.clone();
        let parts = parts.clone();
        async move {
            let asset = root
                .resolve("/main.js", false, asset_test_lease())
                .await
                .unwrap();
            crate::https::asset_response_head(&parts, asset, asset_test_lease())
        }
    };
    let identity = serve(&request(&[])).await;
    assert_eq!(identity.status, 200);
    assert_eq!(identity.headers["cache-control"], "no-cache");
    assert!(identity.headers.get("content-encoding").is_none());
    assert_eq!(
        identity.headers["content-length"],
        script.len().to_string().as_str()
    );
    let tag = identity.headers["etag"].to_str().unwrap().to_owned();
    let br = serve(&request(&[("accept-encoding", "gzip, deflate, br, zstd")])).await;
    assert_eq!(br.headers["content-encoding"], "br");
    assert_eq!(br.headers["vary"], "accept-encoding");
    let br_tag = br.headers["etag"].to_str().unwrap().to_owned();
    assert_ne!(tag, br_tag);
    let refused = serve(&request(&[("accept-encoding", "br;q=0, gzip")])).await;
    assert!(refused.headers.get("content-encoding").is_none());
    let revalidated = serve(&request(&[
        ("accept-encoding", "br"),
        ("if-none-match", &br_tag),
    ]))
    .await;
    assert_eq!(revalidated.status, 304);
    assert!(revalidated.headers.get("content-encoding").is_none());
    let stale = serve(&request(&[("if-none-match", "\"0000\"")])).await;
    assert_eq!(stale.status, 200);
    let identity_hit = serve(&request(&[("if-none-match", &tag)])).await;
    assert_eq!(identity_hit.status, 304);
}

#[tokio::test]
async fn versioned_prefix_requires_the_roots_own_digest_and_is_immutable() {
    let parent = tempfile::tempdir().unwrap();
    let digest = "ab".repeat(32);
    let root_path = parent.path().join(&digest);
    std::fs::create_dir(&root_path).unwrap();
    std::fs::write(
        root_path.join("assets.json"),
        format!("{{\"digest\":\"{digest}\",\"files\":{{}}}}"),
    )
    .unwrap();
    std::fs::write(root_path.join("peer.mjs"), b"export {}").unwrap();
    std::fs::write(root_path.join("index.html"), b"app").unwrap();
    let root = AssetRoot::open(&root_path).unwrap();
    let plain = root
        .resolve("/peer.mjs", false, asset_test_lease())
        .await
        .unwrap();
    assert!(!plain.immutable);
    let versioned = root
        .resolve(&format!("/v/{digest}/peer.mjs"), false, asset_test_lease())
        .await
        .unwrap();
    assert!(versioned.immutable);
    assert_eq!(versioned.length, 9);
    let other = "cd".repeat(32);
    for path in [
        format!("/v/{other}/peer.mjs"),
        format!("/v/{digest}/"),
        format!("/v/{digest}"),
        format!("/v/{digest}/../peer.mjs"),
        format!("/v/{digest}//peer.mjs"),
        format!("/v/{digest}/peer/v3/state"),
    ] {
        assert!(
            root.resolve(&path, true, asset_test_lease()).await.is_err(),
            "{path}"
        );
    }
    // A versioned navigation never falls back to the application entry.
    assert!(
        root.resolve(&format!("/v/{digest}/workspace"), true, asset_test_lease())
            .await
            .is_err()
    );
    // A root whose name does not match its inventory digest has no prefix.
    let unnamed = tempfile::tempdir().unwrap();
    std::fs::write(
        unnamed.path().join("assets.json"),
        format!("{{\"digest\":\"{digest}\"}}"),
    )
    .unwrap();
    std::fs::write(unnamed.path().join("peer.mjs"), b"export {}").unwrap();
    let unnamed = AssetRoot::open(unnamed.path()).unwrap();
    assert!(
        unnamed
            .resolve(&format!("/v/{digest}/peer.mjs"), false, asset_test_lease())
            .await
            .is_err()
    );
}
