# Tilo Shell for Mac and iPhone

Tilo Shell is a native shell around Tilo's own page, for the Mac and the iPhone, from one Swift
package. It shows Tilo as a whole: the site as a browser opens it, with the desktop and every
app.

Only Tilo's web runtime draws a live document or a live view inside a conversation, so the shell
does not draw anything of Tilo a second time. It shows Tilo's page in the system web view ("Links
opened full tab" in
[the web runtime's guide](../../crates/core/tilo-browser-runtime/WEB_RUNTIME.md) is the page that
shows one app) and adds what only the system can do.

The shell's programs name no app. `build.sh` gives a bundle its name, its identifier and what it
shows; no source file of the shell and no file of `Bundle/` names one of the apps, and a test
keeps it so.

```
Package.swift        the targets, and the oldest system the app runs on
Sources/ShellCore    decided without a web view: the site and what of it is shown, which
                     addresses stay inside, what the page's title says, the hand-over from the
                     share sheet, the sentences, the script that joins page and system
Sources/ShellWeb     the web view that holds the page, notifications, dictation, the check
Sources/TiloMac      the Mac app: one window, the menu-bar item, settings
Sources/TiloPhone    the iPhone app
Sources/TiloShare    the share extension of both
Sources/TiloNotice   the notice service extension: opens what a push sealed for this device
Bundle/              what the bundles carry, with room for an app's name and identifier:
                     Info.plists, entitlements, the English sentences, the phone's page in
                     Settings, the icon's one drawing
build.sh             builds the app, signs, checks, installs
```

## The app

|               | Tilo Shell                            |
| ------------- | ------------------------------------- |
| Built by      | `build.sh mac …`, or `build.sh …`     |
| Identifier    | `so.tilo.assistant.shell` (see below) |
| Programs      | `TiloMac`, `TiloPhone`, `TiloShare`, `TiloNotice` |
| What is shown | everything of the site, in a web view |
| Bundle files  | `Bundle/`                             |

**Tilo Shell** opens the site's own entry, as a browser tab does. `TILO_SHELL_APP` at build time,
or `-TiloApp <id>` once on the command line, names an app that is in front on the desktop when it
opens.

**A shell held to one app.** The shell's programs can also show one app alone. No word of
`build.sh` builds such a bundle today. A bundle whose `Info.plist` has `TiloAlone` shows `TiloApp`
alone, whatever a setting or the command line names (`ShellSettings.swift`). The page it opens is
the one-app page, which has no desktop. Beyond that the web view is held to it (`PageHost.swift`,
`SitePolicy.isOwn`):

- The way in may lead anywhere in the site, because that is how a person gets in: its entry page,
  an invitation, a pairing link made on a desktop.
- Once the page says the person is in, an address of the site that is not the app's own page goes
  to the system's browser, like any address that is not Tilo's. That is where "Open in Tilo" in the
  one-app page's bar leads, and a link to another app's document.
- A page writes its own address without asking anyone. If the address it shows is no longer the
  app's own, or the person got in on another page, the page is put out of sight and the app is
  opened in its place. A site that leaves the app each time it is opened is said after the second
  time, not tried for ever.

It shows whatever the site's app of that name shows: on a site whose web bundle has no such app,
it shows what the one-app page says of an app it does not have.

## Tilo Shell's identifier

Tilo Shell was first built under the assistant's name, and it still carries the identifier from
then: `so.tilo.assistant.shell`. That is on purpose, for now. The identifier is what the system
keeps an app's data under. The site's storage is in the Mac's sandbox container and in the
iPhone's app data, and the keychain item WebKit wraps the page's device key with belongs to the app
that made it. A build under another identifier is another app to the system: it starts with
nothing, so everyone who uses Tilo Shell today would be signed out on that device.

Giving it an identifier of its own (`so.tilo.shell`) takes:

- One line, `id` of `shell` in `build.sh`. The share extension's identifier, the shared place, the
  keychain groups and the entitlements follow from it.
- Every device gets in once more. The device key cannot be carried over: it is a key that cannot be
  taken out, wrapped for the old app. A person pairs the new app from a device where they are
  signed in, and removes the old device from their devices.
- On an iPhone the new app installs beside the old one, which the person then deletes; the site
  typed into the old app's page in Settings is typed again. On a Mac the old container
  (`~/Library/Containers/so.tilo.assistant.shell`) and its group container stay until someone
  removes them.
- The system asks again for notifications, the microphone, speech recognition and the camera, and
  "Open at login" is chosen again.

The moment for it is the first build for the App Store, which needs an App ID of its own in any
case (see "Before the App Store"): then people pair once more, not twice.

## What the system adds

**The page, kept.** The web view keeps the site's data. The page holds its device key as a browser
key that WebKit wraps with a key from the app's keychain, so a person who got in once stays in for
as long as the app keeps its signature and its data. Getting in is the site's own flow (an
invitation, a pairing link, its sign-in); nothing hands a native identity to a page.

**The way in, on the site's entry page.** While the site keeps a device out, it shows its entry
page: what the site says to someone it does not know, and what such a person can do. What only the
system can do for them stands on that page, not beside the page: "Scan code" on a phone and "Paste
link", for the code and the link that "Pair a new device" shows on a device where the person is
signed in, with one sentence under them that says so. An invitation's link is pasted the same way.
The entry page is the page's own markup, in one dark look and the system's type, and it scrolls; a
native bar under it followed the system's appearance and looked like a strip stuck to a web page.
So the script puts the two buttons into the entry page's own column, with the entry page's measures
and colours (`Bridge.swift` says where on each of the entry pages, and why). A press comes back to
the shell, which only then asks for the camera, or reads the clipboard. A link is taken out of
whatever is written around it; a link into another site is refused on the entry page, under the
buttons, with the name of the site the app opens. A link the site's own computer made under a name
only it can use is read as the site's (see "One site"). The page writes where it stands on its root
element (`data-entry`): the buttons go once it is in and while a device of the person's is being
asked to approve this one, and a person who came in by a link is taken back to where the app
starts.

A link is also opened from the File menu on the Mac, by a long press on the phone's icon, which
offers the scan too, or by sharing it to the app. An entry page that draws these options itself
marks its element `data-way-in` and asks the shell through `__tiloShell.wayIn`; the shell then adds
nothing.

**One site.** The app shows the origin its settings name and nothing else (`SitePolicy.swift`).
Any other address goes to the system's browser. The script below runs in the page's main frame and
does nothing on another origin, so no other site's content reaches the system through it.

A link that is pasted, scanned or shared is followed only into that origin. One other spelling is
read as the site's own: a link whose site means "this computer" (`localhost`, a name under it, a
`127.x` address, `[::1]`) on the site's own port. That is how the computer that serves a site
writes its pairing links and invitations when its page is opened as
`http://tilo.localhost:24120`, and no other device reaches such a name: on a phone it means the
phone. So the link can only be meant for the site the app opens, under the name other devices use
for it. It is opened in the app's own origin with its path and its secret as they were; and where
the app's site goes by a name on the network, a host in the link's route that also means "this
computer" (`#/@127.0.0.1:24100/w/…`) is written with the site's name and the host's own port,
which is where a site served beyond its computer serves its host. Nothing opens outside the app's
origin by this, so no secret reaches another site's page. Another port, and every other site, stays
refused.

**Notifications.** A system web view has no working Notifications API, so the script
(`Bridge.swift`) is one for the page: `Notification` with `permission`, `requestPermission()`,
`close()` and the `click` and `close` events, as `crates/platform/tilo-notify/web/notify.mjs` uses
them. What the page shows becomes a notification of the system; pressing it brings the app forward
and hands the page its choice. A reply typed into a notification lands in the message field.
Beyond a browser, a page's notification may name `actions` (`{action, title, type, placeholder}`,
up to `Notification.maxActions`): each is a button in the system's notification, a `type: 'text'`
one a field, and the `click` event then carries `action` and `reply`. On the Mac the page answers
such a button without coming forward.

**The menu-bar item** (Mac). Closing the window leaves the app in the menu bar, and the page goes
on running behind it. The item shows the count the page writes into its title, with the line the
page wrote, and offers the window, dictation, settings and "Open at login". Opening at login is the
person's choice; nothing registers the app before they ask, and nothing of it runs after Quit.

**Dictation.** Speech becomes words on the device (`Dictation.swift`); where the system cannot
recognise the language by itself, dictation is not offered. A panel shows what was heard, and
Insert puts it where the person writes.

**The share sheet.** The extension takes words or one web link from another app and leaves them
for the app, which puts them into the message field the next time it is in front. A link into the
Tilo site itself is opened instead.

**Words into the message field** go in as one paste (`compose` in the script). Tilo's page takes a
paste as words wherever its own text has the caret, line breaks included; typed line by line, a
line break would send a message. Where nothing has the caret the words wait for the next field the
person presses, and never go into a password field.

**When there is no page**, the app says what is wrong and offers the one thing to do: try again,
or change the site. The site is the one setting (the Mac app's Settings, the phone's page in the
system's Settings), and `-TiloSite <origin>` on the command line is kept from then on.

## What the page and the host still owe

- **Live windows in the one-app page.** A page that shows one app draws no live windows inside a
  conversation yet; the desktop does. Tilo Shell shows the desktop, so it has them. A shell held to
  one app has them when the one-app page draws them.
- **The one-app page's bar.** The page that shows one app draws a bar over it with Share and "Open
  in Tilo". In a shell held to one app the second leads to the system's browser. A page that knew
  it was shown as an app of its own could leave the bar out.
- **Answers in a notification.** The page names no `actions` today, so a notice the page shows has
  no answer buttons of its own. When it does, they appear with no change here. A push's buttons
  are the thing's own, and answer through the log ("Told while the app is closed" below).
- **A typed way to post.** Words reach the thread through the field the person writes in. Posting
  on the person's word without a caret (a reply from a notification, a share sent straight away)
  needs an action of the page the script can call.
- **Notices while the window is in view but the app is not in front.** The page tells the system
  only while it is hidden.

## Told while the app is closed

While no page runs, the person's host tells them through Apple's push service. A push carries, in
the open, only that something waits and a name for the thing that only this device can tie to it;
the agent's name, the words, the Important mark and the buttons are sealed for a key of the
device's own (`apple_push.rs`, `sealed`). The notice extension (`TiloNotice`) opens the sealed
part with that key, keeps it in the one keychain group the app shares with the extension, and
shows the notice under the name, and a notice the page shows under that tag takes its place: the
page names its own notice of a thing with the push's name (`pushName`). Signing out unregisters
the address, so a closed app is told nothing until someone signs in there again.

The key and the address are handed over by the page's session, not by the shell directly: the
person's identity is the page's device, so only the page can call `tilo.telling`
`addresses#register`. The shell has the token Apple gave it and makes the key; the bridge gives
the page where the app is reached (`__tiloShell.push`) and the page hands it to the host, as the
phone's own app hands its endpoint over. What a push answered reaches the page the same way
(`__tiloShell.told`): the page answers it through the log's own `acknowledge`, with the button's
id as how, and opens what the thing is about. A push that shows nothing takes away what it
showed.

At Apple this takes an App ID with Push Notifications and a profile that grants
`aps-environment` (development or production; none and the build has no push and says so), and
for the host a key from the same team. `build.sh` builds it when `TILO_APPLE_PUSH` is set.

## Build, check, run

`build.sh` builds Tilo Shell.

```sh
dist/apple/build.sh test                         # the package's tests; they open no window
dist/apple/build.sh mac --check --live           # build, sign, and ask the app what it can tell
dist/apple/build.sh simulator --live             # the iPhone app, asked in a simulator with no window
dist/apple/build.sh iphone --install             # build, sign, and put it on the one reachable iPhone
open "target/apple.noindex/shell/mac/Tilo Shell.app"  # start the Mac app
```

Builds land in `target/apple.noindex/shell/`, where Spotlight does not look: a build is offered to
a person as an app only once someone puts it where apps go. A bundle that is open is never
replaced; quit it first.

`build.sh` reads who signs from `TILO_APPLE_SIGNING_IDENTITY`, the iPhone app's provisioning
profile from `TILO_APPLE_PROFILE` and the site from `TILO_SHELL_SITE` (default
`https://tilo.sh`); its head lists the rest, among them what passkeys take ("Passkeys" below). The
signing team is read from the identity and written into the bundles, never into the sources.
Without an identity the Mac app is signed by nobody: it runs on the Mac that built it, a later
build cannot read the keys it kept, and it has no share extension.

A build says which build it is, and nobody sets that by hand. Its version is the release's name
(`dist/VERSION`, or `TILO_VERSION`), and many builds are of one release. Its build number is the
number of commits the commit that is built stands on, so every later head of `main` reads a higher
one: the Mac's About window shows the two as `Version 0.1.13 (49852)`. The commit's own name is in
every bundle's `Info.plist` as `TiloCommit`, with `+changes` behind it when the tree that was built
differs from that commit. A tree that is no git checkout, or one that holds only part of its
history, builds nothing and says why: it could not say which build it is.

The app's icon is drawn by `build.sh` at every size a bundle carries when the bundle is made,
from `Bundle/AppIcon.svg`, Tilo's mark on its light ground. The repository keeps no pictures.

`--check` runs the app with nothing on screen and prints one line per fact: who signed it, whether
it is sandboxed, whether the web view offers a secure context, WebGPU with an adapter, WebTransport
and its datagrams, whether a key the page stored is still there on a second run, whether a notice
and words cross between page and system, what the build opens with the settings kept for it on this
device, whether the app may use the site's passkeys, and whether the site and its host answer.
`--live` also opens and closes one WebTransport session at the host the site names; it opens no
stream, so no person and no workspace is touched. A simulator has no graphics adapter, so it fails
that one line and cannot show Tilo; a real iPhone is asked from a Mac:

```sh
xcrun devicectl device process launch --console --device <id> so.tilo.assistant.shell --check --quit
```

## Passkeys

Tilo's page keeps a person with a passkey and signs them in with one ("Use a passkey"). In a
browser that is the browser's own business. In an app's web view the system hands the page a
passkey only for a site the app is associated with, and refuses every other request before it shows
anything
([Supporting passkeys](https://developer.apple.com/documentation/authenticationservices/supporting-passkeys),
"Use passkeys in a web view"). Associated means both of these:

- **The app's signature names the site.** Its entitlement
  `com.apple.developer.associated-domains` holds `webcredentials:<host>`. `build.sh` writes one
  entry for each host in `TILO_APPLE_PASSKEY_SITES` (default: the host of `TILO_SHELL_SITE`) and
  lists the hosts in the bundle's `Info.plist` (`TiloPasskeySites`). It does so only where the
  provisioning profile grants the entitlement: an iPhone installs no app, and a Mac starts none,
  whose signature claims more than its profile. Such a profile is made for an App ID of the app's
  own identifier with the Associated Domains capability. A profile for every identifier of a team
  (`TEAM.*`), which is what signs a trial build, never grants it. A profile for the app's own
  identifier covers the app alone, so the iPhone's share extension is then signed with a profile of
  its own (`TILO_APPLE_SHARE_PROFILE`; the team's profile for every identifier will do). The Mac
  app runs without a profile and is given one for this alone (`TILO_APPLE_MAC_PROFILE`). A build
  that may name no site says "no passkeys in this build" and is otherwise the same app.
- **The site names the app.** `https://<host>/.well-known/apple-app-site-association` lists the
  app's application identifier (`<team>.<identifier>`) under `webcredentials`. Tilo's web entry
  serves it once its deployment names its apps (`TILO_WEB_APPLE_APPS`, in
  [its start script](../../crates/core/tilo-browser-runtime/container/entrypoint.sh)). Apple's
  servers fetch it over `https`, following no redirect, and a device asks them when the app is
  installed and about weekly after. The address has no place for a port: the file is asked for on
  443, whatever port the site itself answers on.

A passkey belongs to the name of the site it was made at, because the page names its own host as
that site (`web/identity.mjs`). `tilo.sh` in a browser and `tilo.sh` in the app are one site, so
one passkey opens both. The same Tilo under another name is another site, with passkeys of its own:
one made on a Mac at `tilo.localhost` opens nothing at the name a phone reaches that stack by. A
site on this computer (plain `http`, a name under `localhost`) is associated with no app. A person
may name any site in the app's settings, and passkeys work at the ones the build was signed for.

**A site Apple's servers cannot reach**, such as a test stack on a private network, is named with
`?mode=developer` after its host (`TILO_APPLE_PASSKEY_SITES="tilo.example?mode=developer"`), and
the device then reads the file from the site itself. Apple allows this for a build signed with a
development profile, on a device where the person switched it on: on an iPhone in Settings,
Developer, "Associated Domains Development" (the Developer page is there once the phone was used
with Xcode), on a Mac with `swcutil developer-mode -e true`. A build for a store names no site this
way.

Where the app is not associated with its site, nothing but the passkey is missing. The page says
that passkeys cannot be used there, and to add the device from one where the person is signed in
("Pair a new device"): the way in that needs nothing from Apple. `--check` says which it is,
"passkeys of <host>": the app may use them, or the build's signature does not name the site, or it
does and the system still refuses (the site does not name the app yet, or the device has no
passcode).

## Before the App Store

The Mac app is sandboxed from the start with the entitlements `Bundle/Mac.entitlements` explains;
it listens on nothing, and updates nothing of itself. Still to do for a store build: an App ID of
its own (Associated Domains for passkeys, and an app group in place of the keychain group the
phone's share extension uses under a development profile; this is the moment its identifier
changes), a universal Mac binary, an icon drawn for each platform, the usage descriptions in every
language the app ships, and review notes that say what the page is and name each native part above.

## What is checked, and what is not

Checked with no window: the tests (the settings and what a build is held to, the policy, the
hand-over, the sentences against their catalogue, the script against a page that speaks as Tilo's
does, and the web view of a one-app build against pages that leave the app, and the way in against
pages that show the entry page as Tilo's do), the Mac app's `--check` in its real signature and
sandbox, and the iPhone app's in a simulator.

`EntryPageShots` in the tests takes pictures of a site's entry page as the app shows it, with
nothing on screen, when it is asked to (`TILO_SHOTS_DIR`, `TILO_SHOTS_SITE`).

Not checked by anything here, because it needs a window or the system's own interface: how the page
looks in the window, the camera reading a code, the menu-bar item, the settings window, dictation,
the share extension at run time, a notification shown and answered through the system. And a
passkey used in an app that is associated with its site: that takes an App ID with the Associated
Domains capability, which nothing here makes. What is checked instead: that a build names its sites
exactly when its profile grants it, what the system answers a page in a build that names none, and
the page's sentence for it.

The iPhone's entry page is drawn in a simulator that shows no window, and read out there.

Being told while the app is closed is checked by `ShellCoreTests`: what the device's key opens
against the standard's own example (RFC 8291, section 5) and against a push the host's sender
sealed (`Fixtures/SealedPush.json`, kept in step by a test in `apple_push.rs`); the name against
the host's; the extension's notice, under that name; the address handed over; and that a key
prints nothing of itself. `ShellWebTests` checks that the bridge gives the page the address and
what a push answered.

Not checked by anything here: a push from Apple's service, and anything the system does with one.
That takes an App ID with Push Notifications, a profile that grants `aps-environment`, and the
host's own key: a real Mac or iPhone, and nothing a build on this machine makes.
