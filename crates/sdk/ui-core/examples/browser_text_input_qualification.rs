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
        // The input method may serve a focused page field before its lease;
        // the lease is what the field's stamp says.
        if view.ime_edit_fingerprint(&[7; 32]).is_some() {
            assert!(view.ime_content().is_some());
            assert!(
                view.ime_requires_deferred_ack(),
                "decorated field must retain its private acknowledgement contract"
            );
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
                code: None,
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

fn qualify_paragraphs<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    for paragraph in ["First café.\n\nSecond 👋", "\n\n", "First café.\n"] {
        load(engine, url);
        selected(view, engine, "notes");
        assert!(
            view.ime_multiline(),
            "original textarea policy reaches the retained owner"
        );
        let start = view.start_deferred_text().unwrap();
        assert_eq!(finish(view, &start), Ok(()));
        let receipt = view.commit_deferred_text(paragraph).unwrap();
        assert_eq!(finish(view, &receipt), Ok(()));
        let expected = format!("alpha {paragraph} gamma");
        assert_eq!(evaluate(engine, "notes.value"), expected);
        assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
        assert_eq!(evaluate(engine, "document.execCommand('undo')"), true);
        assert_eq!(evaluate(engine, "notes.value"), "alpha beta gamma");
        assert_eq!(evaluate(engine, "document.execCommand('redo')"), true);
        assert_eq!(evaluate(engine, "notes.value"), expected);
        assert_eq!(evaluate(engine, "navigator.userActivation.isActive"), false);
    }
    load(engine, url);
    selected(view, engine, "rich");
    assert!(
        view.ime_multiline(),
        "original rich field admits paragraphs"
    );
    let start = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &start), Ok(()));
    let receipt = view
        .commit_deferred_text("First café.\n\nSecond 👋")
        .unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(
        evaluate(engine, "rich.innerText"),
        "alpha First café.\n\nSecond 👋 gamma"
    );
    assert_eq!(
        evaluate(engine, "rich.innerHTML"),
        "alpha <b>First café.<br><br>Second 👋</b> gamma"
    );
    assert_eq!(evaluate(engine, "document.execCommand('undo')"), true);
    assert_eq!(
        evaluate(engine, "rich.innerHTML"),
        "alpha <b>beta</b> gamma"
    );
    assert_eq!(evaluate(engine, "document.execCommand('redo')"), true);
    assert_eq!(
        evaluate(engine, "rich.innerText"),
        "alpha First café.\n\nSecond 👋 gamma"
    );
    println!("PASS mounted rich paragraph: literal breaks, inherited bold, one native undo/redo");
    load(engine, url);
    selected(view, engine, "first");
    assert!(!view.ime_multiline(), "single-line policy remains closed");
    let start = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &start), Ok(()));
    let receipt = view.commit_deferred_text("first\nsecond").unwrap();
    assert_eq!(finish(view, &receipt), Err(Failure::Rejected));
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    println!("PASS mounted paragraph intent: exact final, one undo/redo and single-line rejection");
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
            view.ime_edit_fingerprint(&[7; 32]).is_none(),
            "one-use lease must leave deferred admission"
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

fn qualify_activation<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    load(engine, url);
    selected(view, engine, "first");
    let stamp = view.ime_edit_fingerprint(&[7; 32]);
    let receipt = view
        .start_deferred_text()
        .expect("original deferred session start");
    assert_eq!(view.ime_edit_fingerprint(&[7; 32]), stamp);
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(view.ime_edit_fingerprint(&[7; 32]), stamp);
    let repeated = view
        .start_deferred_text()
        .expect("typed repeated-start refusal");
    assert_eq!(finish(view, &repeated), Err(Failure::Rejected));
    let final_text = view.commit_deferred_text("consented original").unwrap();
    assert_eq!(finish(view, &final_text), Ok(()));
    assert_eq!(
        evaluate(engine, "first.value"),
        "alpha consented original gamma"
    );
    println!("PASS mounted one-shot session start: stable original stamp, private ACK and final");

    load(engine, url);
    selected(view, engine, "first");
    let receipt = view.start_deferred_text().unwrap();
    receipt.cancel();
    assert_eq!(finish(view, &receipt), Err(Failure::Unknown));
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    selected(view, engine, "notes");
    let receipt = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    let receipt = view.commit_deferred_text("fresh consent").unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(evaluate(engine, "notes.value"), "alpha fresh consent gamma");
    println!("PASS mounted canceled session start and independently consented repeated session");

    view.set_focus(false);
    load(engine, url);
    view.set_focus(true);
    selected(view, engine, "first");
    let old = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &old), Ok(()));
    old.cancel();
    assert!(view.ime_edit_fingerprint(&[7; 32]).is_none());
    wait_ready(view);
    let fresh = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &fresh), Ok(()));
    let stamp = view.ime_edit_fingerprint(&[7; 32]);
    old.cancel();
    Framed::harness_mut(view).relayout();
    assert_eq!(view.ime_edit_fingerprint(&[7; 32]), stamp);
    let receipt = view
        .commit_deferred_text("after acknowledged cancel")
        .unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(
        evaluate(engine, "first.value"),
        "alpha after acknowledged cancel gamma"
    );
    println!("PASS mounted cancel after start ACK: fresh session survives retired cancellation");
}

fn frame_selected<W: Widget + 'static>(
    view: &mut Framed<W>,
    engine: &SharedEngine,
    url: &str,
    target: &str,
) {
    view.set_focus(false);
    load(engine, url);
    view.set_focus(true);
    assert_eq!(
        evaluate(
            engine,
            "(()=>{let f=document.createElement('iframe');f.id='editorFrame';f.src='/frame';document.body.append(f);return true})()"
        ),
        true
    );
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .unwrap();
    while evaluate(
        engine,
        "!!editorFrame.contentDocument?.getElementById('first')",
    ) != true
    {
        assert!(tilo_ui::time::now() < deadline, "owned frame must load");
        pause();
    }
    let selection = if target == "rich" {
        "let r=d.createRange();r.selectNodeContents(e.querySelector('b'));let s=d.getSelection();s.removeAllRanges();s.addRange(r);".to_string()
    } else {
        "e.setSelectionRange(6,10);".to_string()
    };
    assert_eq!(
        evaluate(
            engine,
            &format!(
                "(()=>{{let d=editorFrame.contentDocument,e=d.getElementById({target:?});e.focus();{selection}return d.hasFocus()}})()"
            )
        ),
        true
    );
    view.set_focus(true);
    wait_ready(view);
}

fn qualify_iframes<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    for target in ["first", "notes", "rich"] {
        frame_selected(view, engine, url, target);
        let stamp = view.ime_edit_fingerprint(&[7; 32]);
        let start = view.start_deferred_text().unwrap();
        assert_eq!(finish(view, &start), Ok(()));
        assert_eq!(view.ime_edit_fingerprint(&[7; 32]), stamp);
        let receipt = view.commit_deferred_text("日本語 café العربية").unwrap();
        assert_eq!(finish(view, &receipt), Ok(()));
        let value = if target == "rich" {
            "textContent"
        } else {
            "value"
        };
        let actual = evaluate(
            engine,
            &format!("editorFrame.contentDocument.getElementById({target:?}).{value}"),
        );
        let expected = if target == "rich" {
            "alpha 日本語 café العربية\u{a0}gamma"
        } else {
            "alpha 日本語 café العربية gamma"
        };
        assert_eq!(actual, expected);
        assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
        assert_eq!(
            evaluate(engine, "editorFrame.contentDocument.execCommand('undo')"),
            true
        );
        assert_eq!(
            evaluate(
                engine,
                &format!("editorFrame.contentDocument.getElementById({target:?}).{value}")
            ),
            "alpha beta gamma"
        );
        println!("PASS mounted iframe {target}: original ACK, Unicode replacement and native undo");
    }
    frame_selected(view, engine, url, "first");
    let start = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &start), Ok(()));
    assert_eq!(evaluate(engine, "(editorFrame.remove(),true)"), true);
    let receipt = view.commit_deferred_text("retired frame final").unwrap();
    assert_eq!(finish(view, &receipt), Err(Failure::Rejected));
    assert_eq!(evaluate(engine, "first.value"), "alpha beta gamma");
    frame_selected(view, engine, url, "notes");
    let start = view.start_deferred_text().unwrap();
    assert_eq!(finish(view, &start), Ok(()));
    let receipt = view.commit_deferred_text("fresh frame session").unwrap();
    assert_eq!(finish(view, &receipt), Ok(()));
    assert_eq!(
        evaluate(
            engine,
            "editorFrame.contentDocument.getElementById('notes').value"
        ),
        "alpha fresh frame session gamma"
    );
    println!(
        "PASS mounted iframe removal: rejected original final and independently started recovery session"
    );
}

fn qualify_view<W: Widget + 'static>(view: &mut Framed<W>, engine: &SharedEngine, url: &str) {
    load(engine, url);
    qualify_activation(view, engine, url);
    qualify_iframes(view, engine, url);
    qualify_replacements(view, engine, url);
    qualify_paragraphs(view, engine, url);
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
    assert!(view.ime_content().is_none());
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
    if args.first().is_some_and(|arg| arg == "--native-fixture") {
        assert_eq!(
            args.len(),
            4,
            "--native-fixture HOST PROFILE LOOPBACK_HTTP_URL"
        );
        let url = url::Url::parse(&args[3]).expect("synthetic loopback URL");
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
            "owned Wayland fixture required"
        );
        assert!(
            std::env::var_os("TILO_TEST_CONTROL").is_some(),
            "owned control socket required"
        );
        tilo_ui::App::from_factory(State::new(), || {
            let engine: SharedEngine =
                ChromiumEngine::with_host(Path::new(&args[1]), Path::new(&args[2]))
                    .expect("actual sandboxed Chromium host");
            engine.set_visible(true);
            engine.send(WebViewCommand::Navigate(url.to_string()));
            Box::new(WebView::new(engine))
        })
        .with_window_title("Synthetic dictation qualification")
        .run();
        return;
    }
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
