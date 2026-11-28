//! The sites the person brought their sign-ins of from a browser of theirs, opened signed in in
//! the profile of their own Browser's window.
//!
//! The workspace keeps each as a website sign-in, its cookies in the workspace's vault. A window
//! asks the vault, on its own connection, for those its profile does not hold yet, and hands each
//! to its engine, which sets the cookies in the profile. What the profile holds is written down
//! beside it, by each sign-in's id and digest, once the engine said it took it: a sign-in comes in
//! once, and again only when the person brings it again, so a site the person signed out of
//! stays signed out.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;

use tilo_webview::sessions::SessionCookies;
use tilo_webview::{ServiceSessionReply, ServiceSessionRequest, WebViewCommand};

use crate::PageSession;
use crate::codec;
use crate::logins::VaultAnswer;

/// What the window asks of the person's sign-ins, and what its engine has yet to answer.
pub(crate) struct SignIns {
    /// The file beside the profile that names each sign-in it holds.
    held_at: PathBuf,
    /// The sign-in the engine was handed and has yet to answer: the number it was asked under,
    /// and the name the profile holds it by once it took it. The engine takes one at a time.
    opening: Option<(u32, String)>,
    /// The sign-ins waiting for the engine to answer the one before.
    waiting: VecDeque<(String, SessionCookies)>,
    /// The number the engine was last asked a sign-in under.
    next: u32,
}

impl SignIns {
    /// The sign-ins the profile holds, as `held_at` names them.
    fn held(&self) -> Vec<String> {
        std::fs::read_to_string(&self.held_at)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// What the app is told when the sign-ins did not come in: its note, which it may show.
fn note(why: &str) -> String {
    codec::credential_note("sign_ins", false, why, &[], &[], None, None, false)
}

impl PageSession {
    /// Open this page's profile signed in to the sites the person brought, each sign-in it does
    /// not hold yet, which `held_at` names beside the profile. A private page, and one with no
    /// vault, opens none.
    pub fn open_sign_ins(&mut self, held_at: PathBuf) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        if self.sign_ins.is_some() {
            return;
        }
        let sign_ins = SignIns {
            held_at,
            opening: None,
            waiting: VecDeque::new(),
            next: 0,
        };
        let held = sign_ins.held();
        self.sign_ins = Some(sign_ins);
        let sender = self.vault_answers.0.clone();
        self.vault_pending = self.vault_pending.saturating_add(1);
        vault.website_sign_ins(
            held,
            Box::new(move |result| {
                // A window that closed meanwhile has nobody to hand the sign-ins to.
                drop(sender.send(VaultAnswer::SignIns(result)));
            }),
        );
    }

    /// The vault's answer: each sign-in handed to the engine, or what kept them out.
    pub(crate) fn sign_ins_answered(
        &mut self,
        result: Result<Vec<(String, SessionCookies)>, String>,
    ) {
        let Some(sign_ins) = self.sign_ins.as_mut() else {
            return;
        };
        match result {
            Ok(handed) => {
                sign_ins.waiting.extend(handed);
                self.hand_next_sign_in();
            }
            Err(why) => self.notes.push(note(&why)),
        }
    }

    /// Hand the engine the next sign-in waiting, once it answered the one before.
    fn hand_next_sign_in(&mut self) {
        let Some(sign_ins) = self.sign_ins.as_mut() else {
            return;
        };
        if sign_ins.opening.is_some() {
            return;
        }
        let Some((held, session)) = sign_ins.waiting.pop_front() else {
            return;
        };
        sign_ins.next = sign_ins.next.wrapping_add(1).max(1);
        sign_ins.opening = Some((sign_ins.next, held));
        self.engine.send(WebViewCommand::ImportServiceSession(
            ServiceSessionRequest {
                request_id: sign_ins.next,
                verify_only: false,
                session,
            },
        ));
    }

    /// The engine's answer to a sign-in this session handed it: written down beside the profile
    /// once it took it. `false` where it was no sign-in of this session's.
    pub(crate) fn sign_in_opened(&mut self, reply: &ServiceSessionReply) -> bool {
        let Some(sign_ins) = self.sign_ins.as_mut() else {
            return false;
        };
        let Some((_, held)) = sign_ins
            .opening
            .take_if(|(asked, _)| *asked == reply.request_id)
        else {
            return false;
        };
        if reply.imported {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&sign_ins.held_at)
                .and_then(|mut file| writeln!(file, "{held}"));
            if let Err(error) = written {
                self.notes.push(note(&format!(
                    "The website sign-in came in, but this window could not write down that it \
                     did: {error}"
                )));
            }
        } else {
            self.notes.push(note(
                "This window's profile did not take a website sign-in you brought",
            ));
        }
        self.hand_next_sign_in();
        true
    }

    /// Whether the engine has yet to answer a sign-in it was handed.
    pub(crate) fn sign_ins_opening(&self) -> bool {
        self.sign_ins
            .as_ref()
            .is_some_and(|sign_ins| sign_ins.opening.is_some())
    }
}
