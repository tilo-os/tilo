//! Isolate ad-blocker-related blank frames. The example loads a page, reports
//! its frame uniformity, then installs content filters, user scripts, or both.
//!
//! Run:
//!   WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
//!   cargo run -p browser --example adblock_blank_repro --features wpe --release -- both

use std::sync::Arc;
use ui::time::Duration;

use ui::webview::wpe::WpeEngine;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEngine};

fn latest(engine: &dyn WebViewEngine) -> Option<(Arc<Vec<u8>>, u32, u32)> {
    engine
        .frames()
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| (f.rgba.clone(), f.width, f.height))
}

fn uniformity(rgba: &[u8]) -> f64 {
    if rgba.len() < 4 {
        return 1.0;
    }
    let (r0, g0, b0) = (rgba[0], rgba[1], rgba[2]);
    let (mut same, mut total) = (0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        total += 1;
        if px[0] == r0 && px[1] == g0 && px[2] == b0 {
            same += 1;
        }
    }
    same as f64 / total.max(1) as f64
}

fn report(engine: &dyn WebViewEngine, name: &str) {
    match latest(engine) {
        Some((rgba, w, h)) => {
            let u = uniformity(&rgba) * 100.0;
            println!(
                "  {name}: {w}x{h} uniformity {u:.1}% {}",
                if u > 97.0 { "<< BLANK" } else { "" }
            );
        }
        None => println!("  {name}: NO FRAME"),
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "both".into());
    let url = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "https://duckduckgo.com".into());
    println!("mode={mode} url={url}");

    if mode == "analyze" {
        analyze();
        return;
    }

    let engine: SharedEngine = WpeEngine::new().expect("engine");
    engine.send(WebViewCommand::Resize {
        width: 900,
        height: 600,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate(url));

    // Wait for the real page.
    let deadline = ui::time::now() + Duration::from_secs(25);
    while ui::time::now() < deadline {
        if let Some((r, _, _)) = latest(&*engine) {
            if uniformity(&r) < 0.9 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("== before ad blocker ==");
    report(&*engine, "before install");

    // Optional 3rd arg: install only the first N compiled chunks (to find
    // whether it's a volume limit). Default: all, via the real loader.
    let max_chunks: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    match mode.as_str() {
        "filter" | "both" => {
            if max_chunks == usize::MAX {
                println!("installing content filter (all chunks)…");
                browser::blocker::spawn_filter_loader(engine.clone());
            } else {
                println!("installing content filter (first {max_chunks} chunks)…");
                // REPRO_LISTS=easylist,easyprivacy restricts to those lists
                // (name prefix match); default = all cached lists.
                let want = std::env::var("REPRO_LISTS").unwrap_or_default();
                let dir = tilo_paths::cache_dir()
                    .unwrap()
                    .join("tilo-browser")
                    .join("filters");
                let texts: Vec<String> = std::fs::read_dir(&dir)
                    .unwrap()
                    .flatten()
                    .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("txt"))
                    .filter(|e| {
                        want.is_empty() || {
                            let n = e.file_name().to_string_lossy().to_string();
                            want.split(',').any(|w| n.starts_with(w.trim()))
                        }
                    })
                    .filter_map(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        std::fs::read_to_string(e.path()).ok().map(|t| {
                            println!("  + {n}");
                            t
                        })
                    })
                    .collect();
                let chunked = ui::webview::adblock::compile_filter_lists_chunked(
                    texts.iter().map(String::as_str),
                    25_000,
                )
                .unwrap();
                let chunks: Vec<Arc<String>> =
                    chunked.chunks.into_iter().take(max_chunks).collect();
                println!("  sending {} chunks", chunks.len());
                // Distinct version per N so the store doesn't serve a
                // cached superset.
                engine.send(WebViewCommand::SetContentFilters {
                    version: 0xADB1_0C00_u64 + max_chunks as u64,
                    chunks: Arc::new(chunks),
                });
            }
        }
        _ => {}
    }
    if mode == "scripts" || mode == "both" {
        println!("installing user scripts…");
        engine.send(WebViewCommand::SetUserScripts(Arc::new(
            browser::blocker::user_scripts(),
        )));
    }

    // Wait past the async fetch + compile + install (~5–15s).
    for i in 1..=18 {
        std::thread::sleep(Duration::from_secs(1));
        if i % 3 == 0 {
            let u = latest(&*engine)
                .map(|(r, _, _)| uniformity(&r) * 100.0)
                .unwrap_or(0.0);
            println!("  t+{i}s uniformity {u:.1}%");
        }
    }
    println!("== after install ==");
    report(&*engine, "after install");

    // The user's actual repro: RELOAD with the filter already active (a
    // page *loading* with the filter, vs the filter applied post-load).
    println!("== RELOAD with filter active ==");
    engine.send(WebViewCommand::Reload);
    for i in 1..=10 {
        std::thread::sleep(Duration::from_secs(1));
        if i % 2 == 0 {
            let u = latest(&*engine)
                .map(|(r, _, _)| uniformity(&r) * 100.0)
                .unwrap_or(0.0);
            println!("  reload t+{i}s uniformity {u:.1}%");
        }
    }
    report(&*engine, "after reload");

    // Also test a fresh Navigate to the same URL with the filter active.
    println!("== NAVIGATE with filter active ==");
    engine.send(WebViewCommand::Navigate("https://duckduckgo.com".into()));
    for i in 1..=10 {
        std::thread::sleep(Duration::from_secs(1));
        if i % 2 == 0 {
            let u = latest(&*engine)
                .map(|(r, _, _)| uniformity(&r) * 100.0)
                .unwrap_or(0.0);
            println!("  navigate t+{i}s uniformity {u:.1}%");
        }
    }
    report(&*engine, "after navigate");
    println!("done");
}

/// Compile the cached filter lists (honouring REPRO_LISTS to select a
/// subset) and print any rule that could blank an entire page on load: a
/// broad `css-display-none`, or a `block` whose `url-filter` is short
/// enough to match unrelated first-party resources (documents/scripts/
/// stylesheets), which is how a generic list can kill a page's own assets.
fn analyze() {
    let want = std::env::var("REPRO_LISTS").unwrap_or_default();
    let dir = tilo_paths::cache_dir()
        .unwrap()
        .join("tilo-browser")
        .join("filters");
    let mut texts = Vec::new();
    let mut names = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) == Some("txt") {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            if !want.is_empty() && !want.split(',').any(|w| n.starts_with(w.trim())) {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(&p) {
                names.push(n);
                texts.push(t);
            }
        }
    }
    println!("lists: {names:?}");
    let chunked = ui::webview::adblock::compile_filter_lists_chunked(
        texts.iter().map(String::as_str),
        25_000,
    )
    .expect("compile");
    println!(
        "rules={} chunks={}",
        chunked.rule_count,
        chunked.chunks.len()
    );

    let broad_css = |sel: &str| {
        let s = sel.trim();
        matches!(s, "html" | "body" | ":root" | "*" | "html *" | "body *")
            || s.is_empty()
            || s == "html, body"
    };
    // A block rule that can hit first-party page assets: matches the main
    // document / scripts / stylesheets, over a short (=> broad) url-filter,
    // and is NOT scoped away from the loading site via unless-domain.
    let doc_types = ["document", "script", "style-sheet"];
    let (mut blank_css, mut block_all, mut broad_doc) = (0u32, 0u32, 0u32);
    for chunk in &chunked.chunks {
        let arr: serde_json::Value = serde_json::from_str(chunk).unwrap();
        for r in arr.as_array().unwrap() {
            let action = r["action"]["type"].as_str().unwrap_or("");
            let uf = r["trigger"]["url-filter"].as_str().unwrap_or("");
            if action == "css-display-none" {
                if let Some(sel) = r["action"]["selector"].as_str() {
                    if broad_css(sel) {
                        blank_css += 1;
                        if blank_css <= 10 {
                            println!("  BLANK-CSS selector={sel:?} trigger={}", r["trigger"]);
                        }
                    }
                }
            } else if action == "block" {
                let unless = r["trigger"].get("unless-domain").is_some();
                let if_dom = r["trigger"].get("if-domain").is_some();
                let rtypes: Vec<&str> = r["trigger"]["resource-type"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                let hits_doc = rtypes.is_empty() || rtypes.iter().any(|t| doc_types.contains(t));
                // Strip the scheme anchor to gauge how much path the filter
                // actually constrains.
                let core = uf
                    .trim_start_matches("^https?://")
                    .trim_start_matches(".*")
                    .trim_start_matches('^');
                if (uf == ".*" || uf == "^https?://" || uf.is_empty()) && !if_dom && !unless {
                    block_all += 1;
                    if block_all <= 8 {
                        println!("  BLOCK-ALL rtypes={rtypes:?} trigger={}", r["trigger"]);
                    }
                } else if hits_doc && !if_dom && core.len() < 6 {
                    broad_doc += 1;
                    if broad_doc <= 20 {
                        println!("  BROAD-DOC uf={uf:?} rtypes={rtypes:?} unless={unless}");
                    }
                }
            }
        }
    }
    println!(
        "page-blanking css: {blank_css}, block-all: {block_all}, broad first-party-asset blocks: {broad_doc}"
    );
}
