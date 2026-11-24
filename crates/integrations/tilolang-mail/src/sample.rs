//! What the mail server of a sample workspace holds: one account a working week in, its
//! conversations to read, some answered, one flagged, a few archived, sent and junk.
//!
//! An account and its mail are what its server says, which no action of the app writes: a
//! development session's seed and a staged window take them in through the mail origin
//! ([`ImapOrigin`]) over a connector that answers from this fixture instead of a server. So the account, its mailboxes, its conversations and what they add up to are
//! written by the same conversion a real server's answer goes through, as the service's change.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin as _, OriginCx};

use crate::connector::{
    AccountRef, Changes, Event, ImapMessage, MailConnector, Outgoing, Request, Unmade,
};
use crate::origin::ImapOrigin;

/// The sample account, by the id the workspace's settings give it.
pub const ACCOUNT: &str = "sam@rivera.example";

/// How many polls taking the sample in may take: the account and its mailboxes on the first,
/// each mailbox's mail on the second.
const POLLS: usize = 8;

/// Take the sample account and its mail into the package `app` of `host`, as of `now`: what its
/// server holds, written by the mail origin as a real server's answer is.
///
/// # Errors
/// The package keeps no mail account (no `imap` types), or the workspace did not take all of
/// what the server said.
pub fn take_in(host: &Host, app: &str, now: &str) -> Result<(), String> {
    let woken = Arc::new(AtomicBool::new(true));
    let server = SampleServer {
        woken: Arc::clone(&woken),
        events: Vec::new(),
    };
    let mut origin = ImapOrigin::new(Box::new(server), woken);
    let waker: tilolang_tilo::background::Waker = Arc::new(|| {});
    let cx = OriginCx {
        host,
        app,
        waker: &waker,
        now: now.to_owned(),
    };
    // Polled as an instance's frames poll it, until it has nothing left to do.
    for _ in 0..POLLS {
        if !origin.due() {
            break;
        }
        origin.poll(&cx);
    }
    if origin.due() {
        return Err(format!(
            "the mail was still being taken in after {POLLS} polls"
        ));
    }
    match origin.not_taken_in() {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// A connector that answers from the sample: its one account, and each mailbox's mail when the
/// origin asks for it. It holds nothing else, so nothing a person does is ever sent anywhere.
struct SampleServer {
    /// Set when it has an answer: the origin polls again.
    woken: Arc<AtomicBool>,
    events: Vec<Event>,
}

impl MailConnector for SampleServer {
    fn accounts(&self) -> Vec<AccountRef> {
        vec![AccountRef {
            id: ACCOUNT.to_owned(),
            address: ACCOUNT.to_owned(),
            name: "Sam Rivera".to_owned(),
            identities: Vec::new(),
        }]
    }

    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request> {
        if account != ACCOUNT {
            return None;
        }
        self.events.push(Event::Changes(Changes {
            account: ACCOUNT.to_owned(),
            mailbox: mailbox.to_owned(),
            messages: mail()
                .into_iter()
                .filter(|message| message.mailbox == mailbox)
                .collect(),
            flags: Vec::new(),
            removed: Vec::new(),
            more: None,
            // Read for the first time: the mail is what the folder holds, not news that came.
            reset: true,
        }));
        self.woken.store(true, Ordering::Release);
        Some(uuid::Uuid::new_v4())
    }

    fn older(&mut self, _account: &str, _mailbox: &str) -> Option<Request> {
        None
    }

    fn flag(
        &mut self,
        _account: &str,
        _key: &str,
        _unread: bool,
        _flagged: bool,
    ) -> Option<Request> {
        None
    }

    fn move_to(&mut self, _account: &str, _key: &str, _mailbox: &str) -> Option<Request> {
        None
    }

    fn purge(&mut self, _account: &str, _key: &str) -> Option<Request> {
        None
    }

    fn save_draft(&mut self, _account: &str, _draft: Outgoing) -> Option<Request> {
        None
    }

    fn search(&mut self, _account: &str, _text: &str) -> Option<Request> {
        None
    }

    fn send(&mut self, _account: &str, _message: Outgoing) -> Result<Request, Unmade> {
        Err(Unmade::NoAccount)
    }

    fn watch(&mut self, _account: &str) {}

    fn events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}

/// One message of the sample, as its server holds it: `from` is `(name, address)`, `received`
/// seconds since the epoch.
struct Said {
    key: &'static str,
    thread: &'static str,
    subject: &'static str,
    from: (&'static str, &'static str),
    to: &'static str,
    received: u64,
    body: &'static str,
    references: &'static str,
    unread: bool,
    flagged: bool,
    mailbox: &'static str,
}

impl Said {
    fn message(&self) -> ImapMessage {
        ImapMessage {
            key: self.key.to_owned(),
            thread: self.thread.to_owned(),
            subject: self.subject.to_owned(),
            from: self.from.1.to_owned(),
            from_name: self.from.0.to_owned(),
            to: self.to.to_owned(),
            cc: String::new(),
            received: self.received,
            body: self.body.to_owned(),
            html: String::new(),
            message_id: self.key.to_owned(),
            references: self.references.to_owned(),
            attachments: Vec::new(),
            unread: self.unread,
            flagged: self.flagged,
            mailbox: self.mailbox.to_owned(),
        }
    }
}

/// Every message the sample account's server holds, each in its mailbox. A conversation's first
/// message comes first: it is what the conversation is called by.
fn mail() -> Vec<ImapMessage> {
    SAID.iter().map(Said::message).collect()
}

const SAID: &[Said] = &[
    // 2026-06-30T08:20:00Z
    Said {
        key: "launch@rivera.example",
        thread: "launch@rivera.example",
        subject: "Launch plan for the autumn release",
        from: ("Grace Hopper", "grace@hopper.example"),
        to: "sam@rivera.example, ada@analytical.example",
        received: 1_782_807_600,
        body: "Hi both,\n\nHere is the draft plan for the autumn release. The beta goes out on the fifth, the release on the twelfth if nothing turns up.\n\nCould you each look at your part by Wednesday?\n\nGrace",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-30T09:40:00Z
    Said {
        key: "launch-2@rivera.example",
        thread: "launch@rivera.example",
        subject: "Re: Launch plan for the autumn release",
        from: ("Sam Rivera", "sam@rivera.example"),
        to: "grace@hopper.example, ada@analytical.example",
        received: 1_782_812_400,
        body: "The fifth works for the beta. I would move the release to the fourteenth so the docs can catch up.\n\nSam",
        references: "launch@rivera.example",
        unread: false,
        flagged: false,
        mailbox: "sent",
    },
    // 2026-06-30T10:34:00Z
    Said {
        key: "launch-3@rivera.example",
        thread: "launch@rivera.example",
        subject: "Re: Launch plan for the autumn release",
        from: ("Ada Lovelace", "ada@analytical.example"),
        to: "sam@rivera.example, grace@hopper.example",
        received: 1_782_815_640,
        body: "Agreed on the twelfth. I will tell support today so they can prepare the help pages.\n\nAda",
        references: "launch@rivera.example launch-2@rivera.example",
        unread: true,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-30T10:25:00Z
    Said {
        key: "lease@lettings.example",
        thread: "lease@lettings.example",
        subject: "Your lease renewal",
        from: ("Mill Lane Lettings", "office@lettings.example"),
        to: "sam@rivera.example",
        received: 1_782_815_100,
        body: "Dear Sam,\n\nThe new lease is attached. Please sign by Friday so we can register it before the month ends.\n\nKind regards,\nMill Lane Lettings",
        references: "",
        unread: true,
        flagged: true,
        mailbox: "inbox",
    },
    // 2026-06-30T08:51:00Z
    Said {
        key: "dinner@bo.example",
        thread: "dinner@bo.example",
        subject: "Dinner on Friday?",
        from: ("Bo Chen", "bo@chen.example"),
        to: "sam@rivera.example",
        received: 1_782_809_460,
        body: "Shall we try the new place on Mill Lane? I can book for eight.\n\nBo",
        references: "",
        unread: true,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-29T17:05:00Z
    Said {
        key: "review@code.example",
        thread: "review@code.example",
        subject: "Review requested: importer handles merged cells",
        from: ("Forge", "notifications@code.example"),
        to: "sam@rivera.example",
        received: 1_782_752_700,
        body: "Chen asked you to review the change to the table importer: merged cells are kept, and the column widths follow the source.",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-29T09:30:00Z
    Said {
        key: "library@city.example",
        thread: "library@city.example",
        subject: "Two books due next week",
        from: ("City Library", "loans@city.example"),
        to: "sam@rivera.example",
        received: 1_782_725_400,
        body: "Piranesi and The Dispossessed are due on the seventh. You can renew them twice more.",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-28T19:44:00Z
    Said {
        key: "photos@mia.example",
        thread: "photos@mia.example",
        subject: "Photos from the lake",
        from: ("Mia Novak", "mia@novak.example"),
        to: "sam@rivera.example",
        received: 1_782_675_840,
        body: "The sunset ones came out best. I put the rest in the shared album.\n\nMia",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "inbox",
    },
    // 2026-06-27T08:00:00Z
    Said {
        key: "invoice@studio.example",
        thread: "invoice@studio.example",
        subject: "Invoice for June",
        from: ("North Studio", "accounts@studio.example"),
        to: "sam@rivera.example",
        received: 1_782_547_200,
        body: "Thanks for the work this month. The invoice is attached, payable within thirty days.",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "archive",
    },
    // 2026-06-26T03:12:00Z
    Said {
        key: "offer@deals.example",
        thread: "offer@deals.example",
        subject: "You have won a cruise",
        from: ("Prize Draw", "win@deals.example"),
        to: "sam@rivera.example",
        received: 1_782_443_520,
        body: "Claim your prize within 24 hours by confirming your card details.",
        references: "",
        unread: false,
        flagged: false,
        mailbox: "junk",
    },
];

/// Have a sample workspace's mail come from [`take_in`] (`tilolang_tilo::origin::take_in_samples`).
pub fn register() {
    tilolang_tilo::origin::register_sample("imap", SAMPLE);
}

/// What [`register`] hands the platform.
const SAMPLE: &tilolang_tilo::origin::Sample = &take_in;
