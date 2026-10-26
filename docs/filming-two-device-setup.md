# Filming two devices: the verified setup

The shot is two computers on one desk: a pointer moves off the edge of one and
lands on the other, and an edit made on one appears on the other. This page
says what the product actually does today, which part is proven by a test that
runs on every push, and what to check on the day. It exists so that nothing is
discovered during the take.

## What a test already proves

Each claim below is one test on `main`. The mechanism is the production code
path; only the two physical machines and the browser page are simulated.

| What the shot needs | Proven by | Where |
|---|---|---|
| A change made on one computer reaches the other | `two_actors_exchange_a_workspace_change` | `crates/session/shell/tests/peer_sync_test.rs` |
| A pointer crosses from one computer's screen onto the other's | `native_screen_crossing_reaches_the_two_peer_seat_without_video` | same file |
| The other machine receives the pointer's owner and position | `presence_round_trips_over_iroh` | same file |
| A computer joins a `tilo.coffee`-style site and shares records with a browser in that room | `a_computer_joins_a_sites_workspace_through_its_door_and_shares_it_with_a_browser` | `crates/session/shell/src/shell/network/site_join_tests.rs` |
| Two computers in one site workspace see each other's pointer | `two_computers_in_a_sites_workspace_see_each_others_pointer` | same file |
| The two recordings cut into one side-by-side frame | `two_angles_share_one_frame_as_a_single_step` | `apps/video-editor/src/app/window_bar_tests.rs` |

The browser in those tests is the real `tilo-browser-peer` crate, joined
through the real host's HTTPS routes to a real `tilo-relay` host. What no test
here covers is a page actually mounting the wasm runtime and a pair of real
laptops: that is the day-of check below.

## What a person sets up

Two computers that share one identity, so each finds the other without an
invitation:

1. On the second laptop, make it one of your devices. Palette → **Pair a new
   device** (`peer.pair-device`, "Make your phone, a browser or another
   computer one of your devices"). On a machine without a desktop,
   `tilo-identity pair --label "<what you call it>"` does the same
   ([headless devices](headless-devices.md)).
2. Put both screens in one layout. **Super+P** opens the display layout; each
   of your other screens appears under **Your other screens** with **Include
   this screen in your shared display layout**. Settings › Displays holds the
   same thing with drag placement: **Your shared display layout**, "Drag an
   online device screen beside yours." Placement and input are separate
   decisions; moving a screen grants no input.
3. Let the pointer in. Continuity is on until turned off, including its
   **Display & input** facet ("Use another device as an extra display,
   keyboard and mouse"). Automatic input needs one more switch, once per
   device: Settings › Displays → **Allow automatic input from your devices**.
   Without it the other machine asks before each lease, which is correct but
   makes a bad take.

Super+Esc is **Stop all remote input**: it ends the remote input this machine is
serving, so press it where a stray pointer arrived. It is the way out of a
pointer that went somewhere unexpected while the cameras roll.

## What the browser leg needs

A browser tab is a peer like another laptop: it holds its own device identity,
joined to the same room, and draws natively. So the frame can be one laptop on
native Tilo and the second screen a browser tab on `tilo.coffee`, with both in
one workspace. The browser peer's own prerequisites — certification, the room,
the durable outbox — are in [browser runtime](architecture/browser-runtime.md);
what that page lists as open is a real page mount beside a native shell, not
the routes in between.

## Before the camera rolls

Run this on both machines, with the same workspace open on each. It takes a few
minutes and it is the same check a reader of the finished video would do.

1. Both screens are listed under **Your other screens** (Super+P) and say
   **In layout**. If one says **Offline**, the two are not on one network
   segment or one is asleep.
2. Move the pointer off the edge of screen A onto screen B. It arrives, with
   the pointer's owner named as it moves. Move it back.
3. Type in a document on A; the same text appears on B before the caret
   settles.
4. Stop remote input (Super+Esc) on the machine being controlled, then move
   again. Nothing follows the pointer until a new lease is granted; that is
   the safe state, not a fault.
5. If the browser tab is the second screen: reload it once. It rejoins the
   room and the cursor returns. A tab that cannot rejoin is not ready.

If any step fails, that is the thing to fix before filming, not during. A
failed step names its own remedy: **Offline** is the network, a missing name is
pairing, and a pointer that crosses but types nowhere is the automatic-input
switch.

## What Quinten needs to do on the day

Everything above is someone else's job. The shoot itself is one take and one
drop, about thirty seconds of work:

1. Both laptops on the desk, screens facing the phone, unlocked, same
   workspace open on each.
2. Move the pointer across the gap once, slowly, and let the other pointer
   answer. Do not narrate; the voiceover is recorded after.
3. Keep the phone rolling for the same three actions the screen recordings
   capture, so the cut has something to land on.
4. Drop the raw phone file in the shoot folder. Nothing is edited on the phone.

## The shot, in three steps

1. **Phone take.** Two laptops side by side on the desk, screens visible. Move
   one laptop's mouse across the gap and let the other machine's cursor answer.
   Say what is happening; nothing else happens on screen.
2. **Screen capture, in sync.** Record both screens for the same actions: an
   edit typed on one, the cursor crossing, and an edit back the other way. The
   recording is the material; the phone take is the introduction.
3. **Cut.** Open both recordings in the video editor, put them on two video
   tracks and start them at the same moment (the second recording goes on the
   lower track, so it takes the left half). Press **Place side by side** in the
   editor's bar: the lower track's picture fills the left half of the frame and
   the track above it the right, as one step of undo. The inspector's **Left
   half** and **Right half** buttons place a single picture the same way. The
   cursor crossing is the join: cut to it on both halves at once, which is one
   **Split at playhead** over both tracks.

The take has no script past step 1: everything after it is the same three
actions recorded twice.
