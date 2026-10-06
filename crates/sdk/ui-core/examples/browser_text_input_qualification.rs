//! Explicit real-CEF qualification; requires a synthetic HTTP fixture and owned profile.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "explicit qualification executable asserts synthetic fixture outcomes and fails immediately on missing receipts or malformed invocation; it is not a production application"
)]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_ui::time::Duration;

use serde_json::Value;
use tilo_ui::Widget;
use tilo_ui::headless::Framed;
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::webview::chromium::ChromiumEngine;
use tilo_ui::webview::text_input::{Failure, Receipt};
use tilo_ui::webview::{KeyInput, KeyModifiers, SharedEngine, WebViewCommand, WebViewEvent};
use tilo_ui::widget::WebView;

// Exercise the production decorator blanket implementation, which stands in for
// its inner widget in retained layout and focused dispatch.
struct GuardedView(WebView);
impl tilo_ui::widget::Decorator for GuardedView {
    type Inner = WebView;
    fn inner(&self) -> &WebView {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut WebView {
        &mut self.0
    }
}

static PROBE: AtomicU64 = AtomicU64::new(1);

#[allow(
    clippy::disallowed_methods,
    reason = "standalone qualification CLI polls its owned CEF process in bounded 10 ms waits; it has no UI event loop"
)]
fn pause() {
    std::thread::sleep(Duration::from_millis(10));
}

fn evaluate(engine: &SharedEngine, expression: &str) -> Value {
    let prefix = format!("probe{}:", PROBE.fetch_add(1, Ordering::Relaxed));
    engine.send(WebViewCommand::SetUserScripts(Arc::new(vec![format!(
        "document.title={prefix:?}+JSON.stringify(({expression}));"
    )])));
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .expect("bounded fixture deadline");
    loop {
        for event in engine.drain_events() {
            assert!(
                !matches!(event, WebViewEvent::PageResult { .. }),
                "private input leaked into public page events"
            );
            if let WebViewEvent::TitleChanged(title) = event
                && let Some(value) = title.strip_prefix(&prefix)
            {
                engine.send(WebViewCommand::SetUserScripts(Arc::new(Vec::new())));
                return serde_json::from_str(value).expect("synthetic page observation");
            }
        }
        assert!(
            tilo_ui::time::now() < deadline,
            "actual page did not answer"
        );
        pause();
    }
}

fn wait_ready<W: Widget + 'static>(view: &mut Framed<W>) {
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .expect("bounded fixture deadline");
    loop {
        Framed::harness_mut(view).relayout();
        if view.ime_purpose().is_some() {
            assert!(
                view.ime_requires_deferred_ack(),
                "decorated field must retain its private acknowledgement contract"
            );
            assert!(view.ime_edit_fingerprint(&[7; 32]).is_some());
            return;
        }
        assert!(
            tilo_ui::time::now() < deadline,
            "mounted DOM field did not acquire a guarded lease"
        );
        pause();
    }
}

fn selected<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, target: &str) {
    view.set_focus(false);
    Framed::harness_mut(view).relayout();
    let expression = if target == "rich" {
        "rich.focus();let r=document.createRange();r.selectNodeContents(rich.querySelector('b'));let s=getSelection();s.removeAllRanges();s.addRange(r);"
            .to_string()
    } else {
        format!("{target}.focus();{target}.setSelectionRange(6,10);")
    };
    assert_eq!(
        evaluate(engine, &format!("(()=>{{{expression}return true}})()")),
        true
    );
    view.set_focus(true);
    wait_ready(view);
}

fn finish<W: Widget + 'static>(view: &mut Framed<W>, receipt: &Receipt) -> Result<(), Failure> {
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(12))
        .expect("bounded fixture deadline");
    loop {
        Framed::harness_mut(view).relayout();
        if let Some(result) = receipt.poll() {
            return result.and_then(|lease| {
                if lease.is_none() {
                    Ok(())
                } else {
                    Err(Failure::Unknown)
                }
            });
        }
        if tilo_ui::time::now() >= deadline {
            return Err(Failure::Unknown);
        }
        pause();
    }
}

fn load(engine: &SharedEngine, url: &str) {
    engine.send(WebViewCommand::Navigate(url.into()));
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .expect("bounded fixture deadline");
    loop {
        if engine.drain_events().iter().any(|event| {
            matches!(
                event,
                WebViewEvent::LoadFinished | WebViewEvent::LoadFinishedAt { .. }
            )
        }) {
            return;
        }
        assert!(
            tilo_ui::time::now() < deadline,
            "synthetic fixture did not load"
        );
        pause();
    }
}

fn native_rich_baseline<W: Widget + 'static>(
    view: &mut Framed<W>,
    engine: &SharedEngine,
    url: &str,
) -> Value {
    load(engine, url);
    selected(view, engine, "rich");
    for character in "日本語 café العربية".chars() {
        for down in [true, false] {
            engine.send(WebViewCommand::Key {
                key: KeyInput::Character(character.to_string()),
                down,
                modifiers: KeyModifiers::default(),
            });
        }
    }
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .expect("bounded fixture deadline");
    let snapshot = loop {
        let snapshot = evaluate(engine, "({text:rich.textContent,html:rich.innerHTML})");
        if snapshot["text"] == "alpha 日本語 café العربية gamma" {
            break snapshot;
        }
        assert!(
            tilo_ui::time::now() < deadline,
            "native keys were not delivered: {snapshot}"
        );
        pause();
    };
    assert_eq!(
        snapshot["text"], "alpha 日本語 café العربية gamma",
        "Chromium's independent native typing control"
    );
    assert_eq!(evaluate(engine, "rich.querySelectorAll('b').length"), 1);
    snapshot
}

fn qualify_replacements<W: Widget + 'static>(
    view: &mut Framed<W>,
    engine: &SharedEngine,
    url: &str,
) {
    let native_rich = native_rich_baseline(view, engine, url);
    assert_eq!(
        native_rich["html"],
        "alpha <b>日本語 café العربية</b> gamma"
    );
    // Chromium's one-batch native command rebalances the inline boundary to
    // &nbsp;; character typing does not. Assert both exact representations,
    // bold ownership, and exact restoration instead of normalizing either.
    let native_batch = serde_json::json!({"text":"alpha 日本語 café العربية\u{a0}gamma","html":"alpha <b>日本語 café العربية</b>&nbsp;gamma"});
    for target in ["first", "notes", "rich"] {
        load(engine, url);
        selected(view, engine, target);
        let before = view.ime_edit_fingerprint(&[7; 32]).unwrap();
        let receipt = view
            .commit_deferred_text("日本語 café العربية")
            .expect("deferred native contract");
        assert!(
            view.ime_purpose().is_none(),
            "one-use lease must leave IME admission"
        );
        assert_eq!(finish(view, &receipt), Ok(()));
        let value = if target == "rich" {
            "rich.textContent".to_string()
        } else {
            format!("{target}.value")
        };
        let expected = if target == "rich" {
            "alpha 日本語 café العربية\u{a0}gamma"
        } else {
            "alpha 日本語 café العربية gamma"
        };
        assert_eq!(evaluate(engine, &value), expected);
        if target == "rich" {
            assert_eq!(
                evaluate(engine, "({text:rich.textContent,html:rich.innerHTML})"),
                native_batch,
                "one-batch Chromium edit must preserve exact native HTML"
            );
        }
        assert_eq!(evaluate(engine, "document.execCommand('undo')"), true);
        assert_eq!(evaluate(engine, &value), "alpha beta gamma");
        if target == "rich" {
            assert_eq!(
                evaluate(engine, "rich.innerHTML"),
                "alpha <b>beta</b> gamma",
                "one undo restores the exact original HTML"
            );
        }
        assert_eq!(evaluate(engine, "document.execCommand('redo')"), true);
        assert_eq!(evaluate(engine, &value), expected);
        if target == "rich" {
            assert_eq!(
                evaluate(engine, "({text:rich.textContent,html:rich.innerHTML})"),
                native_batch
            );
        }
        wait_ready(view);
        assert_ne!(view.ime_edit_fingerprint(&[7; 32]).unwrap(), before);
        println!(
            "PASS mounted {target}: acknowledged Unicode selection replacement and native undo/redo"
        );
    }
}
fn qualify_refusals<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    for (name, edit) in [
        (
            "focus return",
            "second.focus();first.focus();first.setSelectionRange(6,10)",
        ),
        ("edit stamp", "first.value='externally changed'"),
        (
            "transient protected styling",
            "first.style.webkitTextSecurity='disc';first.style.webkitTextSecurity='none'",
        ),
        (
            "field replacement",
            "first.replaceWith(first.cloneNode(true))",
        ),
        ("page rejection", "window.rejectEdit=true"),
        ("protected field", "first.type='password'"),
    ] {
        load(engine, url);
        selected(view, engine, "first");
        assert_eq!(
            evaluate(engine, &format!("(()=>{{{edit};return true}})()")),
            true
        );
        let receipt = view
            .commit_deferred_text("must remain private")
            .expect("original native receipt");
        assert_eq!(finish(view, &receipt), Err(Failure::Rejected));
        assert_eq!(evaluate(engine, "second.value"), "second");
        assert_eq!(
            evaluate(engine, "first.value.includes('must remain private')"),
            false
        );
        println!("PASS mounted {name}: refused original transaction without retargeting");
    }
}

fn qualify_queued_finals<W: Widget + 'static>(
    view: &mut Framed<W>,
    engine: &SharedEngine,
    url: &str,
) {
    load(engine, url);
    selected(view, engine, "first");
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_millis(600))
        .expect("bounded fixture check cadence");
    while tilo_ui::time::now() < deadline {
        pause();
    }
    // Queue a real heartbeat check, then a final before consuming its reply.
    Framed::harness_mut(view).relayout();
    let receipt = view.commit_deferred_text("checked once").unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(evaluate(engine, "first.value"), "alpha checked once gamma");
    assert_eq!(evaluate(engine, "document.execCommand('undo')"), true);
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    println!("PASS mounted queued check: one original-field final and one native undo");

    selected(view, engine, "notes");
    let receipt = view
        .commit_deferred_text("canceled before admission")
        .unwrap();
    receipt.cancel();
    assert!(matches!(receipt.poll(), Some(Err(Failure::Unknown))));
    assert_eq!(finish(view, &receipt), Err(Failure::Unknown));
    assert_eq!(evaluate(engine, "notes.value"), "alpha beta gamma");
    selected(view, engine, "notes");
    let receipt = view.commit_deferred_text("replacement session").unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(
        evaluate(engine, "notes.value"),
        "alpha replacement session gamma"
    );
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    println!("PASS mounted canceled queued final and repeated fresh session without retargeting");
}

fn qualify_view<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    load(engine, url);
    qualify_replacements(view, engine, url);
    qualify_common_fields(view, engine, url);
    qualify_refusals(view, engine, url);
    qualify_queued_finals(view, engine, url);
    load(engine, url);
    selected(view, engine, "first");
    let old = view.ime_edit_fingerprint(&[7; 32]).unwrap();
    // Distinct loopback sites exercise Chromium's main-frame context replacement,
    // not just a same-site reload. The owned runner always supplies 127.0.0.1.
    let replacement = url.replacen("127.0.0.1", "localhost", 1);
    assert_ne!(
        replacement, url,
        "qualification needs the owned loopback fixture"
    );
    load(engine, &replacement);
    let receipt = view
        .commit_deferred_text("old document")
        .expect("original navigation receipt");
    assert_eq!(finish(view, &receipt), Err(Failure::Rejected));
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    selected(view, engine, "first");
    assert_ne!(view.ime_edit_fingerprint(&[7; 32]).unwrap(), old);
    let receipt = view.commit_deferred_text("fresh session").unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(evaluate(engine, "first.value"), "alpha fresh session gamma");
    println!(
        "PASS mounted cross-site frame replacement and recovery to a fresh original-field lease"
    );
    selected(view, engine, "notes");
    view.set_focus(false);
    assert!(view.ime_purpose().is_none());
    let receipt = view.commit_deferred_text("canceled").unwrap();
    assert_eq!(finish(view, &receipt), Err(Failure::Rejected));
    assert_eq!(evaluate(engine, "notes.value"), "alpha beta gamma");
    println!("PASS mounted cancellation: no edit or successor retargeting");
    view.set_focus(false);
}

fn qualify_common_fields<W: Widget + 'static>(
    view: &mut Framed<W>,
    engine: &SharedEngine,
    url: &str,
) {
    for kind in ["search", "tel", "url", "plaintext-only"] {
        load(engine, url);
        view.set_focus(false);
        Framed::harness_mut(view).relayout();
        let setup = if kind == "plaintext-only" {
            "let e=document.createElement('div');e.id='commonField';e.contentEditable='plaintext-only';e.textContent='alpha beta gamma';document.body.append(e);e.focus();let r=document.createRange();r.setStart(e.firstChild,6);r.setEnd(e.firstChild,10);let s=getSelection();s.removeAllRanges();s.addRange(r);".to_string()
        } else {
            format!(
                "let e=document.createElement('input');e.id='commonField';e.type={kind:?};e.value='alpha beta gamma';document.body.append(e);e.focus();e.setSelectionRange(6,10);"
            )
        };
        assert_eq!(
            evaluate(engine, &format!("(()=>{{{setup}return true}})()")),
            true
        );
        view.set_focus(true);
        wait_ready(view);
        let stamp = view.ime_edit_fingerprint(&[7; 32]).unwrap();
        let start = view
            .start_deferred_text()
            .expect("original common-field start");
        assert_eq!(finish(view, &start), Ok(()));
        assert_eq!(view.ime_edit_fingerprint(&[7; 32]), Some(stamp));
        let final_receipt = view
            .commit_deferred_text("日本語 café العربية")
            .expect("original common-field final");
        assert_eq!(finish(view, &final_receipt), Ok(()));
        let value = if kind == "plaintext-only" {
            "commonField.textContent"
        } else {
            "commonField.value"
        };
        assert_eq!(evaluate(engine, value), "alpha 日本語 café العربية gamma");
        assert_eq!(evaluate(engine, "document.execCommand('undo')"), true);
        assert_eq!(evaluate(engine, value), "alpha beta gamma");
        assert_eq!(evaluate(engine, "document.execCommand('redo')"), true);
        assert_eq!(evaluate(engine, value), "alpha 日本語 café العربية gamma");
        println!(
            "PASS mounted {kind}: original start/final ACK, Unicode replacement and native undo/redo"
        );
    }
}

fn mount<W: Widget + 'static>(
    host: &Path,
    profile: &Path,
    url: &str,
    make: impl FnOnce(SharedEngine) -> W,
) {
    let engine: SharedEngine =
        ChromiumEngine::with_host(host, profile).expect("actual sandboxed Chromium host");
    engine.set_visible(true);
    let mut view = Framed::new(
        State::new(),
        make(engine.clone()),
        (1000, 800),
        Theme::dark(),
    );
    qualify_view(&mut view, &engine, url);
    drop(view);
    drop(engine);
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] {
        println!("browser-text-input-qualification HOST PROFILE SYNTHETIC_HTTP_URL");
        return;
    }
    assert_eq!(
        args.len(),
        3,
        "HOST PROFILE SYNTHETIC_HTTP_URL are required"
    );
    for (name, decorated) in [("direct", false), ("decorated", true)] {
        println!("QUALIFY {name} retained field owner");
        let profile = Path::new(&args[1]).join(name);
        if decorated {
            mount(Path::new(&args[0]), &profile, &args[2], |engine| {
                GuardedView(WebView::new(engine))
            });
        } else {
            mount(Path::new(&args[0]), &profile, &args[2], WebView::new);
        }
    }
}
