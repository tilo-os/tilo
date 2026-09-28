# Sending feedback about Tilo

Tilo is alpha software, and your reports shape what gets fixed. This page
explains how to send one, what it contains, and who can see it.

## Open the feedback window

Any of these opens **Say something about this**:

- Press **Super+M**.
- Open Quick Settings and choose **Feedback**.
- Press **Ctrl+K** (or **Super+K**), type "say something", and choose
  **Say something about this…**

If an app quits unexpectedly, Tilo shows a notification with **Tell us what
happened**. It opens the same window with the app already filled in.

The same window also sends a remark to a colleague about something in a
shared workspace. Only the destination differs.

## 1. Pick what it is about

Point at the part of the screen your report is about: a button, a field, a
menu, a whole window. This tells the team exactly what "this" means, so
there is no guessing from a description.

- Click to pick one thing. Hold **Shift** to add more.
- Drag to pick everything in an area.
- The arrow keys walk to the enclosing item, a child, or a neighbour.
- Press **Enter** or choose **Done** when you are finished.

Picks work across apps, so one report can be about a button in one window and
an error in another. Fields marked **personal** are shown to you but are
masked in anything that is sent.

## 2. Write what happened

Say what you did, what you expected, and what happened instead. A sentence or
two is enough.

## 3. Show what happened (optional)

A recording lets the team see the problem instead of reading about it. It is
not a screen video. It is a record of what the apps did: which windows were
open, what you pressed, what changed, and any error that appeared. You have
two ways to make one:

- **Record from now.** The **Recorder** joins the workspace and records until
  you choose **Stop**. While it records, it is listed among the workspace's
  collaborators as an agent, on everyone's screen, and a **Recording** pill
  stays on screen. Removing it from the list also stops it. You can record
  any workspace you can see, including one you don't manage, unless its
  administrator turned off **Members may record** in Workspace Settings ›
  Agents. Tilo then tells you so.
- **Use the last 2 minutes.** This only works if you turned on **Keep the last
  2 minutes from now on** beforehand. That setting is off until you turn it
  on. The last two minutes are kept in memory on this device only and are
  never written to disk or sent on their own.

Apps that protect their content, such as a password manager or an
authenticator, contribute nothing to a recording. The review shows them as
"Protected — nothing of it was recorded".

**Include screen frames** adds pictures of the recorded windows, one window
at a time, never your whole desktop. Personal fields are masked in them.
Without this option, Tilo still captures frames to check the recording
against a replay (below), but keeps them on your device. It offers to attach
only the areas where the replay looked different.

## 4. Review and hide things

Nothing is sent before you review it. **Review** walks through the recording
step by step, in plain sentences such as "You opened Notes", "You pressed
Save", "*Couldn't save* appeared". Each step shows what will be sent.

- **Click any word** to hide it. Click a text in the preview to hide all of it.
- **Cut step** leaves a whole step out.
- Some things are already hidden before you start: anything an app marks as
  personal (names, message bodies, contact details), and anything that looks
  like an email address, phone number, IBAN, card number, password, or access
  token. You can hide more, but you cannot un-hide these.
- Passwords and other secrets are never part of a recording at all.

Hidden text is replaced with blocks of the same length (▮▮▮▮▮). This way a
name that was cut off on screen still looks cut off to the team, without
showing the name.

The review also replays your recording on your device and compares it with
what you saw. "Replays exactly" means the problem is in how the app behaves.
"Replay differs below the app" points at drawing, fonts, the graphics driver,
or scaling, and offers those frames to attach.

## The review model

A small AI model can help with review. It suggests more things to hide and
writes clearer step titles. The first time you review, Tilo may show
**Recommended for this device** with a model it can run on your computer.

- **Set it up** downloads and starts it. The size and license are shown
  before anything is downloaded.
- **Not now** skips it. Review still works without a model. It hides the
  things listed above, and step titles come from button names.

A model on your device sees the whole recording, and nothing it is shown
leaves your computer. Review runs only on your device unless you explicitly
choose a hosted model for **Feedback review** in Settings → AI. Even then, the
hosted model gets placeholders such as ‹name› instead of your personal
details. Passwords and secrets never reach any model.

## 5. Choose where it goes

Under **Send to**:

- **Tilo team (private).** Only you and the Tilo team can read it. This is the
  default. Replies arrive as a normal chat conversation.
- **Tilo Community #feedback (public).** Anyone in the community can read it,
  react to it, and reply. The team is there too and sets each report's status
  (Planned, In progress, Fixed in generation N, and so on). When a fix ships
  in the build you are running, the report shows **Your build contains the
  fix**.

You can also send it to a person or a workspace you share.

Then press **Send**. Replies arrive in Chat, and **Open conversation** takes
you there.

## The Tilo Community

The community is a public room where alpha users and the Tilo team meet. You
can read other people's reports there, vote on them with reactions, reply to
them, and get to know people you might later message directly or invite to a
workspace.

- You are in it from the start: once your identity is set up and you are
  online, Tilo joins it for you and opens your private conversation with the
  Tilo team. First setup says so in one line; nobody asks.
- Everyone in it is a Commenter: they can post, reply, and react. Only the
  Tilo team can change a report's status.
- The community is public and **not encrypted**. Treat anything you post
  there like a post on a public forum.
- The community appears as a workspace named **Tilo Community**. To leave
  it, choose **Leave the community** in Settings > Users, or run the "Leave
  the community" action. Leaving applies to all your devices: none of them
  joins it again until you choose **Join the community** there. Whether you
  are in it or not, you can choose the private destination for everything
  you send.

## What Replay recording means for your privacy

The people who receive your report can press **Replay recording** on it (or
find "Replay recording" with Ctrl+K). This reopens the recorded app on their
computer and plays back what you did, step by step, as you saw it, in a
temporary room that is removed when they close it. They can pick items in the
replay, just as you did.

This is not the same as **Save recent replay** (Super+Shift+R), which saves
a short screen capture on your own computer and is not part of feedback.

What they replay is the reviewed copy, not your original document. Everything
you or review hid stays hidden, cut steps are gone, and protected apps are
not there. Your real files are never sent.

Before you send, remember:

- **Private:** the Tilo team can replay it.
- **Public:** anyone in the community can replay it.
- **Withdraw** (in Chat, on your report) removes it from the room. People who
  already opened it keep their copy, so review carefully before sending.

## Quick answers

**Does Tilo send anything automatically?** No. Nothing is recorded unless you
start a recording or turn on the two-minute buffer, and nothing is sent until
you review it and press Send.

**Can I send feedback without a recording?** Yes. Pick, write, and send.

**Where is the recording kept before I send it?** On your device only.
