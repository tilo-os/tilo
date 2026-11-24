# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-playback = Playback

d-c6fmojnr = Output
    .doc = One place a watching or listening session's sound and picture come out: this device, a speaker in a room, or a cast target.
    .noun = { $count ->
        [one] output
       *[other] outputs
    }

d-mtgbfbe3 = Kind
    .doc = `local`, `speaker` or `cast`.

d-q2afggjg = Device
    .doc = The device, speaker or target as the system names it; empty for this device.

d-duy3y22z = Name
    .doc = What people read of it: "Kitchen", "Living room speaker".

d-sz4b5nfh = Volume

d-ewrhlhvs = Synced
    .doc = Whether it plays in step with the session's place, or lags behind a cast target.

d-c6fmojnr-filter-volume = Volume

d-c6fmojnr-filter-synced = Synced

d-kf7cpzbo = Item
    .doc = One item the session can play, with what a person reads of it.
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-jsytyrgy = Ref
    .doc = The app's reference to the item (Video's Clip, Music's Track).

d-hbrdxjws = Title

d-3sty6xvi = Artist

d-wzxeznnc = Cover

d-iueovkzr = Source
    .doc = Where its bytes play from: the kept content, else the source.

d-ggr7nnxj = Length
    .doc = How long it runs, in seconds, when the app knows it.

d-kf7cpzbo-filter-length = Length

d-sa6rjoaz = Progress
    .doc = Where an item was left, so a later opening continues there.
    .noun = { $count ->
        [one] progress
       *[other] progresses
    }

d-ijcc2upy = Ref
    .doc = The app's reference to the item, as the session's `track` names it.

d-owteykf6 = Position
    .doc = Where it stood, in seconds.

d-q64zycak = Updated
    .doc = When the place was last written.

d-sa6rjoaz-filter-position = Position

d-sa6rjoaz-filter-updated = Updated

d-omcimail = Playback
    .doc = What plays and where it stands: one session of the workspace, written only by actions, so every device opens on the same thing at the same place and the system shows it as the person's player.
    .noun = { $count ->
        [one] playback
       *[other] playbacks
    }
    .untitled = Nothing playing

d-hnwowp42 = Track
    .doc = The app's reference to what plays, empty when nothing plays.

d-sx7vtcfl = Title

d-eejl5scp = Artist

d-rjbz4skr = Cover

d-tafuqjic = Source
    .doc = Where the sound or picture plays from: the kept bytes where they are, else the source.

d-xcyilpu4 = State
    .doc = Whether it plays.

d-p2dmm6gi = Outputs
    .doc = Every place it comes out, in the order it was put there: this device first, then the speakers and cast targets the person added.

d-pkexjxh3 = At
    .doc = Where it stands and how long it is, in seconds.

d-min4lfee = Length

d-udayujnj = Volume

d-4t7z5bco = Shuffle

d-2l7pa2rs = Repeat
    .doc = `off`, `all` or `one`.

d-wwdrlrqy = Queue
    .doc = The items that follow, in order, and which one plays.

d-krqh6s6b = Queue at

d-eixby7cm = Progress
    .doc = Where each item was left: what a later opening resumes from, one entry per item.

d-cvxa6lh7 = Playback state
    .doc = Whether the player stands still, plays, or has nothing at all.

d-6lrdx5em = Stopped

d-x3vz7lp5 = Playing

d-zb4h4oj4 = Paused

d-yeogsqcw = Play list
    .doc = Plays the items `items` names, in the order given, from the one at `at`: the queue is what follows, and the item at `at` plays.

d-yeogsqcw-items = Items

d-yeogsqcw-at = At

d-ovcgpk6a = Set current
    .doc = The session shows `item`: its fields are the item's, so a shared next or previous never leaves the title, cover or source of the one before it.

d-ovcgpk6a-item = Item

d-d4zwmel2 = Set playing
    .doc = Plays or pauses what plays, as a player's own control and a media key ask it.

d-d4zwmel2-on = On

d-osacia6u = Set source
    .doc = Where what plays comes from, as the app that knows it answered again: a video's other quality, or the address it was asked for once more.

d-osacia6u-source = Source

d-pk7bfu5k = Set length
    .doc = What the player reports of its length, once the decoder knows it.

d-pk7bfu5k-seconds = Seconds

d-i2veyrzn = Remember position
    .doc = Keeps the session's place and the item's as `when` records them.

d-i2veyrzn-seconds = Seconds

d-i2veyrzn-when = When

d-xh3lfbpp = Set position
    .doc = Where the player stands, as it reports it, so a later opening resumes there: the session's place and the item's, kept so another device or a later opening continues from it.

d-xh3lfbpp-seconds = Seconds

d-3gntm43c = Set share
    .doc = Where the player stands, as the share of its length a player reports (`0..=1`), kept so a later opening resumes there.

d-3gntm43c-share = Share

d-lk75gedd = Resume
    .doc = Moves what plays to where `ref` was left, so a video or a song continues where the person stopped instead of beginning again.

d-lk75gedd-ref = Ref

d-g5ehiohm = Play
    .doc = Plays what is chosen, or begins again where it stands.

d-2u5srydi = Toggle
    .doc = Plays or pauses what plays.

d-dd6dt32e = Pause
    .doc = Pauses what plays and starts it again where it stood ([`toggle`]'s own job).

d-i5ctxqgd = Stop
    .doc = Stops what plays: it plays on no device, and the shell shows it no more.

d-fxxarqxu = Next
    .doc = The item after this one: the queue's next, or the first when repeat is on.

d-jmnwv624 = Previous
    .doc = The item before this one.

d-sueidkl2 = Select
    .doc = Plays the queued item at `at`, keeping the queue as it stands.

d-sueidkl2-at = At

d-o5tnzj23 = Seek by
    .doc = Moves what plays by `seconds`, for the live card's two seek controls.

d-o5tnzj23-seconds = Seconds

d-ftrjircx = Seek
    .doc = Moves what plays to `place`, a fraction of its length, and keeps the place so a later opening continues there.

d-ftrjircx-place = Place

d-4ndxwmgz = Set volume
    .doc = How loud the session is.

d-4ndxwmgz-level = Level

d-bpl24zko = Mute
    .doc = Silences the session, or brings back the volume it had.

d-kfomtoxg = Shuffle toggle
    .doc = Shuffles the queue, or puts its order back.

d-kfomtoxg-shuffle = Shuffle

d-6lghyo6k = Cycle repeat
    .doc = Moves repeat on: off, all, one.

d-kw3s3ite = Queue add
    .doc = Adds `item` after what plays.

d-kw3s3ite-item = Item

d-u7afdn3b = Play next
    .doc = Adds `item` right after what plays.

d-u7afdn3b-item = Item

d-v4y4chqp = Queue remove
    .doc = Takes the item at `index` out of the queue.

d-v4y4chqp-index = Index

d-v4y4chqp-queue_at = Queue at

d-tcakivo7 = Queue move
    .doc = Moves the item called `from` to stand where `to` stands: the queue row a person dragged, and the ids its reorder reports.

d-tcakivo7-from = From

d-tcakivo7-to = To

d-qbjrwcgo = Add output
    .doc = Puts the sound on one more place: a speaker in a room, or a cast target.

d-qbjrwcgo-output = Output

d-u7fcmuqw = Remove output
    .doc = Takes the place at `index` out of the session; this device is never taken out.

d-u7fcmuqw-index = Index

d-wahda3bo = Move to
    .doc = Moves the place at `from` to where `to` is: "continue on the speaker" is moving the session from this device to it.

d-wahda3bo-from = From

d-wahda3bo-to = To

d-gomcpa5y = Set output volume
    .doc = Sets how loud the place at `index` is.

d-gomcpa5y-index = Index

d-gomcpa5y-level = Level

d-4zpe77fa = Live
    .w-nothing-playing = Nothing playing
    .w-previous = Previous
    .w-pause = Pause
    .w-play = Play
    .w-next = Next
    .w-back-15-seconds = Back 15 seconds
    .w-forward-15-seconds = Forward 15 seconds
    .w-stop = Stop
