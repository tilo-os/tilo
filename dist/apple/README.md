# Tilo Shell and Tilo Assistant for Mac and iPhone

Two apps, each for the Mac and the iPhone, from one Swift package.

- **Tilo Shell** is a native shell around Tilo's own page. It shows Tilo as a whole: the site
  as a browser opens it, with the desktop and every app.
- **Tilo Assistant** is the assistant's window as an app of its own, in SwiftUI
  (`so.tilo.assistant`): the one main chat, the sub-agents, the Tasks panel, the assistant
  view. Its views read and call one interface and never Tilo itself.

Only Tilo's web runtime draws a live document or a live view inside a conversation, so neither
app draws anything of Tilo a second time. Tilo Shell shows Tilo's page in the system web view
("Links opened full tab" in
[the web runtime's guide](../../crates/browser/tilo-browser-runtime/WEB_RUNTIME.md) is the
page that shows one app) and adds what only the system can do. Tilo Assistant draws its own
frame and leaves a place for each live thing, which Tilo's runtime draws there ("Tilo inside a
native app" below). An iPhone has no other way to draw Tilo: no native runtime exists for it.

The shell's programs name no app. `build.sh` gives a bundle its name, its identifier and what
it shows; no source file of the shell and no file of `Bundle/` names one of the two apps, and
a test keeps it so.

```
Package.swift        the targets, and the oldest system the apps run on
Sources/ShellCore    decided without a web view: the site and what of it is shown, which
                     addresses stay inside, what the page's title says, the hand-over from the
                     share sheet, the sentences, the script that joins page and system
Sources/ShellWeb     the web view that holds the page, notifications, dictation, the check
Sources/TiloMac      the Mac app: one window, the menu-bar item, settings
Sources/TiloPhone    the iPhone app
Sources/TiloShare    the share extension of both
Sources/TiloLive     Tilo inside a native app: Tilo's page as the client behind a Swift
                     interface, and the view that shows one live Tilo thing
Sources/TiloLiveProbe  the same with nothing on screen, for a check to drive
Sources/AssistantTilo  Tilo Assistant's source on Tilo itself: the app as a device of its
                     person, through the engine
Sources/AssistantCore  Tilo Assistant: what its window shows, and the one interface it all
                     comes through
Sources/AssistantUI  its views, shared by the Mac and the iPhone
Sources/AssistantMac, AssistantPhone, AssistantWidgets
                     its Mac app, its iPhone app, and the iPhone's widget extension
Sources/AssistantDrawing  the content of the window's drawing, for tests and pictures only
Sources/AssistantShots  draws every screen to a picture with nothing on screen
AssistantBundle/     what Tilo Assistant's bundles carry
assistant-shots.sh   draws every screen of Tilo Assistant, on this Mac and in a simulator
Bundle/              what the bundles carry, with room for an app's name and identifier:
                     Info.plists, entitlements, the English sentences, the phone's page in
                     Settings, the icon's one drawing
build.sh             builds either app, signs, checks, installs
```

## The two apps

|               | Tilo Shell                              | Tilo Assistant                                       |
| ------------- | --------------------------------------- | ---------------------------------------------------- |
| Built by      | `build.sh shell …`, or `build.sh …`     | `build.sh assistant …`                               |
| Identifier    | `so.tilo.assistant.shell` (see below)   | `so.tilo.assistant`                                  |
| Programs      | `TiloMac`, `TiloPhone`, `TiloShare`     | `AssistantMac`, `AssistantPhone`, `AssistantWidgets` |
| What is shown | everything of the site, in a web view   | the assistant's window, in SwiftUI                   |
| Bundle files  | `Bundle/`                               | `AssistantBundle/`                                   |

Each app keeps its own data and its own keys, so a person is signed in to each by itself.
"Tilo Assistant" below says what that app is made of.

**Tilo Shell** opens the site's own entry, as a browser tab does. `TILO_SHELL_APP` at build
time, or `-TiloApp <id>` once on the command line, names an app that is in front on the
desktop when it opens.

**A shell held to one app.** The shell's programs can also show one app alone, which is what
Tilo Assistant was before it became an app of its own; no word of `build.sh` builds such a
bundle today. A bundle whose `Info.plist` has
`TiloAlone` shows `TiloApp` alone, whatever a setting or the command line names
(`ShellSettings.swift`). The page it opens is the one-app page, which has no desktop. Beyond
that the web view is held to it (`PageHost.swift`, `SitePolicy.isOwn`):

- The way in may lead anywhere in the site, because that is how a person gets in: its entry page,
  an invitation, a pairing link made on a desktop.
- Once the page says the person is in, an address of the site that is not the app's own page
  goes to the system's browser, like any address that is not Tilo's. That is where "Open in
  Tilo" in the one-app page's bar leads, and a link to another app's document.
- A page writes its own address without asking anyone. If the address it shows is no longer
  the app's own, or the person got in on another page, the page is put out of sight and the
  app is opened in its place. A site that leaves the app each time it is opened is said after
  the second time, not tried for ever.

It shows whatever the site's app of that name shows: on a site whose web bundle has no
such app, it shows what the one-app page says of an app it does not have.

## Tilo Shell's identifier

Tilo Shell was first built under the other app's name, and it still carries the identifier
from then: `so.tilo.assistant.shell`. That is on purpose, for now. The identifier is what the
system keeps an app's data under. The site's storage is in the Mac's sandbox container and in
the iPhone's app data, and the keychain item WebKit wraps the page's device key with belongs
to the app that made it. A build under another identifier is another app to the system: it
starts with nothing, so everyone who uses Tilo Shell today would be signed out on that device.

Giving it an identifier of its own (`so.tilo.shell`) takes:

- One line, `id` of `shell` in `build.sh`. The share extension's identifier, the shared
  place, the keychain groups and the entitlements follow from it.
- Every device gets in once more. The device key cannot be carried over: it is a key that
  cannot be taken out, wrapped for the old app. A person pairs the new app from a device
  where they are signed in, and removes the old device from their devices.
- On an iPhone the new app installs beside the old one, which the person then deletes; the
  site typed into the old app's page in Settings is typed again. On a Mac the old container
  (`~/Library/Containers/so.tilo.assistant.shell`) and its group container stay until someone
  removes them.
- The system asks again for notifications, the microphone, speech recognition and the camera,
  and "Open at login" is chosen again.

The moment for it is the first build for the App Store, which needs an App ID of its own in
any case (see "Before the App Store"): then people pair once more, not twice.

## What the system adds

**The page, kept.** The web view keeps the site's data. The page holds its device key as a
browser key that WebKit wraps with a key from the app's keychain, so a person who got in once
stays in for as long as the app keeps its signature and its data. Getting in is the site's own
flow (an invitation, a pairing link, its sign-in); nothing hands a native identity to a page.

**The way in, on the site's entry page.** While the site keeps a device out, it shows its entry page:
what the site says to someone it does not know, and what such a person can do. What only the
system can do for them stands on that page, not beside the page: "Scan code" on a phone and
"Paste link", for the code and the link that "Pair a new device" shows on a device where the
person is signed in, with one sentence under them that says so. An invitation's link is
pasted the same way. The entry page is the page's own markup, in one dark look and the system's
type, and it scrolls; a native bar under it followed the system's appearance and looked like
a strip stuck to a web page. So the script puts the two buttons into the entry page's own column,
with the entry page's measures and colours (`Bridge.swift` says where on each of the entry pages,
and why). A press comes back to the shell, which only then asks for the camera, or reads the
clipboard. A link is taken out of whatever is written around it; a link into another site is
refused on the entry page, under the buttons, with the name of the site the app opens. A link the
site's own computer made under a name only it can use is read as the site's (see "One
site"). The page writes where it stands on its root element (`data-entry`): the
buttons go once it is in and while a device of the person's is being asked to approve this
one, and a person who came in by a link is taken back to where the app starts.

A link is also opened from the File menu on the Mac, by a long press on the phone's icon,
which offers the scan too, or by sharing it to the app. An entry page that draws these options
itself marks its element `data-way-in` and asks the shell through `__tiloShell.wayIn`; the
shell then adds nothing.

**One site.** The app shows the origin its settings name and nothing else
(`SitePolicy.swift`). Any other address goes to the system's browser. The script below runs in
the page's main frame and does nothing on another origin, so no other site's content reaches
the system through it.

A link that is pasted, scanned or shared is followed only into that origin. One other spelling
is read as the site's own: a link whose site means "this computer" (`localhost`, a name under
it, a `127.x` address, `[::1]`) on the site's own port. That is how the computer that serves a
site writes its pairing links and invitations when its page is opened as
`http://tilo.localhost:24120`, and no other device reaches such a name: on a phone it means
the phone. So the link can only be meant for the site the app opens, under the name other
devices use for it. It is opened in the app's own origin with its path and its secret as they
were; and where the app's site goes by a name on the network, a host in the link's route that
also means "this computer" (`#/@127.0.0.1:24100/w/…`) is written with the site's name and the
host's own port, which is where a site served beyond its computer serves its host. Nothing
opens outside the app's origin by this, so no secret reaches another site's page. Another
port, and every other site, stays refused.

**Notifications.** A system web view has no working Notifications API, so the script
(`Bridge.swift`) is one for the page: `Notification` with `permission`, `requestPermission()`,
`close()` and the `click` and `close` events, as `crates/platform/tilo-notify/web/notify.mjs`
uses them. What the page shows becomes a notification of the system; pressing it brings the
app forward and hands the page its choice. A reply typed into a notification lands in the
message field. Beyond a browser, a page's notification may name `actions`
(`{action, title, type, placeholder}`, up to `Notification.maxActions`): each is a button in
the system's notification, a `type: 'text'` one a field, and the `click` event then carries
`action` and `reply`. On the Mac the page answers such a button without coming forward.

**The menu-bar item** (Mac). Closing the window leaves the app in the menu bar, and the page
goes on running behind it. The item shows the count the page writes into its title, with the
line the page wrote, and offers the window, dictation, settings and "Open at login". Opening
at login is the person's choice; nothing registers the app before they ask, and nothing of it
runs after Quit.

**Dictation.** Speech becomes words on the device (`Dictation.swift`); where the system
cannot recognise the language by itself, dictation is not offered. A panel shows what was
heard, and Insert puts it where the person writes.

**The share sheet.** The extension takes words or one web link from another app and leaves
them for the app, which puts them into the message field the next time it is in front. A link
into the Tilo site itself is opened instead.

**Words into the message field** go in as one paste (`compose` in the script). Tilo's page
takes a paste as words wherever its own text has the caret, line breaks included; typed line
by line, a line break would send a message. Where nothing has the caret the words wait for the
next field the person presses, and never go into a password field.

**When there is no page**, the app says what is wrong and offers the one thing to do: try
again, or change the site. The site is the one setting (the Mac app's Settings, the phone's
page in the system's Settings), and `-TiloSite <origin>` on the command line is kept from then
on.

## What the page and the host still owe

- **Live windows in the one-app page.** A page that shows one app draws no live windows
  inside a conversation yet; the desktop does. Tilo Shell shows the desktop, so it has them.
  A shell held to one app has them when the one-app page draws them.
- **The one-app page's bar.** The page that shows one app draws a bar over it with Share and
  "Open in Tilo". In a shell held to one app the second leads to the system's browser. A page that
  knew it was shown as an app of its own could leave the bar out.
- **Answers in a notification.** The page names no `actions` today, so a notification has no
  answer buttons of its own. When it does, they appear with no change here.
- **Being told while nothing runs.** A notice is shown while the app runs, because the page
  asks for it. On a phone that is only while the app is in front. Web push does not exist in a
  system web view; telling a person whose app is closed takes a push from their host through
  Apple's service, an App ID that may receive it, and a place where the host keeps the
  device's address for the person, as it keeps a browser's for web push.
- **An answer with the app closed.** The key that proves the person is the page's. A button
  that answers while the page does not run needs a key the app itself holds.
- **A typed way to post.** Words reach the thread through the field the person writes in.
  Posting on the person's word without a caret (a reply from a notification, a share sent
  straight away) needs an action of the page the script can call.
- **Notices while the window is in view but the app is not in front.** The page tells the
  system only while it is hidden.

## Tilo inside a native app

`TiloLive` is for an app whose own views are native and that is still a device of its person.
It draws nothing of Tilo a second time and speaks to no API with a key.

**The engine** (`TiloEngine`) holds Tilo's page in a web view that shows nothing: the page
that shows Chat alone, which has no desktop and writes nothing into a workspace's layout. The
page is the device: its key, its pairing, its connection, its records. The engine gives the app
what a resource reads as (`read`), word that something changed (`changes`), and the typed
actions with their answers (`invoke`), through the page's own mount: the entry its palette,
agents and assistive technology use, under the person's own access. It pairs from a link
"Pair a new device" made (`pair`), and says where it stands (`states`): the code to compare,
in, or the site's own refusal. It hands the app no key. It opens its one site and nothing else,
and hears only that site's page in the main frame.

A web view in no window calls its page hidden and asks it for no frame, and Tilo's page is in
only once it drew one. So while nobody sees the page a clock asks for its frames, slowly; while
someone does, the screen asks (`EngineScript`). On an iPhone a web view runs its page only in a
window: the app puts `EngineRoom` into its root view, which takes no room.

**A live thing** (`TiloLiveView`) is one app on one record, drawn by Tilo's own runtime inside a
native view. It is one more mount in the engine's own document, through the page's own
connection, so it is the same device: a second web view of the same store would take a device
key of its own, which a paired device cannot certify. The engine's one web view is what the
native view shows. So one thing is live at a time: the others show the picture taken as they
left the front, and wake when they come to it (`LiveStage`). In a message it is looked at and
takes no touch, so the conversation scrolls over it; when the person hops in it fills its room
and the keyboard is its own, on the same mount.

**Where a paired device stands.** A site lets a new device in where it lets people in, which
need not be the workspace its pairing link names: the page that paired can stand in the site's
home workspace under the address of the link's. So once a pairing got in, the engine loads the
workspace the link names once more, without the link's secret, and the device opens it as
itself. A page that stands in another workspace than was asked for is moved there, three times
at most, and then it is said. A device that just got in holds its records before its app has
published every action: Chat lists its messages a few seconds before it offers its send, so
`chatSend` asks again at the page's next word of a change (`whenPublished`).

**The person's own providers.** A person's models, their vault, their events and the like are
kept by the host that runs their workspace, and the host answers them only to the person on a
device of theirs: it decides by the device's certificate, which the transport verified. Tilo's
page is that device, and its script already asks the host as the person (`web/peer.mjs`,
`actions`); it puts no handle on that session where another script finds it. To a native app
that holds it, the page hands the entry of its device, once: the
engine's script says it is there before any of the page's code runs, keeps what it is handed
in its own scope, and gives the app's code `TiloEngine.ask` for exactly the providers in
`PersonProvider`, with typed calls for the models and the vault (`Person.swift`). No new
authority comes of this: it is the session the page holds anyway.

What keeps it the app's alone:

- The script takes such an ask only with a pass the engine made: thirty-two random bytes that
  stand in the script's own scope and in the engine, in no function a page could print and on
  no global. Any other script in the document is answered what an unknown action is answered.
- Only the first entry of a document is taken, and the name the page looks for cannot be
  written over.
- The engine asks only the providers on its list. What else the device could ask its host (a
  workspace's governance, a machine's commands, pairing) it refuses before the page is asked.
- The engine's web view loads no frame. Tilo's page draws everything itself and makes none, so
  content from elsewhere inside a live thing gets no document to run in.
- What a person typed into a protected field is a `Protected`, which prints nothing of itself.
  It goes into the one call that carries it to the host and comes back in no answer: the
  script takes it out of whatever the host says, and the engine hands on no answer that still
  holds it. No text under a name that only ever holds a secret crosses to the app either, and
  the page keeps nothing of such a call (`actions.once`).

**The assistant’s source.** Inside Tilo, open Chat and choose **Set up personal assistant**
in Direct Messages. The Assistant app manages the assistant and opens that same conversation.
On Mac and iPhone, Tilo Assistant presents the conversation in SwiftUI. As a paired device,
it discovers and calls the workspace’s signed actions through `TiloSource`. Chat owns the
messages, Assistant owns the workspace agent roster, and Tasks owns the issues. On a hosted
page the client reads Chat’s message chain and posts directly to the same conversation. Chat names who wrote a message by their kind (`{"person": …}`, `{"agent": …}`) and
spells the conversation of a person and an agent from the two (`ChatMember`,
`ChatConversation.direct`); a page from before that says bare ids, and both are read. While
an agent is at work on a message of a conversation the source says so, and
`stopAnswering(in:)` is Chat's own `stop-agent` on that message: the message field's button
stops the answer while nothing is typed. The models, the vault and the events are asked of
the person's server when the device gets in and after a call of the app changed them, never
on a clock: `link(provider:key:model:)` carries a provider's key from the protected field to
the server, and `setUpEvent` sets up what comes at a time. A secret goes to the vault, and
the question is answered with the item's reference: the conversation holds nothing else of
it. Where the page says which workspace is the person's own, the app goes there, because
that is where the assistant lives. Tasks come from the workspace tracker; attached files
use the device’s workspace content service when available.

**Approving a hosted assistant.** In the personal assistant conversation, request Chat access
to let the assistant reply. To let it work on tasks, proposals and schedules, choose the
option that lists Chat, Tasks, Assistant and Shortcuts. For other workspace apps, choose
**Choose apps…**, select the apps the assistant may use and choose **Request selected apps**.
Chat is required for replies. All optional apps start unchecked. The workspace supplies the
available choices. Each request covers this workspace for up to six days.

In the browser where you created or restored your Tilo identity, choose
**Open your personal workspace** from the Lounge. Review the assistant, workspace, apps and
expiry, compare the code with the one on the requesting device, and choose **Approve** or **Decline**.
With a model connected, the assistant can reply once its approved session connects.

You can request access again after a denial or expiry. To change the apps it may use, open
**Manage assistant access** in Chat or **Assistant access** in the Swift app, revoke its
current access, and request the new scope. The same browser approval applies. Connecting
an account or approving an individual task still uses that service’s usual consent controls.

**A service by its address.** The person gives their assistant a service's address. Their
server looks at it and asks them in their conversation whether to connect it: a question that
keeps the service's card beside its words (`tilo.connections`, `propose`). The app draws that
question as the card (`AssistantTilo/Connections.swift`). "Not now" is their no and
"Authenticate" their yes, both Chat's own answer, and the yes is what adds the service on
their server. Where the service wants a sign-in, the app begins it as the person on this
device and opens the service's own page in the system's sign-in sheet
(`ASWebAuthenticationSession`), which comes back to an address in a scheme of the app's own
(`SignInReturn`). Nothing of a sign-in is typed into the app; what the sheet comes back with
goes to the person's server as what a protected field holds, and to nobody else. The card
keeps no state of its own: it stands by the question and by the person's connections as
their server lists them, which the app asks on the way in, after its own calls, and once
more when a card says their server added a service that the list as last read does not hold.
The assistant view's "Connections" shows that same list, newest first: each service by its
own name, when it was added and how many commands it offers, or that nobody is signed in to it.

**Which agent is the assistant, and the first time.** When its device gets in, the app asks
the workspace's server, as the person on this device, which agents the workspace keeps
(`tilo.workspace.agent`, which Assistant keeps, through the device's entry:
`TiloEngine.askAgents`). The assistant is the
active agent the person keeps that no outside client connects as and that is nobody's
sub-agent, the earliest made. Where the server answers that the person keeps none, the app
calls the same `ensure-personal-assistant` action as Tilo's Assistant. It creates one
assistant for the signed-in person with its initial instruction. Tilo assigns each turn to
an eligible device. Only the server's
word makes one: a page that just got in may not have handed its device's entry over yet. On
a page that hands an app no way to its server, nothing says which agents the workspace keeps
and the page publishes none of them: the main chat says so, and nothing is made. No setting
names an agent. What is in the way is
said in the main chat (`AssistantSource.setup()`): that the assistant is being set up, or
the server's own sentence where it refused, with Try again.

**The first seconds.** Whether the person's Tilo answers has three values (`Reach`). It is not
known from the start until a first try to get in has ended; the window then says "Waiting
for Tilo" in its quiet hand and nothing of a failure. "Tilo cannot be reached right now" is
said only after a try that failed, and goes the moment a later try gets in. No count of
seconds ends a try: its page's own word does, or the network's failure. A message sent in
those first seconds waits for the try to end and for the assistant to be found, and goes out
then; where the try fails it is back in the field with the reason, as any message that did
not go through.

**A site that answers and lets the device in no more.** A device that is its person's can
stand before a site that serves its page and whose page's join stops at every load: a store
that missed the one time Tilo's page said that its workspace is gone stands so for ever. The
engine tells a try that ended so from one that met no server, by the case the page's failure
screen names (`TiloEngine.joinStops`, `answered(failure:)`); `TiloSource` says it to a device
that is its person's (`AssistantSource.joinStops()`); and where the window would say that Tilo
cannot be reached it says so in one sentence and offers the way in: one button, which does
what "Sign out of this device" does. Nothing of the device is given up before that press, and
nothing presses for the person. `WorkspaceGoneTests` checks the engine and the source,
`WayInOfferedTests` the window.

**A device that was approved and stopped before it got in.** A pairing can stop after a device
where the person is signed in approved it: the page that was let in cannot draw, its join
stops, its site goes away, or the app is ended. The store then holds the device that was
approved, and an app that stood at its plain way in at its next start led its person to pair
a second time and to meet the same stop. The engine holds the workspace a pairing was let into
from the moment its page is past its join (`TiloEngine.letInto`, `letIn(entry:arrival:)`).
`TiloSource` keeps it with its time (`TiloLiveApproved`) until the device is its person's,
another link is taken, it is signed out or its workspace is gone, and at its next start opens
that workspace as the device the store is, with nothing to pair with
(`AssistantSource.joinApproved()`, `goOn()`). The way in then says that the device was
approved and when, says what stopped in the page's own sentence with its technical details
under it, and offers Try again first, which goes on as that device. Pairing afresh stays
beside it, also while a try is in work: no count of seconds ends one. `ApprovedJoinTests`
checks the engine and the source across the app's starts, `WayInApprovedTests` the window.

**The provider step.** While no provider of the person's has a key, the main chat says so
and offers the step: the person picks a provider and types a key of their own into the
protected field, the one field of the app a secret is typed into (`ProtectedField`), and
`link` carries it to their server, which keeps it in their vault. The models the server then
lists are offered, and the one chosen answers and works in the main chat. The server offers
no list of the providers it links by name yet, so the four names its `add-preset` knows are
written in `TiloSource`.

**The way back to the key.** Once a provider has its key the step is gone from the main chat,
and the assistant view's Model line leads back to it: under the models its menu offers "Use
another key for NAME" for each linked provider and "Link another provider" where one is left
(`AssistantModel.keyWays`, from `AssistantSource.providers()`). Either shows the same step in
the main chat again with that provider picked (`AssistantModel.keyStep`); the app grows no
second field for a key. A key typed for a linked provider takes the place of the one their
server keeps (`set-key`; the provider is not added again), the model step follows, and the
model the person chose stands. Not now puts the step away.

**The way back from the failure itself.** A reply whose model gave no answer carries why, where
the person's server says so with the reply's work (`failure_cause`: the kind, and the provider by
the id and the name their server gives it). Where the cause is the key, the key's account or a
missing provider (`authentication`, `permission_denied`, `credits_exhausted`, `no_provider`),
the main chat offers one press under the newest such reply: "Use another key for NAME", or "Link
another provider" (`AssistantModel.replyWay(in:)`). It shows the same step, with what their
server lists at that moment. The app reads the typed cause and never the reply's words: a
server that says no cause gets no press. Once a key was typed for that provider nothing is
offered under that reply again.

**What Tilo asks before it pairs.** Tilo's page asks "Add this device?" before it asks
anyone to let the device in, and says whose devices it would join and which server adds it.
The engine tells the app the page's own sentences and its own words for the two ways on
(`TiloEngine.State.asking`), the way in shows them, and only the person's press goes on or
stops, as a press of the page's own button (`answerPairing`). Nothing in the app answers for
them; in the programs with no window, whoever drives them does.

**The page's word of a change.** Tilo's page says `tiloChanges` at every frame it draws and
at every word of its session, many times a second while nothing happens. The engine hands
the word on only when what the page's device heard or holds moved (`tiloChannels`), looked
at eight times a second at most; when the page's description of itself for assistive
technology changed; and after a call of the app's own changed something.

**The device's name.** The app names its device where its person lets it in and lists their
devices ("Tilo Assistant on Ada's Mac"). The engine's script says it as
`tiloNativeHost.name`, which the page's `deviceLabel` (`web/pairing.mjs`) reads before it
falls back to the browser and the system.

What it leans on, until Tilo's page offers each: a second mount is made through the page's
own `host.mjs` and `peer.mjs`, where the page has no `tiloOpen`; and the person's own
providers are reached only on a page that hands a native app its device's entry.

`TiloLiveProbe` is the engine with no window, driven over its standard input: it pairs, prints
Chat's messages as they arrive, sends through Chat's own send, and shows a document in a
window that is never put on a screen, with real key presses. A store it keeps holds a device
key that only the very program that made it reads again. `AssistantMac --drive` is Tilo
Assistant with no window (`Drive.swift`): the same source and the same model its window's
views read and call, driven the same way. Of each reply that grew it says when it drew the
first change of its words, when the last and how many (`grew`): with no window, it counts
what a person would have seen. Both end when whoever drives them is gone.

## Tilo Assistant

The assistant's window as a SwiftUI app, built after one accepted drawing: a sidebar with the
one main chat and the sub-agents, the conversation with its message field, the Tasks panel, a
sub-agent's window, the assistant view, and the way in before a device is paired.

**Proactive checks.** In your private assistant conversation, ask “What can you do for me
today?” or “Check this workspace every day at 09:00.” The schedule runs on the workspace
host's clock while your app is closed. It checks arriving mail, changed workspace documents
and open or due tasks. A scheduled check asks the model only when something is new and
there is budget left. Proposed tasks appear in the workspace's shared Tasks tracker, where
the tracker controls who can read them. In your assistant conversation you can change the
schedule, lower the daily limit or pause the checks. Your device's notification settings
control which resulting notifications interrupt you.

**One interface.** The views read and call `AssistantSource` (`Sources/AssistantCore`) and
nothing else: a conversation as a live stream of pieces (a message, a one-line report, a
question with its answers, a secret it needs, a service that wants a sign-in, an event it set
up, a brief, a reminder), the sub-agents, the tasks, what comes at a time and what happens
when something else does, the facts of the assistant view, and pairing. Every read is a
stream that begins with what is known now, and what a call changed comes back through the
streams. An implementation speaks to Tilo; `MemorySource` keeps everything in memory, for
tests and pictures. One line in each app names the implementation (`Source.swift`): Tilo
itself, with the app as a device of its person on the site its build names (`TiloSource`, see
"Tilo inside a native app"). Until a device is paired, the app is empty and stands at the way
in.

**The app starts empty.** The drawing's content lives in `AssistantDrawing`, which the tests
and the picture tool link and no app does. Its people and firms are examples: a test keeps
the app's sources and sentences free of them, because nothing in the app is about one
person's line of work.

**A live thing** in a message, or at the head of a sub-agent's window, is a place the frame
leaves. `LiveThing` is a value the frame never reads, and the view that draws it comes in
through the environment (`liveThingView`) from whoever implements the source. `AssistantRoot`
also takes the source's engine view, which has to stand in an iPhone's window to run and
shows nothing.

**The message field** is the system's text view. Markdown is formatted in place while it is
typed, and every character stays: Markdown's own signs are out of sight, except in the words
the person is writing in (`Markdown.marks`, `Markdown.hidden`, `FieldStyle`). Return sends and
Shift Return makes a line; on an iPhone's glass keyboard Return makes a line and the arrow
sends. On a keyboard, the Mac's or one attached to an iPhone, Up in an empty field brings back
the last message the person sent in the conversation, Up again the one before and Down the
next, until they type, move the caret or press Escape (`Stepping`): the messages are the
conversation's own, and the app keeps no list of them. Everywhere else the arrows move the
caret. While an input method is putting a letter together the field takes no key for itself.
On an iPhone a drag down the conversation takes the keyboard down with the finger, the
system's own way for a scroll view, and a touch on the conversation puts it away; the words
being written stay in the field and the conversation stays at its newest piece. Sending
leaves the keyboard up: the field is empty and takes the next message. A screen's ground
reaches past every safe area, the keyboard's too: the keyboard's upper corners are round, and
what they leave in sight is the screen's own ground, where the system's own view is black in
the dark look.
Attachments and dictation stand under the field, and dictation is the shell's.

**A file in a message** is shown with the name and the size Chat gives. In Tilo a file that a
message carries is content its workspace keeps and the message names (Chat's `attachments`):
`ChatMessage.files` reads them, `TiloFiles` hands them to the views, and a file's tag says its
name and its size (`Attachment.words`). The app keeps nothing of a file.

**A file opens** by a link. A press on a file's tag reads its message again and asks Tilo's
page for a link to the content (`TiloEngine.contentLink`, the page's `link`: an address on the
app's own site, good for a short time; one that leads anywhere else is not followed). The
system downloads it straight into a file under the app's temporary folder, named by the last
part of the file's name alone, and the system's own viewer shows it: Quick Look on the Mac,
the same on the iPhone (`quickLookPreview`). A file is looked at there and nothing in it is
started. The link is in no log and in nothing the app keeps; every press asks for a new one,
and what was opened more than an hour ago is removed at the next press. The page of a site
whose code is older hands no link: the app says "A file cannot be fetched into this app yet".
`FileOpeningTests` checks this with a download of its own making, not with Tilo's own page,
its server or the viewer on a screen.

**A file is sent** through Tilo's page. The plus under the message field, or a file dropped on
it, puts a file under the draft. Send puts each file into the person's workspace a piece at a
time (`TiloEngine.putBegin`, `putAppend`, `putFinish` ask the page's `put`: 384 KiB a piece,
read from the file as it goes, never whole in the app or in the page) and then sends ONE
message with the words, which names every file as Chat does (`send` with `attached`: the name,
what the workspace keeps the content under, the size). The page refuses a file larger than a
message may carry by its size alone, before any of it crosses, and the app then says how large
the file is and how large one may be. A file that is empty, cannot be read or did not go in
whole sends nothing, and the draft stays in the field. The page of a site whose code is older
hands no such way: the app says "A file cannot be sent from this app yet" and touches nothing
of the file. `FileSendingTests` checks this against a page that speaks as Tilo's does, not
against Tilo's own page and server.

**A secret** is typed into the system's protected field. What is typed lives in that one view
until Save hands it to `AssistantSource.keep`, and the field is emptied before the call. It is
never part of a message, a draft, a notification or a log, and a `Secret` prints nothing of
itself. The Mac's window is not one the system restores, so nothing of it is written down for
its next start; on an iPhone the field is covered from the moment the app leaves the front,
which is when the system keeps a picture of the window.

**The way in.** “Get a pairing link” opens the configured Tilo site's desktop entry in the
system browser. On a first visit, once the site admits you, keep your recovery words and choose
“Skip for now” to open your workspace. If you already have an identity, open your workspace in
the browser that holds it. Choose “Pair a new device” on the workspace home page and leave that
page open. Choose “Open in Tilo Assistant” on the same device, scan the code on a phone, or paste
the link into the app. Compare the code shown in Assistant (`Session.confirming`) with the one
shown in the browser and approve the device there if they match.

**A pairing that ends.** A pairing can end before the device is anybody's: its site refuses
the link, the page stops after the person pressed its button, or the device is let in and its
page cannot draw there. The engine keeps the page's own sentence for it (`TiloEngine.State`
`refused` and `failed`), `TiloSource` says it (`AssistantSource.pairingEnded()`), and the way in
shows it under its buttons until another link is taken or the device is in: whoever stands at
the way in again reads why. A device that is its person's already is told nothing there.
`PairingEndedTests` checks this against a page that speaks as Tilo's does, `ModelTests` in the
window's model.

**A workspace that is gone.** A device goes back to its way in by itself when Tilo's page says
that the workspace it was let into is no longer there: the site's server was set up again
since, or the workspace was deleted. The engine reads the page's own name for the case
(`data-failure` `gone` or `deleted` from its failure screen) and, for a device that kept another
server's certificate, the one sentence today's page has for it (`TiloEngine.resetSaid`, which a
test holds against `web/peer.mjs`). It then is `gone` and loads nothing again. A page whose
server turns its open session away (`data-refusal`) still shows what it showed: the engine
loads it again, as a page that stopped, and its entry says which of these it is. So is a page
that is in and says that its connection to its server is cut (`data-connection` `offline`),
unless it finds its server again before it is asked for: a site that is set up again is away
for a while, comes back as another and turns nobody's open session away. `TiloSource`
puts away what the app held, as at a sign-out, asks nothing of a server that holds none of it,
and says `Session.reset`: the way in with one sentence that says why, at every start until the
device is paired again. What the person typed and did not send stays in its field. A refusal
that names no such case (a person taken out of a workspace, a device removed from its
person's) changes nothing: the app stands as it did. `WorkspaceGoneTests` checks this against a
page that speaks as Tilo's does, `WayInAfterResetTests` in the window.

**A link handed over.** The app answers to one more address of its own,
`tilo-assistant://pair?link=<a pairing link>` (`HandedLink`). Tilo's page can offer it beside the
link and the code of "Pair a new device", so that on the computer the app is on nobody
copies and pastes. The app takes it only while it stands at its way in, and then it is a pasted
link and nothing more: a link that leads to another site than the app's own is refused in words
and shows no card; Tilo's question stands with the app's own site named in full under it
(`PairingAsk.site`); and nothing goes on before the person's press there and their approval, with
the code both show, on the device that made the link. An app that is in, or in the middle of a
pairing, takes no link from outside and says nothing of it. An app that was started by the
address keeps the link until it knows where it stands (`AssistantModel.takeHanded`).
`HandedLinkTests` checks this in the window.

**Read out.** The Mac's window is read in three named parts in the order a person reads it:
the sidebar, the conversation with its field, the Tasks panel. Every control is named by
what it does, the drawing's dots between facts are pauses and not words, the person's own
words are read as theirs, a question that waits says that it waits, and the protected field
is one. On an iPhone the same views say the same.

**At the largest text sizes** on an iPhone the signs grow with the words, a row too narrow
for two things side by side stacks them, names are read whole on as many lines as they
take, the way in scrolls, and the message field keeps its height while the conversation
gives way.

**A piece's menu.** A long press on a piece of words on an iPhone, the second button or
Control and a click on a Mac, opens a menu for it (`PieceMenu`): Copy and Select text. Copy
puts the piece's words on the pasteboard as plain text with every line where it stood
(`Markdown.copied`): Markdown's own signs are gone, a list keeps its entries and its numbers,
code keeps its lines, a link's address follows its words. Select text lays the same words out
in the system's own text view, where a part is marked with the system's handles on an iPhone
and by a drag on a Mac, and copied by the system's own Copy; nothing in it can be changed. On
a Mac a drag also marks a message's words where they stand in the conversation. Words that can
be marked answer the second button with the system's own menu for text, so a view of the
app's lies over a message there and takes the second button and Control with a click alone
(`SecondButtonView`): every other press goes past it to the words. The app keeps nothing of
this.

**Answering a message.** Reply, the first row of a piece's menu, and on an iPhone a swipe to
the right on the piece (`SwipeToReply`) put the piece above the field as what is answered
(`AnsweredLine`): one line of its words and a cross that drops it. It is kept on the draft
(`Draft.answers`) and nowhere else. The message then sent says which message it answers. Where
the page's Chat keeps that (its `post` takes `answers`, as the page lists it) the app posts
with it, a read gives it back (`Message.answers`), and the conversation shows the line above
the reply's words, where a press goes to the answered piece. On a Tilo whose Chat does not keep
it yet the words are sent with one line of the answered message above them as a quote, so the
assistant is told what is answered either way.

**Quoting words of a message.** Quote in a piece's menu lays its words out as Select text does,
under a line that says to select the part. With a part marked there, Quote answers that part: a
button in the sheet's head, and the first row of the menu the text itself opens over marked
words (the system's own on an iPhone). The marked words stand above the field; the message then
sent carries them and where they stand in the answered message's own words
(`Answered.span(of:in:)`, as Chat keeps a quote: `text`, `start`, `end` in UTF-8 bytes). A part
is marked in the plain words, so where Markdown's signs stand between its words the place
reaches from its first word to its last.

**A task's replies.** A task's line counts the replies the conversation does not show.
Pressed, they open under the line (`AssistantSource.replies(of:in:)`), and the same press
puts them away. On a narrow window what does not fit beside a task's title goes under the
title, where its words begin, and the Important mark stays on the title's line while it fits.

**The Mac app** is one AppKit window that holds these views, with the menus and the menu-bar
item's menu made by hand (`MacApp`, in `AssistantUI`). SwiftUI makes a scene's menus and its
menu-bar item only while the app runs on a screen; made by hand, each is a thing a test builds
and drives with nothing on a screen. `Entry.main` in `AssistantMac` is all the program adds:
it names the source.

**Type and colour** are the drawing's: its colours as named tokens (`Token`), its measures
(`Measure`), its lines 1.45 apart. The face is Tilo Sans, which is the drawing's Instrument
Sans with more letters. The repository keeps no font, so `build.sh` takes the two files from
the font catalogue (`tools/font-catalog.py prepare`, into `target/prepared-fonts`) and the
app makes them known to itself alone.

**Outside the window.** `NoticePlan` says which pieces become notifications: a decision that
waits, a reminder and a brief interrupt, and so does what the assistant judged important;
what did not interrupt is listed without a sound, with the assistant's reason. A question's
first answer is the system's own button beside Open, and a secret is never asked for there.
`LiveActivityPlan` says what earns a live activity on an iPhone: a reminder that comes once
within the hour, a brief before something begins, and work the person waits on. The
assistant's lasting work earns none, because an activity that is always there says nothing.
The app tells both while it runs.

**What the person's server told them.** Whether something is a reminder, a brief or important
is decided by the person's server, which writes each such thing into their log of what they
were told (`tilo.telling`, `told`). `TiloSource` follows that log while the app is open: it
reads it whole once, then asks for what is newer, and the server holds each ask until the log
moves, so the app keeps no clock for it. An entry names the message its cause wrote (an event
of the person's that came, a moment they asked to be told at); the assistant's answer to that
message is the piece. The cause's own message, which is what the server told the assistant,
is no piece: the answer stands under a line with the cause's name, and is a reminder or a
brief, or carries the mark, as the entry says. A piece that becomes one of these after its
words arrived is told then, once. What the person saw, opened from its notification or
arriving in the conversation they look at, is acknowledged to their server, which then tells
them of it nowhere else.

What their server kept from notifying is listed with no sound and no banner, and its words
say why, from the reason their server gives (`Quiet.line`): no rule of theirs, a line of
their rule that waits on something, their own model's line, or that nobody could judge it.
The buttons under a notice are the ones the entry offers (`Item.offers`), the expected one
first, and a press invokes that action as the person and tells their server which one it
was; an action of another app than Chat is not offered there, and neither is a permission
that is to stay. A thing that insists is told again each time their server raises it, until
the person saw it. What came while no device of theirs was looking (`since-you-left`) is one
message at the top of the new stretch of the main chat: what waits, what was done without
asking, what else came in.

**Told while the app is closed** is off in a build until the build is told to include it:
`TILO_APPLE_PUSH=1` in `build.sh`'s environment. Without that word a build is as it always
was: it claims no push right, carries no notice extension, and is signed as before. With it,
the app claims Apple's push right only where its provisioning profile grants it
(`aps-environment` on the iPhone; `com.apple.developer.aps-environment` on the Mac, where
the app then carries its profile, `TILO_APPLE_MAC_PROFILE`): a signature that claims what its
profile does not grant is not installed on an iPhone and not started on a Mac. A build that
was told to and whose profile grants none says `no push in this build` with the reason, and
is as one that was not told. A build with push also carries the notice extension
(`AssistantNotice`, `so.tilo.assistant.notice`; on the iPhone under a profile of its own,
`TILO_APPLE_NOTICE_PROFILE`), shares one keychain group with it
(`<team>.so.tilo.assistant.told`, after the app's own, which stays the first), says in its
`Info.plist` which of Apple's two services reaches it (`TiloPush`), and on the iPhone may be
woken by a push that shows nothing. What it takes at Apple, which nothing here makes: Push
Notifications and Time Sensitive Notifications on the App ID `so.tilo.assistant`, an App ID
`so.tilo.assistant.notice`, their profiles made again after that, and a key for Apple's push
service, which the person's server holds and never the app. A Mac build with push names
itself by its application identifier for the first time; whether it still reads what a build
without one kept on that Mac is not shown.

In a build with push the app asks Apple's service for an address once its person allowed
notifications, at every launch, and hands it to the person's server
(`AssistantSource.register`; `register` on `addresses` of `tilo.telling`) with a key of the
device's own (`AssistantClosed`): a P-256 key their server seals what a push says for, and
sixteen secret bytes it names a thing by, so that no word and no id of a thing can be read on
the way. The key is made when the address is first handed over after a sign-in, kept in the
group the app shares with the extension, and gone at sign-out, when their server is also told
to forget the address; a key left behind is gone the moment a sign-in begins. A piece the
person was told of carries the name a push gives it (`Item.toldAs`), and the app's own notice
is kept under that name, so a push for the same thing takes its place and nothing is shown
twice; a push that comes while the app is in front shows nothing, and one that says a thing
waits no more takes its notice away. A build without push does none of this: it asks for no
address, hands over nothing and keeps no key.

The notice extension (`AssistantNotice`) is handed a push before the person sees it. Where the
push carries a part sealed for this device and it opens with the key the app keeps, the notice
says who told it, the server's words and the Important mark, under its kind's own category.
Where a push carries no sealed part (a workspace whose words stay on its server), where it
does not open, or where no key is kept, the notice is handed on as it came: the one plain line
the bundle's sentences give it (`TOLD_WAITS`) stands. Not there: the buttons a thing offers
are not put under a notice that came by push, because whether one may be pressed from under a
notification is judged by what it would invoke, which no push carries; and a press on such a
notice opens the app, not yet the thing itself. A live activity that the app starts and keeps
current itself needs no capability.

**Pictures.** `assistant-shots.sh DIR` draws every screen with nothing on screen, light and
dark: the Mac's by this Mac, in a window that is never put on a screen, and the iPhone's in a
simulator that shows no window. The Mac's picture tool is signed before it is started
(`TILO_APPLE_SIGNING_IDENTITY`): nothing that nobody signed is started on a Mac that someone
works on. The script's head names the simulator, the kind of iPhone and the text size it is
set to, and its other ways: `said` writes down what every screen of the iPhone says of itself
to someone who cannot see it, `secret` types into the protected field and looks at the window
as the app leaves the front, `typing` takes the whole screen with the keyboard up, `place`
shows a chat for the first time in each way the app does and checks that it rests at its end
and that each message is read out where a held finger reaches it (a line FAULT fails it), and
`keyboard` brings the keyboard up, puts it away and sends a message, light and dark: it says
what a drag on the conversation does to the keyboard, what the window draws behind the
keyboard's upper corners, what Up, Down and Escape of an attached keyboard do in the field
and what is kept, takes the whole screen
with the keyboard up and with it away, and fails where it found a fault. Asked for `told`, it also hands the system the drawing's
notifications and live activities there, and writes down what the system then holds.

## Limits

- The page draws with WebGPU and reaches its host over WebTransport: system 26.4 or newer.
- The page opens no session where WebTransport offers no datagrams
  (`crates/platform/tilo-net/src/web/webtransport.rs`). The check below says whether a device
  has them.
- A site is reached over `https`, or over `http` on this computer only: a page elsewhere over
  plain `http` is no secure context, so it has neither its keys nor WebGPU.
- The live connection is HTTP/3 over UDP to the host itself. A front that carries only HTTPS
  does not carry it.

## Build, check, run

`build.sh` takes the app first, `shell` or `assistant`; without one it builds Tilo Shell.

```sh
dist/apple/build.sh test                         # the package's tests; they open no window
dist/apple/build.sh shell mac --check --live     # build, sign, and ask the app what it can tell
dist/apple/build.sh shell simulator --live       # the iPhone app, asked in a simulator with no window
dist/apple/build.sh shell iphone --install       # build, sign, and put it on the one reachable iPhone
dist/apple/build.sh assistant mac --check        # Tilo Assistant: build, sign, and ask it what it can tell
dist/apple/build.sh assistant iphone --install   # its iPhone app with its widget extension
TILO_APPLE_PUSH=1 dist/apple/build.sh assistant iphone  # the same, told while it is closed, where its profile grants push
dist/apple/assistant-shots.sh DIR                # every screen of Tilo Assistant as a picture, nothing on screen
open "target/apple.noindex/shell/mac/Tilo Shell.app"  # start the Mac app
```

Builds land in `target/apple.noindex/<app>/`, where Spotlight does not look: a build is
offered to a person as an app only once someone puts it where apps go. A bundle that is open
is never replaced; quit it first.

`build.sh` reads who signs from `TILO_APPLE_SIGNING_IDENTITY`, the iPhone app's provisioning
profile from `TILO_APPLE_PROFILE` and the site from `TILO_SHELL_SITE` (default
`https://tilo.sh`); its head lists the rest, among them what passkeys take ("Passkeys" below). The signing team is read from the identity and
written into the bundles, never into the sources. Without an identity the Mac app is signed by
nobody: it runs on the Mac that built it, a later build cannot read the keys it kept, and it
has no share extension.

A build says which build it is, and nobody sets that by hand. Its version is the release's
name (`dist/VERSION`, or `TILO_VERSION`), and many builds are of one release. Its build number
is the number of commits the commit that is built stands on, so every later head of `main`
reads a higher one: the Mac's About window shows the two as `Version 0.1.13 (49852)`. The
commit's own name is in every bundle's `Info.plist` as `TiloCommit`, with `+changes` behind it
when the tree that was built differs from that commit, and Tilo Assistant's `--check` says all
three in its first line. An iPhone's Settings show the version alone, so on a phone the app
says the line of the Mac's About window itself, from the same two entries of its bundle
(`AssistantBuild.said`): at the foot of its way in and under its person at the foot of its
list, wherever its root hands the line down (`BuildLine`, `BuildLineTests`). A Mac's window is
handed none. A tree that is no git checkout, or one that holds only part of its history,
builds nothing and says why: it could not say which build it is.

Each app has one drawing for its icon, so the two are told apart at a glance: Tilo Shell's is
`Bundle/AppIcon.svg`, Tilo's mark on its light ground; Tilo Assistant's is
`AssistantBundle/AppIcon.svg`, a message on Tilo's blue with the mark's three parts as three
dots. The repository keeps no pictures, so `build.sh` draws an app's drawing at every size a
bundle carries when the bundle is made: the Mac's icon, and the two files the iPhone's
`Info.plist` names.

`--check` runs the app with nothing on screen and prints one line per fact: who signed it,
whether it is sandboxed, whether the web view offers a secure context, WebGPU with an adapter,
WebTransport and its datagrams, whether a key the page stored is still there on a second run,
whether a notice and words cross between page and system, what the build opens with the
settings kept for it on this device, whether the app may use the site's passkeys, and whether the site and its host answer. `--live` also
opens and closes one WebTransport session at the host the site names; it opens no stream, so
no person and no workspace is touched. A simulator has no graphics adapter, so it fails that
one line and cannot show Tilo; a real iPhone is asked from a Mac:

```sh
xcrun devicectl device process launch --console --device <id> so.tilo.assistant.shell --check --quit
```

## Passkeys

Tilo's page keeps a person with a passkey and signs them in with one ("Use a passkey"). In a
browser that is the browser's own business. In an app's web view the system hands the page a
passkey only for a site the app is associated with, and refuses every other request before
it shows anything
([Supporting passkeys](https://developer.apple.com/documentation/authenticationservices/supporting-passkeys),
"Use passkeys in a web view"). Associated means both of these:

- **The app's signature names the site.** Its entitlement
  `com.apple.developer.associated-domains` holds `webcredentials:<host>`. `build.sh` writes
  one entry for each host in `TILO_APPLE_PASSKEY_SITES` (default: the host of
  `TILO_SHELL_SITE`) and lists the hosts in the bundle's `Info.plist` (`TiloPasskeySites`).
  It does so only where the provisioning profile grants the entitlement: an iPhone installs
  no app, and a Mac starts none, whose signature claims more than its profile. Such a profile
  is made for an App ID of the app's own identifier with the Associated Domains capability.
  A profile for every identifier of a team (`TEAM.*`), which is what signs a trial build,
  never grants it. A profile for the app's own identifier covers the app alone, so the
  iPhone's share extension is then signed with a profile of its own
  (`TILO_APPLE_SHARE_PROFILE`; the team's profile for every identifier will do). The Mac app
  runs without a profile and is given one for this alone (`TILO_APPLE_MAC_PROFILE`). A build that may name no site says "no passkeys in this build"
  and is otherwise the same app.
- **The site names the app.** `https://<host>/.well-known/apple-app-site-association` lists
  the app's application identifier (`<team>.<identifier>`) under `webcredentials`. Tilo's web
  entry serves it once its deployment names its apps (`TILO_WEB_APPLE_APPS`, in
  [its start script](../../crates/browser/tilo-browser-runtime/container/entrypoint.sh)).
  Apple's servers fetch it over `https`, following no redirect, and a device asks them when
  the app is installed and about weekly after. The address has no place for a port: the file
  is asked for on 443, whatever port the site itself answers on.

A passkey belongs to the name of the site it was made at, because the page names its own host
as that site (`web/identity.mjs`). `tilo.sh` in a browser and `tilo.sh` in the app are one
site, so one passkey opens both. The same Tilo under another name is another site, with
passkeys of its own: one made on a Mac at `tilo.localhost` opens nothing at the name a phone
reaches that stack by. A site on this computer (plain `http`, a name under `localhost`) is
associated with no app. A person may name any site in the app's settings, and passkeys work
at the ones the build was signed for.

**A site Apple's servers cannot reach**, such as a test stack on a private network, is named
with `?mode=developer` after its host (`TILO_APPLE_PASSKEY_SITES="tilo.example?mode=developer"`),
and the device then reads the file from the site itself. Apple allows this for a build signed
with a development profile, on a device where the person switched it on: on an iPhone in
Settings, Developer, "Associated Domains Development" (the Developer page is there once the
phone was used with Xcode), on a Mac with `swcutil developer-mode -e true`. A build for a
store names no site this way.

Where the app is not associated with its site, nothing but the passkey is missing. The page
says that passkeys cannot be used there, and to add the device from one where the person is
signed in ("Pair a new device"): the way in that needs nothing from Apple. `--check` says
which it is, "passkeys of <host>": the app may use them, or the build's signature does not
name the site, or it does and the system still refuses (the site does not name the app yet,
or the device has no passcode).

## Before the App Store

The Mac apps are sandboxed from the start with the entitlements `Bundle/Mac.entitlements`
explains; they listen on nothing, and update nothing of themselves. Still to do for a store
build of either: an App ID of its own (push, Associated Domains for passkeys, and an app group in place of the keychain group
the phone's share extension uses under a development profile; for Tilo Shell this is the
moment its identifier changes), a universal Mac binary, an icon drawn for each platform, the
usage descriptions in every language the app ships, and review notes that say what the page is
and name each native part above.

## What is checked, and what is not

Checked with no window: the tests (the settings and what a build of one app is held to, the
policy, the hand-over, the sentences against their catalogue and under both apps' names, the
script against a page that speaks as Tilo's does, and the web view of a one-app build against
pages that leave the app, and the way in against pages that show the entry page as Tilo's
do), the Mac apps' `--check` in their real signature and sandbox, and
the iPhone apps' in a simulator.

What `build.sh` makes of Tilo Assistant with and without push is checked by `BuildTests`: the
script's own functions make a bundle from stand-ins for the programs, with the signing
command written down instead of carried out and a profile that is only the list a real one
decodes to. A build that was not told to, one told to whose profile grants push, and one told
to whose profile grants none: the rights its signature would claim, what the bundle says of
itself, the files it carries, and what is signed in which order. Not checked by anything
here: a build with push signed under a real profile, installed or started, and a push from
Apple's service.

`AssistantClosed` is checked by its own tests: what the device's key opens against the
example message the standard for web push itself gives (RFC 8291, section 5), which nothing
of Tilo made, and against messages the tests seal the way the person's server does; the name
of a thing against names worked out outside the package; and that a key prints nothing of
itself. `TiloSourceTests` hands the source a store that keeps the key in memory: no test
touches a keychain, so the keychain store itself is checked by nothing here, and neither is
anything the system does with a push.

`EntryPageShots` in the tests takes pictures of a site's entry page as the apps show it, with nothing on
screen, when it is asked to (`TILO_SHOTS_DIR`, `TILO_SHOTS_SITE`).

Not checked by anything here, because it needs a window or the system's own interface: how
the page looks in the window, the camera reading a code, the menu-bar item, the settings window, dictation, the share
extension at run time, a notification shown and answered through the system. And a passkey used in an app that is
associated with its site: that takes an App ID with the Associated Domains capability, which
nothing here makes. What is checked instead: that a build names its sites exactly when its
profile grants it, what the system answers a page in a build that names none, and the page's
sentence for it.

Tilo Assistant's Mac app is driven by its tests while it runs (`Tests/AssistantUITests`;
`Drive.swift` says how): the app as its program makes it, over a source that writes its calls
down, in a window that is made and never ordered onto a screen, in a process that may never
come to the front. Every key is a real key event: typing into the message field, Return and
Shift Return, the menus' keys through the menus themselves, Return and Escape in a sheet. A
SwiftUI control answers no mouse event in a window that is not on a screen, however it is
handed one, so a control is pressed by its accessibility press, after the check that it is
what stands at its own centre. Checked so: the field (formatting in place, what is sent, a
pasted paragraph, a long text that scrolls inside it), the sidebar and a sub-agent's window,
the two ways to start a sub-agent, the answers under a message, a task's replies, the panel's
four parts and an event's switch, the assistant view, every line of the menus with its key,
the menu-bar item's lines, where the window's three buttons stand, and that what is typed
into the protected field reaches the source's `keep` and nothing else (no message, no draft,
no log line, not the pasteboard, nothing the app keeps). Not checked by it, because only a
window on a screen shows it: the pointer's own way into the views (hover, how a press looks,
a drag), the two pop-up menus, the search popover, the file picker and dictation (each puts a
window of the system's on the screen), whether the conversation keeps its end in view while
the field under it grows, and the app's first start on a screen. What the Mac's window says
of itself is read by the same tests (`SaidTests`), part by part.

The iPhone's screens are drawn in a simulator that shows no window, at the default and at the
largest text size, on a large and on the smallest phone, and read out there (`said`). Not
shown by a simulator: a notification on the lock screen with its buttons, a live activity on
the lock screen and in the island, and the picture the system itself keeps of the app as it
leaves the front. Those take a phone in a hand.

`TiloLive` is checked by its own tests, which run the engine against a page that speaks as
Tilo's does: where the page stands, a page that stopped or cannot draw, a change, reads,
actions and their refusals, a second mount, the frames of a page nobody sees, and the person's
own providers behind a device that answers carelessly (no key and no secret comes back, and a
frame gets none of it). `AssistantTilo` is checked the same way, call by call of the frame's
interface.

Against a real site, with throwaway people and nothing on a screen, the real programs did
this ten times of ten, on the page build `b341b6ba19b3be4cc4d7` (an older tree than this one).
`TiloLiveProbe` paired from a link and came back as the same device; printed a message the same
person typed elsewhere, told by the page and never asked for on a clock; sent through Chat's
own send; and showed a live document that took an edit made elsewhere and typed keys that
landed in the record. `AssistantMac --drive`, the app's own program with no window, signed in,
filled its conversation, took a message typed elsewhere and sent one. No real program has yet
used the person's own providers, answered a question that waits or kept a secret from the
protected field: that tree has none of the three, and they wait for a site of this tree.

In a simulator `TiloLivePhoneProbe`, an iPhone app with the engine unseen in its root view,
paired as a new device with the code compared four times of four and kept its device's key,
and came back as the same device with nobody asked three times of three; a device nobody
paired was stopped while joining. No simulator offers a page a graphics adapter, so there the
page is let in and never opens: reading, a change, sending and the live view have not run on
an iPhone's system, and only a phone shows them. A simulator program needs a keychain group
of its own for any of this (`build.sh` gives the apps theirs): without one a web view cannot
keep the key that wraps a page's keys, and the page stops while joining.
