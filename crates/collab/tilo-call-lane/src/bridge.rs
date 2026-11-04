//! The lane member that carries a page's pictures into a call.
//!
//! A page speaks a call in sealed frames but has no endpoint to carry them
//! on. Its own device has one, and it is that device whose key the page's
//! frames name and whose membership the workspace's roster admits, so this is
//! the software that takes part in the call for the page: it joins the call's
//! picture lane as that device, dials the call's other devices and answers the
//! ones that dial it, and copies pictures between them and the page. The
//! page's voice is not here: it publishes on the workspace's live plane,
//! which a page reaches over WebTransport itself.
//!
//! Nothing here touches a key: the frames the page hands over are sealed, the
//! frames that arrive are copied back exactly as they came off the lane, and
//! the page opens and seals them itself. A call the page is bridged into is
//! therefore as end-to-end encrypted as one between two devices, and this
//! member never sees a picture of it.
//!
//! A bridge is one turn at a time ([`Bridge::exchange`]): the page's host
//! hands it what the page sent, and gets back what the call's devices sent
//! since. That is the shape a page's session carries, so a bridge holds
//! nothing longer than a turn's worth of media.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use iroh::{Endpoint, EndpointAddr};
use tilo_lock::MutexRecover as _;

use crate::carrier::{Calls, Failed, Hello, Peer};
use crate::frame::Lane;
use crate::page::{self, Asked, CarriedPicture, Down, Showing, Up};
use crate::roster::Roster;
use crate::video::{Control, PicturePeer};
use crate::wire::source_wire;

/// Pictures held for a page that has not come for them yet. Past it the oldest
/// goes, as a call drops what it can no longer play.
const MAX_FRAMES: usize = 64;

/// Why a turn did not go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The turn is for another call than the one this bridge carries.
    #[error("this bridge carries another call")]
    OtherCall,
    /// The bridge was closed: the call it carried is over.
    #[error("the bridge is closed")]
    Closed,
}

/// A device's call, carrying a page's pictures: what the page sends, the lane
/// it goes on, and what came back for it.
pub struct Bridge {
    workspace: String,
    call: String,
    /// The device the page's frames name, and that this bridge takes part as.
    device: [u8; 32],
    /// The lane the page's pictures go on, and the handler the endpoint
    /// answers the call's connections with.
    video: Calls,
    held: Mutex<Held>,
    closed: AtomicBool,
}

/// What a bridge holds of the call between two turns.
#[derive(Default)]
struct Held {
    pictures: BTreeMap<[u8; 32], PicturePeer>,
    /// What each device says it shows of the page, as its control stream last
    /// said: the layers the page encodes for.
    showing: BTreeMap<[u8; 32], Showing>,
    /// What the devices sent for the page to open, oldest first.
    frames: VecDeque<CarriedPicture>,
    /// Sources a device asked the page to start over at a keyframe.
    asked: Vec<Asked>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("call", &self.call)
            .field("device", &self.device)
            .field("pictures", &self.held.lock_recover().pictures.len())
            .finish_non_exhaustive()
    }
}

impl Bridge {
    /// Carry a page into `call` of the workspace `workspace`, as `device`, for
    /// the devices `roster` admits.
    ///
    /// The endpoint's router must answer the call's protocol with
    /// [`Self::handler`] and use [`crate::carrier::transport_config`]; the
    /// caller opens lanes with [`Self::dial`] to the devices the call has.
    /// Only a lane's frames are read or written: the page's keys stay with the
    /// page.
    ///
    /// # Panics
    /// Called outside a tokio runtime, which the lanes' readers run on.
    #[must_use]
    pub fn new(
        workspace: String,
        channel: collab_types::ChannelId,
        call: String,
        device: [u8; 32],
        roster: Roster,
    ) -> Arc<Self> {
        let (video, accepted) = Calls::new(
            Hello {
                workspace: workspace.clone(),
                channel,
                call: call.clone(),
                lane: Lane::Video,
            },
            roster,
        );
        let bridge = Arc::new(Self {
            workspace,
            call,
            device,
            video,
            held: Mutex::new(Held::default()),
            closed: AtomicBool::new(false),
        });
        let answering = Arc::downgrade(&bridge);
        tokio::spawn(async move {
            let mut accepted = accepted;
            while let Some(peer) = accepted.recv().await {
                let Some(bridge) = answering.upgrade() else {
                    peer.close();
                    return;
                };
                if bridge.closed.load(Ordering::Acquire) {
                    peer.close();
                    return;
                }
                bridge.watch(peer);
            }
        });
        bridge
    }

    /// The handler an endpoint's router answers the call's connections with.
    #[must_use]
    pub fn handler(&self) -> Calls {
        self.video.clone()
    }

    /// The device this bridge takes part as: the one the page's frames name.
    #[must_use]
    pub const fn device(&self) -> [u8; 32] {
        self.device
    }

    /// The workspace and the call this bridge carries.
    #[must_use]
    pub fn call(&self) -> (&str, &str) {
        (&self.workspace, &self.call)
    }

    /// Who may send to this bridge from now on, as the workspace's governance
    /// moves it. Lanes already open are the caller's to close.
    pub fn set_roster(&self, roster: Roster) {
        self.video.set_roster(roster);
    }

    /// Open the call's lane to `device`, and take what it sends from now on.
    ///
    /// # Errors
    ///
    /// As [`Calls::dial`]: the device refused the call, or did not answer.
    pub async fn dial(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), Failed> {
        let peer = self.video.dial(endpoint, addr.into()).await?;
        if self.closed.load(Ordering::Acquire) {
            peer.close();
            return Err(Failed::Ended("the bridge closed".to_owned()));
        }
        self.watch(peer);
        Ok(())
    }

    /// Take one turn with the page: send what it sealed, and hand back what
    /// the call's devices sent since the last turn.
    ///
    /// # Errors
    ///
    /// [`Refused::OtherCall`] for a turn that names another call, and
    /// [`Refused::Closed`] once the bridge is closed.
    pub async fn exchange(&self, up: Up) -> Result<Down, Refused> {
        if up.workspace != self.workspace || up.call != self.call {
            return Err(Refused::OtherCall);
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(Refused::Closed);
        }
        let pictures = self
            .held
            .lock_recover()
            .pictures
            .values()
            .cloned()
            .collect::<Vec<_>>();
        // What the page shows of a device is what that device sends it, so the
        // device is told before the layers it is sent start arriving.
        for showing in &up.shows {
            if let Some(peer) = pictures.iter().find(|peer| peer.remote() == showing.device) {
                peer.send_shows(
                    &page::layers(&showing.camera),
                    &page::layers(&showing.screen),
                );
            }
        }
        for sealed in &up.pictures {
            let (Some(source), Some(layer)) = (sealed.source(), sealed.layer()) else {
                continue;
            };
            for peer in &pictures {
                peer.send_picture(source, layer, sealed.keyframe, &sealed.sealed)
                    .await;
            }
        }
        for asked in &up.keyframes {
            if let Some(source) = crate::wire::source_of_wire(asked.source)
                && let Some(peer) = pictures.iter().find(|peer| peer.remote() == asked.device)
            {
                peer.ask_keyframe(source);
            }
        }
        let mut held = self.held.lock_recover();
        Ok(Down {
            devices: held.pictures.keys().copied().collect(),
            pictures: held.frames.drain(..).collect(),
            shows: held.showing.values().cloned().collect(),
            keyframes: std::mem::take(&mut held.asked),
        })
    }

    /// End every lane of the call: what a page that left the call does.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut held = self.held.lock_recover();
        for peer in held.pictures.values() {
            peer.close();
        }
        held.pictures.clear();
        held.frames.clear();
    }

    /// Take what the device on `peer`'s picture lane sends, until the lane
    /// ends: sealed pictures off its streams, and what it says on its control
    /// streams.
    fn watch(self: &Arc<Self>, peer: Peer) {
        let from = peer.remote();
        let lane = PicturePeer::new(peer);
        let replaced = { self.held.lock_recover().pictures.insert(from, lane.clone()) };
        if let Some(old) = replaced {
            old.close();
        }
        if let Some(controls) = lane.take_controls() {
            let writing = lane.clone();
            tokio::spawn(async move { writing.write_controls(controls).await });
        }
        let reading = lane.clone();
        let arriving = Arc::downgrade(self);
        let cut_off = Arc::downgrade(self);
        tokio::spawn(async move {
            reading
                .read_pictures_with(
                    move |source, sealed| {
                        let Some(bridge) = arriving.upgrade() else {
                            return;
                        };
                        let mut held = bridge.held.lock_recover();
                        held.frames.push_back(CarriedPicture {
                            from,
                            sealed,
                            source: source_wire(source),
                        });
                        while held.frames.len() > MAX_FRAMES {
                            held.frames.pop_front();
                        }
                        drop(held);
                    },
                    move |source| {
                        let Some(bridge) = cut_off.upgrade() else {
                            return;
                        };
                        let mut held = bridge.held.lock_recover();
                        // Nothing of the stream before the cut can be opened,
                        // so what the page was holding of it goes, and the
                        // device is asked to start over.
                        held.frames.retain(|frame| {
                            frame.from != from || frame.source != source_wire(source)
                        });
                        if let Some(peer) = held.pictures.get(&from) {
                            peer.ask_keyframe(source);
                        }
                    },
                )
                .await;
        });
        let told = Arc::downgrade(self);
        tokio::spawn(async move {
            lane.follow_control_with(move |message| {
                let Some(bridge) = told.upgrade() else {
                    return;
                };
                let mut held = bridge.held.lock_recover();
                match message {
                    // What the device shows of the page: the layers the
                    // page's pictures of it are sent at.
                    Control::Shows { camera, screen } => {
                        if let Some(peer) = held.pictures.get(&from) {
                            peer.set_showing(&page::layers(&camera), &page::layers(&screen));
                        }
                        held.showing.insert(
                            from,
                            Showing {
                                device: from,
                                camera,
                                screen,
                            },
                        );
                    }
                    // The page's stream of this source was cut off on the
                    // way to it: it starts over at a keyframe.
                    Control::Keyframe { source } => held.asked.push(Asked {
                        device: from,
                        source,
                    }),
                }
            })
            .await;
        });
    }
}
