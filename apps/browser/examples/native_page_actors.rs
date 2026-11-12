//! Real Chromium and the Browser's registered page actions, used by several
//! actors in one tab: two agents answer one form at once, each under the
//! name the action door admitted it by, and whoever reviews the form reads
//! who set each field and what was done, in order.
//!
//! Run explicitly, against an engine host that is already built, inside the
//! test environment:
//! `cargo build -p browser --example native_page_actors` and then
//! `tools/with-test-environment.sh target/debug/examples/native_page_actors --host PATH`.
//! This is the real Browser: started any other way it would browse with the
//! profile of whoever runs it, so it refuses to. It never disables the
//! engine's sandbox. Its own pages are a fixture on the loopback; the Browser
//! opens its start page first, as it does for a person.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use browser::effects::BrowserEffect;
use browser::resource::BrowserShell;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActionOutput, ActorKind, InvocationId, InvocationUpdate};

type Failure = Box<dyn std::error::Error>;

const FORM: &str = "<!doctype html><title>Apply</title><h1>Apply for a visitor visa</h1>\
<form action='/sent' method='get'>\
<label>Given names <input name='given'></label>\
<label>Family name <input name='family'></label>\
<label>Nationality <select name='nationality' id='nat'><option value=''>Select a country</option>\
<option>Kenya</option><option>Other</option></select></label>\
<div id='other' hidden><label>Which country? <input name='other' required></label></div>\
<label>Passport number <input name='passport'></label>\
<fieldset><legend>Refused before?</legend>\
<label><input type='radio' name='refused' value='yes'> Yes</label>\
<label><input type='radio' name='refused' value='no'> No</label></fieldset>\
<label>Arrival <input type='date' name='arrival'></label>\
<button>Send application</button></form>\
<script>document.getElementById('nat').addEventListener('change', e => \
document.getElementById('other').hidden = e.target.value !== 'Other');</script>";

struct FixtureServer {
    address: std::net::SocketAddr,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0; 4096];
                let Ok(read) = stream.read(&mut request) else {
                    continue;
                };
                let head = String::from_utf8_lossy(request.get(..read).unwrap_or_default());
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                // The page a sent form lands on says what the site received,
                // so the run reads the answers back from the site itself.
                let body = match target.strip_prefix("/sent") {
                    Some(query) => format!(
                        "<!doctype html><title>Sent</title><h1>Application sent</h1><p>{}</p>",
                        query.trim_start_matches('?').replace('&', "&amp;")
                    ),
                    None => FORM.to_owned(),
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Ok(Self {
            address,
            stopped,
            worker: Some(worker),
        })
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A page action that was started and has not been waited for yet.
struct Started {
    action: String,
    invocation: InvocationId,
    answered: Option<ActionOutput>,
}

/// Start `effect` on the page for `invoker`, as the host does for an agent
/// or a collaborator it admitted; nobody, for the person at the window.
fn start(
    app: &mut TestApp<BrowserShell>,
    invoker: Option<&ActionInvoker>,
    effect: &BrowserEffect,
) -> Result<Started, Failure> {
    use ui::tilo_resource::Effect;
    let instance = app
        .harness()
        .component_instance_id()
        .ok_or("missing Browser component")?;
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address: effect.address_on("browser.page"),
    };
    let (invocation, started) = app.harness().start_bound_job_for(
        invoker.cloned(),
        &address,
        &effect.arguments(),
        invoker.is_some(),
    )?;
    Ok(Started {
        action: effect.id().to_owned(),
        invocation,
        answered: started.completed(),
    })
}

/// Wait for a started action's answer.
fn finish(app: &mut TestApp<BrowserShell>, started: Started) -> Result<Value, Failure> {
    if let Some(output) = started.answered {
        return output
            .value
            .ok_or_else(|| format!("{} answered nothing", started.action).into());
    }
    let invocation = started.invocation;
    app.harness()
        .wait_until(ui::time::Duration::from_secs(60), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        });
    for update in app.harness().job_updates(invocation) {
        match update {
            InvocationUpdate::Succeeded { output, .. } => {
                return output
                    .value
                    .clone()
                    .ok_or_else(|| format!("{} answered nothing", started.action).into());
            }
            InvocationUpdate::Failed { error, .. } => {
                return Err(
                    format!("{}: {}: {}", started.action, error.code, error.message).into(),
                );
            }
            _ => {}
        }
    }
    Err(format!("{} did not answer", started.action).into())
}

fn invoke(
    app: &mut TestApp<BrowserShell>,
    invoker: Option<&ActionInvoker>,
    effect: &BrowserEffect,
) -> Result<Value, Failure> {
    let started = start(app, invoker, effect)?;
    finish(app, started)
}

fn check(holds: bool, what: &str, seen: &Value) -> Result<(), Failure> {
    if holds {
        println!("ok: {what}");
        return Ok(());
    }
    Err(format!("{what}: observed {seen}").into())
}

fn answers(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

/// The entries of `answer`'s `others` that `actor` made.
fn told_of(answer: &Value, actor: &str) -> usize {
    answer
        .get("others")
        .and_then(Value::as_array)
        .map_or(0, |others| {
            others
                .iter()
                .filter(|entry| entry.get("actor").and_then(Value::as_str) == Some(actor))
                .count()
        })
}

/// The field of a review read that answers `question`.
fn field<'a>(review: &'a Value, question: &str) -> Option<&'a Value> {
    review
        .pointer("/data/fields")?
        .as_array()?
        .iter()
        .find(|field| field.get("question").and_then(Value::as_str) == Some(question))
}

fn host_argument() -> Result<std::path::PathBuf, Failure> {
    let mut arguments = std::env::args_os().skip(1);
    match (arguments.next(), arguments.next(), arguments.next()) {
        (Some(flag), Some(path), None) if flag == "--host" => {
            Ok(std::path::PathBuf::from(path).canonicalize()?)
        }
        _ => Err("usage: native_page_actors --host PATH".into()),
    }
}

type App = TestApp<BrowserShell>;

/// The person opens the fixture. The Browser opens its own start page as it
/// starts, and a navigation asked for meanwhile is superseded by it or lands
/// under it, so the page is asked for until it is the one the tab shows.
fn open_the_form(app: &mut App, url: &str) -> Result<(), Failure> {
    for _ in 0..8 {
        let _ = invoke(
            app,
            None,
            &BrowserEffect::Navigate {
                url: url.to_owned(),
            },
        );
        let _ = invoke(
            app,
            None,
            &BrowserEffect::WaitPageSettled {
                quiet_ms: Some(200),
                timeout_ms: Some(5000),
                text: None,
                gone: None,
                frame: None,
            },
        );
        let shown = invoke(
            app,
            None,
            &BrowserEffect::ReadPage {
                from: None,
                frame: None,
            },
        )?;
        if shown.get("url").and_then(Value::as_str) == Some(url) {
            println!("ok: the tab shows the fixture");
            return Ok(());
        }
        app.harness()
            .wait_until(ui::time::Duration::from_millis(500), |_| false);
    }
    Err("the Browser never showed the fixture".into())
}

/// The page's form, read as the questions it asks.
fn read_the_form(app: &mut App, ada: &ActionInvoker) -> Result<(), Failure> {
    let forms = invoke(
        app,
        Some(ada),
        &BrowserEffect::ReadPageForms { frame: None },
    )?;
    let form = forms.pointer("/data/forms/0").cloned().unwrap_or_default();
    let asked: Vec<&str> = form
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| field.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    check(
        form.get("form") == Some(&json!("form1"))
            && form.get("label") == Some(&json!("Apply for a visitor visa"))
            && asked
                == [
                    "Given names",
                    "Family name",
                    "Nationality",
                    "Passport number",
                    "Refused before?",
                    "Arrival",
                ],
        "the real page's form is read as its questions through read-page-forms",
        &forms,
    )
}

fn answer(form_answers: &[(&str, Value)], submit: Option<bool>) -> BrowserEffect {
    BrowserEffect::FillPageForm {
        form: "form1".into(),
        answers: answers(form_answers),
        submit,
        submit_button: None,
        cross_origin: None,
        frame: None,
    }
}

/// Two agents answer the one form at once: the second is admitted while the
/// first still holds the tab, and waits its turn.
fn answer_at_once(app: &mut App, ada: &ActionInvoker, ben: &ActionInvoker) -> Result<(), Failure> {
    let first = start(
        app,
        Some(ada),
        &answer(
            &[
                ("Given names", json!("Maria Elena")),
                ("Family name", json!("Okonkwo-Reyes")),
                ("Which country?", json!("Portugal")),
                ("Nationality", json!("Other")),
            ],
            None,
        ),
    )?;
    let second = start(
        app,
        Some(ben),
        &answer(
            &[
                ("Passport number", json!("P7K290XB4")),
                ("Refused before?", json!("No")),
                ("Arrival", json!("2027-02-03")),
            ],
            None,
        ),
    )?;
    let by_ada = finish(app, first)?;
    let by_ben = finish(app, second)?;
    let placed = |answer: &Value| {
        answer
            .pointer("/data/set")
            .and_then(Value::as_array)
            .map(Vec::len)
    };
    check(
        placed(&by_ada) == Some(4)
            && by_ada.pointer("/data/now_asks/0") == Some(&json!("Which country?")),
        "one agent's answers are all placed, the one a choice opened among them",
        &by_ada,
    )?;
    check(
        placed(&by_ben) == Some(3),
        "the other agent's answers are placed in the same tab, after its turn",
        &by_ben,
    )?;
    check(
        by_ada.get("seq") == Some(&json!(1)) && by_ben.get("seq") == Some(&json!(2)),
        "the two actions ran in the order they were asked",
        &json!([by_ada.get("seq"), by_ben.get("seq")]),
    )?;
    check(
        told_of(&by_ben, "agent:ada") == 1 && told_of(&by_ada, "agent:ben") == 0,
        "the second agent's answer says what the first did meanwhile",
        &by_ben,
    )
}

/// Whoever reviews reads every field with who set it, and the tab's record.
fn review(app: &mut App) -> Result<(), Failure> {
    let fields = invoke(app, None, &BrowserEffect::ReadPageFields { frame: None })?;
    let set_by = |question: &str| {
        field(&fields, question)
            .and_then(|field| field.pointer("/set/by"))
            .and_then(Value::as_str)
    };
    let holds = |question: &str| {
        field(&fields, question)
            .and_then(|field| field.get("value"))
            .and_then(Value::as_str)
    };
    check(
        set_by("Given names") == Some("agent:ada")
            && set_by("Which country?") == Some("agent:ada")
            && set_by("Passport number") == Some("agent:ben")
            && set_by("Arrival") == Some("agent:ben")
            && holds("Which country?") == Some("Portugal")
            && holds("Passport number") == Some("P7K290XB4"),
        "the review names the agent that set each field, and what the field holds",
        &fields,
    )?;
    let record = invoke(app, None, &BrowserEffect::ReadPageRecord { since: None })?;
    let entries: Vec<(Option<&str>, Option<&str>)> = record
        .pointer("/data/entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry.get("actor").and_then(Value::as_str),
                        entry.get("action").and_then(Value::as_str),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    check(
        entries
            == [
                (Some("agent:ada"), Some("fill_form")),
                (Some("agent:ben"), Some("fill_form")),
            ],
        "the tab's record holds both agents' actions, in order",
        &record,
    )
}

/// Words are found by line, and a read starts from that line.
fn find_and_read_on(app: &mut App, ben: &ActionInvoker) -> Result<(), Failure> {
    let found = invoke(
        app,
        Some(ben),
        &BrowserEffect::FindPageText {
            text: "refused before".into(),
            frame: None,
            frames: None,
        },
    )?;
    let line = found
        .pointer("/data/matches/0/line")
        .and_then(Value::as_u64)
        .and_then(|line| u32::try_from(line).ok());
    check(
        line.is_some(),
        "find-page-text names the line that holds the words",
        &found,
    )?;
    let onward = invoke(
        app,
        Some(ben),
        &BrowserEffect::ReadPage {
            from: line,
            frame: None,
        },
    )?;
    check(
        onward
            .get("outline")
            .and_then(Value::as_str)
            .and_then(|outline| outline.lines().next())
            .is_some_and(|first| first.contains("Refused before?")),
        "read-page reads on from that line",
        &onward,
    )
}

/// The form is sent by one action, which completes on the page it opened,
/// and the site received what the agents placed.
fn send_the_form(app: &mut App, ada: &ActionInvoker) -> Result<(), Failure> {
    let sent = invoke(app, Some(ada), &answer(&[], Some(true)))?;
    check(
        sent.pointer("/data/submitted") == Some(&json!(true))
            && sent
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|landed| landed.contains("/sent?")),
        "fill-page-form sends the form and completes on the page it opened",
        &sent,
    )?;
    let landed = invoke(
        app,
        None,
        &BrowserEffect::ReadPage {
            from: None,
            frame: None,
        },
    )?;
    let received = landed
        .get("outline")
        .and_then(Value::as_str)
        .unwrap_or_default();
    check(
        [
            "Application sent",
            "given=Maria+Elena",
            "other=Portugal",
            "passport=P7K290XB4",
            "refused=no",
            "arrival=2027-02-03",
        ]
        .iter()
        .all(|said| received.contains(said)),
        "the site received both agents' answers",
        &landed,
    )
}

fn main() -> Result<(), Failure> {
    if std::env::var_os("TILO_TEST_ENVIRONMENT").is_none() {
        return Err(
            "run this inside tools/with-test-environment.sh: it starts the real Browser, which \
             would otherwise use your own profile"
                .into(),
        );
    }
    let host = host_argument()?;
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(ui::webview::chromium::runtime::HOST_OVERRIDE_ENV, &host);
    let server = FixtureServer::start()?;
    let url = format!("http://{}/", server.address);
    let mut app = App::new()?;
    let ada = ActionInvoker::actor("agent:ada").with_kind(ActorKind::Agent);
    let ben = ActionInvoker::actor("agent:ben").with_kind(ActorKind::Agent);
    open_the_form(&mut app, &url)?;
    read_the_form(&mut app, &ada)?;
    answer_at_once(&mut app, &ada, &ben)?;
    review(&mut app)?;
    find_and_read_on(&mut app, &ben)?;
    send_the_form(&mut app, &ada)?;
    println!(
        "PASS: real Chromium, the Browser's registered page actions, two named agents and a reviewer in one tab"
    );
    Ok(())
}
