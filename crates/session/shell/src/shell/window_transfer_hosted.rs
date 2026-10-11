//! Carrying a window into or out of a governed workspace, whose records
//! live with its owner rather than in this device's app profiles
//! ([`crate::shell::document_import`]).
//!
//! Out of one, the record is read through the room's bounded committed
//! read, and the running instance marks it moved through its own route.
//! Into one, the document is proposed to the room: nothing opens there and
//! nothing leaves the source until the owner has accepted it and the record
//! they accepted has arrived here. A declined or lapsed proposal leaves the
//! source as it was, and the person is told.
//!
//! As an owner, this desktop's person is asked about each proposal waiting
//! for them, and their signed answer goes to the room's governor.
use std::collections::HashSet;

use tilo_actions::remote::{Pending, PendingPoll};
use tilo_peer_state::document_import::{DocumentImportProposalV1, ImportState};

use super::*;
use crate::shell::document_import::{PROPOSAL_LIFETIME, ProposalStatus};
use crate::shell::portal_dialog::{DocumentImportDecision, DocumentImportRequest};

/// How long past a proposal's lifetime this side waits for the room to say
/// it lapsed before giving up on its own.
const LAPSE_GRACE: Duration = Duration::from_secs(30);
/// How often governed rooms are looked at for proposals to answer.
const CONSENT_SCAN: Duration = Duration::from_secs(1);

/// The source document as last made durable where it lives.
pub(super) fn read_source(
    transfers: &WindowTransfers,
    plan: &Plan,
    origin: &Origin,
    source_root: &Path,
) -> Result<AutoCommit, String> {
    let room = plan.source.room.to_string();
    if plan.source.hosted {
        let (Some(hosted), Origin::Record(checkout)) = (transfers.hosted.as_deref(), origin) else {
            return Err(t("window-transfer-failed-record"));
        };
        let bytes = hosted
            .read(plan.source.room, &plan.app_id, &checkout.id)
            .map_err(|_| t("window-transfer-failed-record"))?;
        let doc = AutoCommit::load(&bytes).map_err(|_| t("window-transfer-failed-record"))?;
        origin
            .validate(&doc, &room)
            .map_err(|error| transfer_reason(&error))?;
        return Ok(doc);
    }
    let source = NativeProfile::open(source_root, Some(&room))
        .map_err(|_| t("window-transfer-failed-storage"))?;
    transfer::read(source.store(), &plan.app_id, source.workspace_id(), origin)
        .map_err(|error| transfer_reason(&error))
}

/// What a proposal into a governed destination starts from.
pub(super) struct Proposal<'a> {
    pub(super) origin: &'a Origin,
    pub(super) snapshot: &'a AutoCommit,
    pub(super) known: Vec<ChangeHash>,
    pub(super) view: &'a SavedInstanceView,
    pub(super) resume: Option<(u16, Vec<u8>)>,
    pub(super) instance: u64,
    pub(super) source_root: PathBuf,
    pub(super) destination_root: PathBuf,
}

/// Where a proposal stands on this side.
pub(super) enum Wait {
    /// Sent; the governor has not recorded it yet.
    Sending(Pending<Result<[u8; 32], String>>),
    /// Recorded under this id; waiting for the owner.
    Recorded([u8; 32]),
}

/// A transfer waiting on a governed destination.
pub(super) struct Waiting {
    pub(super) mode: TransferMode,
    pub(super) tab: TabId,
    pub(super) carried: Box<Carried>,
    pub(super) view: Box<SavedInstanceView>,
    pub(super) resume: Option<(u16, Vec<u8>)>,
    pub(super) wait: Wait,
    pub(super) since: Instant,
}

fn remove_card(st: &mut ChromeState, tab: TabId) {
    let removed = {
        let mut tree = st.tree_mut();
        let removed = crate::shell::tab_binding::remove_tab(&mut tree, tab);
        if removed {
            tree.queue_save();
        }
        removed
    };
    if removed {
        st.bump();
    }
}

/// Propose the document to the destination's owner, with a card there
/// saying what it waits for. Nothing in the source changes.
pub(super) fn propose(
    st: &mut ChromeState,
    transfers: &WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
    proposal: Proposal<'_>,
) -> Option<Phase> {
    let to = plan.destination.name.as_str();
    let Some(hosted) = transfers.hosted.as_deref() else {
        failed(st, plan, &t("window-move-hosted-workspace"));
        return None;
    };
    let target = RecordId::minted();
    let rehomed = transfer::rehome(
        proposal.snapshot,
        proposal.origin,
        &plan.source.room.to_string(),
        &plan.destination.room.to_string(),
        &target,
    );
    let bytes = match rehomed {
        Ok(mut rehomed) => rehomed.save(),
        Err(error) => {
            failed(st, plan, &transfer_reason(&error));
            return None;
        }
    };
    if u64::try_from(bytes.len()).map_or(true, |len| {
        len > tilo_peer_state::document_import::MAX_IMPORT_DOCUMENT_BYTES
    }) {
        failed(
            st,
            plan,
            &t_args("window-transfer-too-large-hosted", &[("to", to.into())]),
        );
        return None;
    }
    let Some(strip) = destination_strip(st, plan.destination.workspace) else {
        failed(st, plan, &t("window-move-unknown-workspace"));
        return None;
    };
    let tab = place_in(
        st,
        plan.destination.workspace,
        strip,
        Tab {
            id: crate::shell::state::next_id(),
            title: plan.title.clone(),
            tile: TileContent::LocalApp {
                app_id: plan.app_id.clone(),
                label: plan.title.clone(),
                note: t_args("window-transfer-awaiting-owner", &[("to", to.into())]),
            },
        },
    );
    let wait = Wait::Sending(hosted.propose(
        plan.destination.room,
        &plan.app_id,
        &target,
        &plan.title,
        &bytes,
    ));
    let carried = Box::new(Carried {
        origin: proposal.origin.clone(),
        target: RecordCheckout::new(target, proposal.origin.kind().clone()),
        known: proposal.known,
        source_root: proposal.source_root,
        destination_root: proposal.destination_root,
        instance: proposal.instance,
        retired_at: None,
    });
    Some(Phase::Proposing {
        mode,
        tab,
        carried,
        view: Box::new(proposal.view.clone()),
        resume: proposal.resume,
        wait,
        since: Instant::now(),
    })
}

/// Look at a waiting proposal again: open the window once the owner has
/// accepted and the record is here, or give up, telling the person why.
pub(super) fn proposing(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    waiting: Waiting,
) -> Option<Phase> {
    let Waiting {
        mode,
        tab,
        carried,
        view,
        resume,
        mut wait,
        since,
    } = waiting;
    let to = plan.destination.name.as_str();
    let give_up = |st: &mut ChromeState, reason: String| {
        remove_card(st, tab);
        failed(st, plan, &reason);
        None
    };
    if tab_tile(st, tab).is_none() {
        // The person closed the waiting card; the proposal lapses unanswered.
        failed(st, plan, &t("window-transfer-failed-closed"));
        return None;
    }
    let Some(hosted) = transfers.hosted.as_deref() else {
        return give_up(st, t("window-move-hosted-workspace"));
    };
    if let Wait::Sending(answer) = &mut wait {
        match answer.poll_ready() {
            PendingPoll::Waiting => {}
            PendingPoll::Ready(Ok(id)) => wait = Wait::Recorded(id),
            PendingPoll::Ready(Err(reason)) => {
                return give_up(
                    st,
                    t_args(
                        "window-transfer-proposal-failed",
                        &[("to", to.into()), ("reason", reason.into())],
                    ),
                );
            }
            PendingPoll::Abandoned => {
                return give_up(
                    st,
                    t_args(
                        "window-transfer-proposal-failed",
                        &[
                            ("to", to.into()),
                            ("reason", t("window-transfer-failed-storage").into()),
                        ],
                    ),
                );
            }
        }
    }
    if let Wait::Recorded(id) = &wait {
        match hosted.proposal(plan.destination.room, id) {
            Some(ProposalStatus {
                state: ImportState::Accepted,
                record_present: true,
            }) => return accepted(st, transfers, plan, mode, tab, carried, &view, resume),
            Some(ProposalStatus {
                state: ImportState::Declined,
                ..
            }) => return give_up(st, t_args("window-transfer-declined", &[("to", to.into())])),
            Some(ProposalStatus {
                state: ImportState::Expired,
                ..
            }) => {
                return give_up(
                    st,
                    t_args("window-transfer-owner-timeout", &[("to", to.into())]),
                );
            }
            _ => {}
        }
    }
    if since.elapsed() > PROPOSAL_LIFETIME.saturating_add(LAPSE_GRACE) {
        return give_up(
            st,
            t_args("window-transfer-owner-timeout", &[("to", to.into())]),
        );
    }
    Some(Phase::Proposing {
        mode,
        tab,
        carried,
        view,
        resume,
        wait,
        since,
    })
}

/// The owner accepted and the record is here: give the instance its
/// presentation in the destination and start it there.
fn accepted(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
    tab: TabId,
    carried: Box<Carried>,
    view: &SavedInstanceView,
    resume: Option<(u16, Vec<u8>)>,
) -> Option<Phase> {
    let presented = std::fs::create_dir_all(&carried.destination_root)
        .ok()
        .and_then(|()| {
            NativeProfile::open(
                &carried.destination_root,
                Some(&plan.destination.room.to_string()),
            )
            .ok()
        })
        .is_some_and(|profile| {
            profile
                .adopt_view(view, carried.instance, &carried.target)
                .is_ok()
        });
    if !presented {
        roll_back(
            st,
            plan,
            Some(tab),
            &carried,
            &t("window-transfer-failed-storage"),
        );
        return None;
    }
    set_note(st, tab, crate::shell::peer_windows::OPENING.to_string());
    start(st, transfers, plan, mode, tab, carried, resume)
}

/// Whether the source changed after the transfer read it.
pub(super) fn changed_since_read(
    transfers: &WindowTransfers,
    plan: &Plan,
    carried: &Carried,
) -> bool {
    // Unreadable is treated as changed: keeping the original is the safe side.
    read_source(transfers, plan, &carried.origin, &carried.source_root).map_or(true, |now| {
        transfer::changes_since(&now, &carried.known) > 0
    })
}

/// A move into a governed room that has to stay a copy: the source changed
/// while the owner decided, so it is not retired.
pub(super) fn kept_as_copy(st: &mut ChromeState, plan: &Plan, carried: &Carried) {
    let window = plan.title.as_str();
    let from = plan.source.name.as_str();
    let to = plan.destination.name.as_str();
    receipt(
        st,
        plan.destination.workspace,
        &plan.app_id,
        &plan.title,
        &carried.target,
        t_args(
            "window-transfer-receipt-copied",
            &[("window", window.into()), ("from", from.into())],
        ),
    );
    st.push_warning_notification(
        t_args(
            "window-transfer-kept-as-copy-title",
            &[("window", window.into())],
        ),
        t_args(
            "window-transfer-kept-as-copy-detail",
            &[("from", from.into()), ("to", to.into())],
        ),
    );
}

/// The source as its old instance left it: up to its retirement stamp.
pub(super) fn read_last(
    transfers: &WindowTransfers,
    plan: &Plan,
    carried: &Carried,
) -> Result<AutoCommit, String> {
    let room = plan.source.room.to_string();
    if !plan.source.hosted {
        let source =
            source_profile(plan, carried).ok_or_else(|| t("window-transfer-failed-storage"))?;
        let read = match &carried.retired_at {
            Some(heads) => transfer::read_until(
                source.store(),
                &plan.app_id,
                source.workspace_id(),
                &carried.origin,
                heads,
            ),
            None => transfer::read(
                source.store(),
                &plan.app_id,
                source.workspace_id(),
                &carried.origin,
            ),
        };
        return read.map_err(|error| transfer_reason(&error));
    }
    let (Some(hosted), Origin::Record(checkout)) = (transfers.hosted.as_deref(), &carried.origin)
    else {
        return Err(t("window-transfer-failed-record"));
    };
    let bytes = hosted
        .read(plan.source.room, &plan.app_id, &checkout.id)
        .map_err(|_| t("window-transfer-failed-record"))?;
    let mut doc = AutoCommit::load(&bytes).map_err(|_| t("window-transfer-failed-record"))?;
    let doc = match &carried.retired_at {
        Some(heads) => doc
            .fork_at(heads)
            .map_err(|_| t("window-transfer-failed-record"))?,
        None => doc,
    };
    carried
        .origin
        .validate(&doc, &room)
        .map_err(|error| transfer_reason(&error))?;
    Ok(doc)
}

pub(super) fn late_edits_stay(st: &mut ChromeState, plan: &Plan) {
    st.push_warning_notification(
        t_args(
            "window-transfer-late-edits-stay-title",
            &[("from", plan.source.name.as_str().into())],
        ),
        t_args(
            "window-transfer-late-edits-stay-detail",
            &[
                ("from", plan.source.name.as_str().into()),
                ("to", plan.destination.name.as_str().into()),
            ],
        ),
    );
}

/// A governed source that could not be marked moved stays live; say so.
pub(super) fn not_retired(st: &mut ChromeState, plan: &Plan) {
    let window = plan.title.as_str();
    st.push_warning_notification(
        t_args(
            "window-transfer-not-retired-title",
            &[
                ("window", window.into()),
                ("from", plan.source.name.as_str().into()),
            ],
        ),
        t_args(
            "window-transfer-not-retired-detail",
            &[("from", plan.source.name.as_str().into())],
        ),
    );
}

/// Proposals this desktop's person is answering as a room's owner.
#[derive(Default)]
pub(super) struct Consent {
    /// Proposals already put to the person this session; each is asked once.
    asked: HashSet<(RoomId, [u8; 32])>,
    open: Vec<Open>,
    scanned: Option<Instant>,
    /// This person owns a governed room, so proposals can arrive for them.
    watching: bool,
}

struct Open {
    room: RoomId,
    proposal: DocumentImportProposalV1,
    answer: Answer,
}

enum Answer {
    Asking(oneshot::Receiver<DocumentImportDecision>),
    Sending(Pending<Result<(), String>>),
}

impl Consent {
    pub(super) fn is_busy(&self) -> bool {
        !self.open.is_empty()
    }

    /// When to look for proposals again; only while this person owns a
    /// governed room.
    pub(super) fn next_scan(&self) -> Option<Instant> {
        self.watching.then(|| {
            self.scanned
                .and_then(|scanned| scanned.checked_add(CONSENT_SCAN))
                .unwrap_or_else(Instant::now)
        })
    }

    pub(super) fn due(&self) -> bool {
        self.scanned
            .is_none_or(|scanned| scanned.elapsed() >= CONSENT_SCAN)
    }
}

fn person_name(st: &ChromeState, room: RoomId, person: Option<&str>) -> String {
    let Some(person) = person else {
        return t("document-import-someone");
    };
    let tree = st.tree();
    let Some(workspace) = tree.desktop.workspaces.iter().find(|ws| ws.room == room) else {
        return t("document-import-someone");
    };
    let me = crate::shell::state::local_user_id_for_room(&room);
    tilo_desktop::overview::room_people(
        workspace,
        &tree.desktop,
        &crate::shell::overview::ShellOverview::for_user(&me),
        tilo_clock::unix_now(),
    )
    .into_iter()
    .find(|found| found.id == person)
    .map_or_else(|| t("document-import-someone"), |found| found.name)
}

/// The sheet asking the owner about `proposal` for workspace `name`.
fn consent_sheet(
    st: &ChromeState,
    room: RoomId,
    name: &str,
    proposal: &DocumentImportProposalV1,
    proposer: Option<&str>,
) -> DocumentImportRequest {
    let person = person_name(st, room, proposer);
    let app = crate::shell::tab_binding::tab_label_for_foreign(&proposal.app);
    DocumentImportRequest {
        handle: format!(
            "document-import:{}",
            tilo_peer_state::document_import::key(&proposal.proposal_id)
        ),
        app_id: proposal.app.clone(),
        title: t_args(
            "document-import-title",
            &[
                ("title", proposal.title.as_str().into()),
                ("to", name.into()),
            ],
        ),
        detail: t_args(
            "document-import-detail",
            &[
                ("person", person.as_str().into()),
                ("title", proposal.title.as_str().into()),
                ("app", app.as_str().into()),
                ("to", name.into()),
            ],
        ),
        accept_label: t_args("document-import-accept", &[("to", name.into())]),
        decline_label: t("document-import-decline"),
        confirm_label: t("document-import-confirm"),
    }
}

/// Ask this desktop's person about proposals waiting for them as owner,
/// and send the answers they give.
pub(super) fn consent(st: &mut ChromeState, transfers: &mut WindowTransfers) {
    let Some(hosted) = transfers.hosted.as_deref() else {
        return;
    };
    if transfers.consent.due() {
        transfers.consent.scanned = Some(Instant::now());
        let rooms: Vec<(RoomId, String)> = {
            let tree = st.tree();
            tree.desktop
                .workspaces
                .iter()
                .filter(|ws| tree.requires_native_owner(ws.room) || hosted.hosts(ws.room))
                .map(|ws| (ws.room, ws.name.clone()))
                .collect()
        };
        transfers.consent.watching = false;
        for (room, name) in rooms {
            let Some(awaiting) = hosted.awaiting_me(room) else {
                continue;
            };
            transfers.consent.watching = true;
            for awaiting in awaiting {
                let proposal = awaiting.proposal;
                if !transfers.consent.asked.insert((room, proposal.proposal_id)) {
                    continue;
                }
                let request =
                    consent_sheet(st, room, &name, &proposal, awaiting.proposer.as_deref());
                let asked = transfers
                    .queue
                    .clone()
                    .unwrap_or_else(PortalDialogQueue::global)
                    .enqueue_document_import(request);
                if let Ok(receiver) = asked {
                    transfers.consent.open.push(Open {
                        room,
                        proposal,
                        answer: Answer::Asking(receiver),
                    });
                }
            }
        }
    }
    for mut open in std::mem::take(&mut transfers.consent.open) {
        if let Answer::Asking(receiver) = &mut open.answer {
            let accepted = match receiver.try_recv() {
                Err(oneshot::error::TryRecvError::Empty) => {
                    transfers.consent.open.push(open);
                    continue;
                }
                Ok(DocumentImportDecision::Accept) => true,
                Ok(DocumentImportDecision::Decline) => false,
                // Unanswered: the proposal lapses on its own.
                Ok(DocumentImportDecision::Dismissed) | Err(_) => continue,
            };
            open.answer = Answer::Sending(hosted.decide(open.room, &open.proposal, accepted));
        }
        let keep = match &mut open.answer {
            Answer::Asking(_) => true,
            Answer::Sending(answer) => match answer.poll_ready() {
                PendingPoll::Waiting => true,
                PendingPoll::Ready(Ok(())) | PendingPoll::Abandoned => false,
                PendingPoll::Ready(Err(reason)) => {
                    st.push_error_notification(
                        t_args(
                            "document-import-answer-failed-title",
                            &[("title", open.proposal.title.as_str().into())],
                        ),
                        reason,
                    );
                    false
                }
            },
        };
        if keep {
            transfers.consent.open.push(open);
        }
    }
}
