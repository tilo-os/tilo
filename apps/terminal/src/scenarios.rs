//! Screenshot scenarios: the output a scenario's process printed, and the view state shown over it.

use std::fmt::Write as _;

use crate::target::{EnvironmentSummary, SessionTarget};

pub(crate) const SCENARIOS: &[&str] = &[
    "shell",
    "shell-select",
    "colors",
    "tui",
    "scrollback",
    "find",
    "empty",
    "empty-menu",
    "ended",
    "targets",
    "environments",
    "host",
    "held",
];

pub(crate) fn target(name: &str) -> Option<(SessionTarget, Vec<EnvironmentSummary>)> {
    match name {
        "host" => Some((SessionTarget::Host, environments())),
        "targets" | "environments" => Some((SessionTarget::environment("work"), environments())),
        _ => None,
    }
}

pub(crate) fn held(key: &str) -> Vec<u8> {
    use base64::Engine as _;
    use ui::tilo_actions::remote::host_account::{NextStep, NextStepKind, Reason, Refusal};
    let b64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let message = "This needs a Linux machine. Tilo Cloud runs Linux machines for accounts with a \
                   card on file, and this account has none yet. Add a card to continue.";
    let refusal = Refusal {
        reason: Reason::NoPaymentMethod,
        message: message.into(),
        next: vec![NextStep {
            kind: NextStepKind::AddPaymentMethod,
            url: Some("https://tilo.sh/billing/card?account=acct-ada".into()),
        }],
    };
    let refusal = serde_json::to_string(&refusal).unwrap_or_default();
    let prompt =
        format!("\x1b]133;A;k={key}\x07\x1b[1;32mada@build-box\x1b[0m:\x1b[1;34m~/site\x1b[0m$ ");
    format!(
        "{prompt}cargo test\r\n\
         \x1b]133;C;k={key};cmd={};cwd={};t=1\x07\
         \x1b]133;W;k={key};a=0;r={}\x07\
         ush: cargo: needs a Linux machine. Waiting for the card; Ctrl+C cancels.\r\n",
        b64("cargo test"),
        b64("/home/ada/site"),
        b64(&refusal),
    )
    .into_bytes()
}

pub(crate) fn output(name: &str) -> Vec<u8> {
    match name {
        "shell" | "shell-select" => shell(),
        "colors" => colors(),
        "tui" => tui(),
        "scrollback" => numbered_log(|i| {
            format!(
                "\x1b[38;5;250m{i:>4}\x1b[0m  log entry {i:03}: the quick brown fox jumps over the lazy dog"
            )
        }),
        "find" => numbered_log(|i| format!("{i:>4}  build step {i:03}: compiling terminal search")),
        "host" => b"you@laptop:~$ podman ps\r\nCONTAINER ID  IMAGE  COMMAND  CREATED  STATUS\r\nyou@laptop:~$ "
            .to_vec(),
        "targets" | "environments" => {
            b"[work ~]$ pandoc --version | head -1\r\npandoc 3.1.11\r\n[work ~]$ ".to_vec()
        }
        _ => Vec::new(),
    }
}

fn shell() -> Vec<u8> {
    let prompt = "\x1b[1;32myou@tilo\x1b[0m:\x1b[1;34m~\x1b[0m$ ";
    let mut s = String::new();
    let _ = write!(
        s,
        "{prompt}printf 'Tilo terminal - live PTY session\\n'\r\n\
         Tilo terminal - live PTY session\r\n\
         {prompt}printf 'primes below 20: 2 3 5 7 11 13 17 19\\n'\r\n\
         primes below 20: 2 3 5 7 11 13 17 19\r\n\
         {prompt}printf '\\033[1;32mdone\\033[0m\\n'\r\n\
         \x1b[1;32mdone\x1b[0m\r\n\
         {prompt}"
    );
    s.into_bytes()
}

fn colors() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("\x1b[1mTilo terminal — SGR palette\x1b[0m\r\n\r\n");
    s.push_str(" 16-color ANSI:  ");
    for n in 0..8 {
        let _ = write!(s, "\x1b[4{n}m  \x1b[0m");
    }
    s.push(' ');
    for n in 0..8 {
        let _ = write!(s, "\x1b[48;5;{}m  \x1b[0m", n + 8);
    }
    s.push_str("\r\n\r\n 256-color:      ");
    for n in (16..232).step_by(6) {
        let _ = write!(s, "\x1b[48;5;{n}m \x1b[0m");
    }
    s.push_str("\r\n\r\n truecolor:      ");
    for i in 0..48 {
        let r = (i * 255 / 47) as u8;
        let g = (255 - i * 255 / 47) as u8;
        let _ = write!(s, "\x1b[48;2;{r};{g};128m \x1b[0m");
    }
    s.push_str("\r\n\r\n styles:         ");
    s.push_str("\x1b[1mbold\x1b[0m  \x1b[2mdim\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[9mstrike\x1b[0m  \x1b[7mreverse\x1b[0m\r\n");
    s.push_str("\r\n foreground:     ");
    s.push_str(
        "\x1b[38;2;255;90;90mred \x1b[38;2;90;220;120mgreen \x1b[38;2;110;160;255mblue\x1b[0m\r\n",
    );
    s.push_str("\r\n$ \x1b[38;5;120mecho\x1b[0m \x1b[38;5;223m\"hello, Tilo\"\x1b[0m\r\n");
    s.into_bytes()
}

fn tui() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("\x1b[?1049h\x1b[2J\x1b[H");
    s.push_str("\x1b[1;1H\x1b[7m Tilo EDIT  main.rs  [+]                                      unix  utf-8 \x1b[0m");
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "demo source text shown verbatim in the tui shot; the braces are \
                  the sample code's own println! syntax, not a format template here"
    )]
    let lines = [
        "fn start() {",
        "    // alternate-screen redraw via CSI ?1049h",
        "    let greeting = \"hello, Tilo\";",
        "    for i in 0..3 {",
        "        println!(\"{i}: {greeting}\");",
        "    }",
        "}",
    ];
    for (i, line) in lines.iter().enumerate() {
        let _ = write!(
            s,
            "\x1b[{};1H\x1b[38;5;240m{:>3} \x1b[0m{}",
            i + 3,
            i + 1,
            line
        );
    }
    s.push_str("\x1b[23;1H\x1b[7m NORMAL                                              3,17          Top \x1b[0m");
    s.push_str("\x1b[24;1H:wq");
    s.into_bytes()
}

fn numbered_log(line: impl Fn(usize) -> String) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..80 {
        s.push_str(&line(i));
        s.push_str("\r\n");
    }
    s.into_bytes()
}

fn environments() -> Vec<EnvironmentSummary> {
    vec![
        EnvironmentSummary {
            key: "default".into(),
            name: "Default".into(),
            tools: vec!["git".into(), "ripgrep".into()],
            grants: vec![],
        },
        EnvironmentSummary {
            key: "work".into(),
            name: "Work".into(),
            tools: vec!["pandoc".into(), "python3".into(), "jq".into()],
            grants: vec![
                ("~/src".into(), true),
                ("~/Documents".into(), false),
                ("~/Projects/website".into(), true),
                ("~/Projects/thesis/figures".into(), true),
                ("~/Downloads/datasets".into(), false),
            ],
        },
    ]
}
