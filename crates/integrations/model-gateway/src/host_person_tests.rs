//! Which addresses a host dials for its people's providers, and how long
//! one of them may show nothing before its call is let go.
use super::*;

#[test]
fn only_a_public_address_over_tls_is_a_persons_provider() {
    for public in [
        "https://api.anthropic.com",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "https://8.8.8.8/v1",
        "https://1.1.1.1./v1",
        "https://[2606:4700:4700::1111]/v1",
    ] {
        assert!(public_endpoint(public), "{public}");
    }
    for inside in [
        "http://api.example.com/v1",
        "https://localhost/v1",
        "https://models.lan/v1",
        "https://intranet/v1",
        "https://127.0.0.1/v1",
        "https://127.0.0.1./v1",
        "https://2130706433/v1",
        "https://0x7f.1/v1",
        "https://10.1.2.3/v1",
        "https://192.168.1.4/v1",
        // Where a cloud machine reads its own credentials.
        "https://169.254.169.254/latest",
        "https://100.64.0.1/v1",
        "https://0.0.0.0/v1",
        "https://[::1]/v1",
        "https://[::ffff:127.0.0.1]/v1",
        "https://[::ffff:10.0.0.1]/v1",
        "https://[fd00::1]/v1",
        "blob:http://127.0.0.1/x",
        "blob:https://1.1.1.1/x",
        "redis://127.0.0.1/",
        "redis://1.1.1.1/",
        "file://127.0.0.1/etc/passwd",
        "file://1.1.1.1/etc/passwd",
        "not an address",
        "",
    ] {
        assert!(!public_endpoint(inside), "{inside}");
    }
}

/// A name is checked again when it resolves: one that leads to this machine
/// is not dialled, whatever it is called.
#[tokio::test(flavor = "current_thread")]
async fn a_name_that_resolves_inside_is_not_dialled() {
    use reqwest::dns::Resolve as _;
    let name: reqwest::dns::Name = "localhost".parse().unwrap();
    assert!(PublicNames.resolve(name).await.is_err());
}

/// One hundredth of the times a hosted turn is held to: its provider may
/// show nothing for 750 ms (75 s there).
const QUIET: Duration = Duration::from_millis(750);
/// One hundredth of what a call that shows life has in all: 6 s (600 s).
const WHOLE: Duration = Duration::from_secs(6);
/// Half of it: a call let go for silence ends long before this.
const HALF: Duration = Duration::from_secs(3);

/// How a provider begins a stream.
const HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
/// An answer in words as an OpenAI-compatible provider streams it.
const ANSWER: &str = concat!(
    "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Now.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":1,\"total_tokens\":11}}\n\n",
    "data: [DONE]\n\n",
);

/// Ask the stand-in provider at `endpoint` once as a person's own, on this
/// machine, with a made-up key: the answer or the failure, and how long the
/// call took. `quiet` is how long it may show nothing, `whole` the call's
/// own end.
async fn ask(
    endpoint: &str,
    quiet: Option<Duration>,
    whole: Duration,
) -> (Result<String, neutral::ModelError>, Duration) {
    let mut profile =
        ProviderProfile::hosted(agent_provider_config::ProviderProtocol::OpenAiCompatible);
    profile.endpoint = format!("{endpoint}/v1");
    let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 64);
    let began = Instant::now();
    let answer = PersonProviders::new()
        .unwrap()
        .complete(
            PersonTurn {
                profile: &profile,
                model: "m",
                key: Some("made-up-test-key"),
                request: &request,
                timeout: whole,
                private_addresses: true,
                quiet,
            },
            &mut |_| {},
        )
        .await
        .map(|answer| {
            answer
                .message
                .content
                .iter()
                .filter_map(|part| match part {
                    neutral::Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        });
    (answer, began.elapsed())
}

/// What a call let go for silence ends as: timed out, saying how long the
/// provider sent nothing, after that long and long before the call's own
/// end; and the provider was asked once, since it may have taken the call.
fn assert_let_go(
    (answer, took): (Result<String, neutral::ModelError>, Duration),
    responder: &test_http::Responder,
) {
    let error = answer.unwrap_err();
    assert_eq!(error.kind, neutral::ErrorKind::TimedOut);
    assert_eq!(
        error.message,
        "The model's provider sent nothing for 0.75 s"
    );
    assert!(took >= QUIET && took < HALF, "{took:?}");
    assert_eq!(responder.drain_requests().len(), 1);
}

/// A provider that takes the request and sends nothing at all, not even its
/// headers, is let go when it has been silent for as long as it may.
#[tokio::test(flavor = "current_thread")]
async fn a_provider_that_sends_nothing_at_all_is_let_go_after_its_silence() {
    let responder = test_http::Responder::serve(|peer| {
        let _closed = peer.wait_for_close();
    });
    assert_let_go(ask(&responder.url(), Some(QUIET), WHOLE).await, &responder);
}

/// A provider that answers its headers and then sends no byte is let go the
/// same: headers alone are no work.
#[tokio::test(flavor = "current_thread")]
async fn a_provider_that_sends_its_headers_and_then_nothing_is_let_go_after_its_silence() {
    let responder = test_http::Responder::serve(|peer| {
        let _ = peer.write(HEAD);
        let _closed = peer.wait_for_close();
    });
    assert_let_go(ask(&responder.url(), Some(QUIET), WHOLE).await, &responder);
}

/// A provider that shows life is given its time. This one sends a
/// keep-alive comment every 100 ms (every ten seconds, at the scale of
/// these tests) and its answer after 2.4 s (four minutes): more than three
/// times as long as it may be silent, and it is answered.
#[tokio::test(flavor = "current_thread")]
async fn a_provider_that_keeps_its_call_alive_is_given_longer_than_any_silence() {
    let responder = test_http::Responder::serve(|peer| {
        let _ = peer.write(HEAD);
        for _ in 0..24 {
            std::thread::sleep(Duration::from_millis(100));
            let _ = peer.write(b": keep-alive\n\n");
        }
        let _ = peer.write(ANSWER.as_bytes());
    });
    let (answer, took) = ask(&responder.url(), Some(QUIET), WHOLE).await;
    assert_eq!(answer.unwrap(), "Now.");
    assert!(took >= Duration::from_millis(2400), "{took:?}");
    assert_eq!(responder.drain_requests().len(), 1);
}

/// A provider that refuses once and asks to be left alone for a second,
/// longer than it may be silent, is asked again after that second and
/// answers: a wait the provider asked for is not its silence.
#[tokio::test(flavor = "current_thread")]
async fn a_wait_the_provider_asked_for_is_not_counted_as_its_silence() {
    let responder = test_http::Responder::start(vec![
        test_http::Reply::status(503).header("Retry-After", "1"),
        test_http::Reply::status(200)
            .header("Content-Type", "text/event-stream")
            .body(ANSWER),
    ]);
    let (answer, took) = ask(&responder.url(), Some(QUIET), Duration::from_secs(20)).await;
    assert_eq!(answer.unwrap(), "Now.");
    assert!(took >= Duration::from_secs(1), "{took:?}");
    assert_eq!(responder.drain_requests().len(), 2);
}

/// A call that names no such time is held to its own end alone, as every
/// call was: a silent provider keeps it until then.
#[tokio::test(flavor = "current_thread")]
async fn a_call_that_names_no_silence_waits_for_its_own_end() {
    let responder = test_http::Responder::serve(|peer| {
        let _closed = peer.wait_for_close();
    });
    let whole = Duration::from_millis(1200);
    let (answer, took) = ask(&responder.url(), None, whole).await;
    assert_eq!(
        answer.unwrap_err(),
        neutral::ModelError::new(neutral::ErrorKind::TimedOut)
    );
    assert!(took >= whole, "{took:?}");
}
