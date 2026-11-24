# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-feeds = Feeds

d-so-tilo-feeds-keywords = rss, atom, feeds, feed, news, newsreader, newsfeed, subscriptions, akregator, gemini, gemtext, nachrichten, abos

d-rapg7cgp = Folder
    .doc = A folder that keeps related subscriptions together in the sidebar.
    .noun = { $count ->
        [one] folder
       *[other] folders
    }

d-ygksbjy5 = Name

d-56bq23fl = Feed
    .doc = One subscription: the feed a person reads, and what this workspace knows of it.
    .noun = { $count ->
        [one] feed
       *[other] feeds
    }

d-ruq4ynjw = Name
    .doc = What it is called on screen: its own title once a refresh brought one, and its site's address until then.

d-jdxu5vhb = Url
    .doc = The feed's address, which a refresh reads again.

d-kitws4yh = Site
    .doc = The site it comes from.

d-ecrfpxxe = Folder
    .doc = The folder it is listed in; none for the top of the sidebar.

d-lb5mig4n = Format

d-2wtntp4k = Pin
    .doc = The certificate a `gemini://` server presented first, as hex: the one thing that stops anyone on the path from serving the feed in its place.

d-py34q4xk = Trouble
    .doc = Why the last refresh failed, in words; empty while it stands well.

d-56bq23fl-filter-folder = Folder

d-56bq23fl-filter-format = Format

d-a3monasj = Article
    .doc = One article a refresh brought in: what the feed says of it, and what its readers decided.
    .noun = { $count ->
        [one] article
       *[other] articles
    }
    .untitled = Untitled article

d-chbpw5pg = Feed
    .doc = The subscription it came from.

d-swmvsojn = Title

d-gvntfpn6 = Author
    .doc = Who wrote it; empty when the feed does not say.

d-p5lxkp7w = Url
    .doc = The address of the article's own page.

d-s2wxy2go = Published
    .doc = When it was published, where the feed says.

d-ok5mmsqb = Summary
    .doc = Its first line, as a row reads it.

d-2yi7idis = Body
    .doc = What it says, as the feed sent it.

d-y7lqgqyr = Read

d-hqmrnoz5 = Starred

d-4nrqtndr = Tags
    .doc = The readers' own labels: lower case, without `#`.

d-a3monasj-filter-unread = Unread

d-a3monasj-filter-starred = Starred

d-a3monasj-filter-feed = Feed

d-a3monasj-filter-tags = Tags

d-tirekt2c = Reader
    .doc = One person's reading: the place they are reading in, and the size, face and paper the article's page is set in.
    .noun = { $count ->
        [one] reader
       *[other] readers
    }

d-tqbu72hj = Place
    .doc = The place in the sidebar they are reading: `all`, `unread`, `starred`, `feed:<id>`, `folder:<id>` or `tag:<name>`.

d-lx6zg57h = Scale
    .doc = The text's size as a multiple of the reading base, between 0.8 and 2.

d-um6nclsf = Face
    .doc = The face the article is set in: `serif`, `sans` or `mono`.

d-to5unuga = Paper
    .doc = The page behind the words: `system` follows the theme, or a fixed `light`, `sepia` or `dark` page for long-form reading.

d-tirekt2c-filter-scale = Scale

d-hkyhrlx4 = Refresh
    .doc = How far a refresh has got, and how the last one ended.
    .noun = { $count ->
        [one] refresh
       *[other] refreshes
    }

d-crwgoxd6 = Job
    .doc = The job the device is running, by its id; empty while nothing runs.

d-dzn4ssl3 = State
    .doc = `running` while the device reads the subscriptions, and `done` once it has.

d-7d33zhis = Done

d-mq3vs354 = Total

d-pvpr56lq = Fresh
    .doc = How many articles the last refresh brought in.

d-gbspv2g7 = Failed
    .doc = How many subscriptions it could not read.

d-dbqvtyzb = Note
    .doc = Why it could not start, in words; empty otherwise.

d-hkyhrlx4-filter-done = Done

d-hkyhrlx4-filter-total = Total

d-hkyhrlx4-filter-fresh = Fresh

d-hkyhrlx4-filter-failed = Failed

d-u5mhrzrx = Library
    .doc = The workspace's feeds: its folders, its subscriptions, the articles they brought, and how each member of the workspace reads them.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-urf3jsnz = Folders

d-ba25drsv = Feeds

d-mymgo65x = Articles

d-53g3dh5y = Readers
    .doc = The workspace's readers: one record each, its own to its reader alone.

d-ptibzrny = Refresh

d-u5mhrzrx-filter-refresh = Refresh

d-4y3xzmua = Ask
    .doc = The subscriptions one refresh is asked for.
    .noun = { $count ->
        [one] ask
       *[other] asks
    }

d-ajc3gacu = Feeds

d-jl5g75cq = Ask feed
    .noun = { $count ->
        [one] ask feed
       *[other] ask feeds
    }

d-wvgxfoj6 = Url

d-rzd7dr5x = Pin

d-wptdjxom = Job
    .doc = How far the device has got.
    .noun = { $count ->
        [one] job
       *[other] jobs
    }

d-vxpbania = State

d-qen3hx3f = Done

d-fm7ik3wg = Total

d-krtut5p6 = Ready

d-wptdjxom-filter-done = Done

d-wptdjxom-filter-total = Total

d-4mwlj5jz = Taken
    .doc = One feed's answer, taken as it lands.
    .noun = { $count ->
        [one] taken
       *[other] takens
    }

d-karmfgtt = Ok

d-mkw76ot4 = Title

d-vxvxtmdp = Pin

d-rcxpbdkb = Reason

d-tkh3vj2o = Status

d-3oef7xt5 = Message

d-in2no76u = Articles
    .doc = Each article's own JSON, as text: a list of records holds resources, which JSON text makes none, so each is read into its record here.

d-4mwlj5jz-filter-ok = Ok

d-4mwlj5jz-filter-status = Status

d-tl62g2cq = Pulled
    .doc = One article a feed lists.
    .noun = { $count ->
        [one] pulled
       *[other] pulleds
    }

d-qablmehl = Title

d-5sdtzscn = Author

d-ae5rtinu = Url

d-gmi4oufp = Published

d-pj5brijd = Summary

d-t2byonw5 = Body

d-tl62g2cq-filter-published = Published

d-mvrkvcfa = Take ask
    .noun = { $count ->
        [one] take ask
       *[other] take asks
    }

d-5mcvbcwk = Job

d-ixh37lm6 = Url

d-cdqjjp54 = Outline
    .doc = What an OPML file lists.
    .noun = { $count ->
        [one] outline
       *[other] outlines
    }

d-esmrzlts = Title

d-fhfn2dm2 = Url

d-akah2ujo = Site

d-otz67kho = Folder

d-amdvzfos = Outlines
    .noun = { $count ->
        [one] outline
       *[other] outlines
    }

d-z646rsds = Outlines
    .doc = Each outline's own JSON, as text.

d-kzsmsaqw = Opml ask
    .noun = { $count ->
        [one] opml ask
       *[other] opml asks
    }

d-vg5idgn5 = Text

d-5xejf43x = Opml write
    .noun = { $count ->
        [one] opml write
       *[other] opml writes
    }

d-pd5lnepq = Feeds

d-cyxxthpb = Opml feed
    .noun = { $count ->
        [one] opml feed
       *[other] opml feeds
    }

d-5w7b7tgv = Title

d-5u77j2s2 = Url

d-igyb7lxx = Site

d-gjs6qx4q = Format

d-367at7x3 = Folder

d-6dcuxfum = Opml text
    .noun = { $count ->
        [one] opml text
       *[other] opml texts
    }

d-q2mosawv = Text

d-5nvadw6d = Feed format
    .doc = The format a subscription is read in.

d-aasdudc2 = An RSS feed
    .doc = An RSS feed.

d-5hhba3hx = An Atom feed
    .doc = An Atom feed.

d-akvlsxee = A JSON Feed
    .doc = A JSON Feed.

d-ccbdoa7f = A Gemini capsule
    .doc = A Gemini capsule.

d-ndtl6ntc = Start reading here
    .doc = Gives the person a reading of their own here, if they have none yet: the place they read in and how the article's page is set.

d-5hhcf7z5 = Arrive
    .doc = Opening Feeds gives the person a reading of their own.

d-hzsnnlnm = Read on open
    .doc = Opening an article reads it, as every path to it does.

d-wcne5wpz = Show a place
    .doc = Reads from `place` in the sidebar.

d-wcne5wpz-place = Place

d-ojrglzv3 = Show tagged articles
    .doc = Shows the place a tag's articles stand in.

d-ojrglzv3-tag = Tag

d-cdpf55sg = Show the article's feed
    .doc = Shows the place an article's feed lists it in, so a search hit or an agent's `open` lands its row in the list beside it.

d-w73iiryz = Add subscription
    .doc = Subscribes to a feed's address, or shows the subscription this library already has.
    .did = subscribed to a feed
    .w-a-subscription-needs-a-feed-s = A subscription needs a feed's address

d-w73iiryz-url = Url

d-w73iiryz-folder = Folder

d-jzvp3vyd = Unsubscribe
    .doc = Unsubscribes: the subscription leaves the sidebar, and what it brought in is taken away with it.
    .did = unsubscribed from a feed

d-jzvp3vyd-feed = Feed

d-apxioyg5 = New folder
    .doc = Starts a folder that keeps related subscriptions together; its subscriptions stay when it goes.
    .did = made a folder
    .w-a-folder-needs-a-name = A folder needs a name

d-apxioyg5-name = Name

d-uahbgvjf = Rename
    .doc = Renames a folder.
    .w-a-folder-needs-a-name = A folder needs a name

d-uahbgvjf-name = Name

d-lray7fou = Delete folder
    .doc = Deletes a folder; its subscriptions stay, at the top of the sidebar.
    .did = deleted a folder

d-lray7fou-folder = Folder

d-dduccdwb = Rename
    .doc = Renames a subscription, whatever its feed calls itself.
    .w-a-subscription-needs-a-name = A subscription needs a name

d-dduccdwb-name = Name

d-cmlsymqr = Move to folder
    .doc = Moves a subscription into a folder, or out of every folder.

d-cmlsymqr-folder = Folder

d-bmzxopbs = Mark all as read
    .doc = Marks everything the place `place` lists as read.
    .did = marked articles read

d-bmzxopbs-place = Place

d-ducv63vk = Mark as unread
    .doc = Marks an article read, or unread.
    .w-mark-as-unread = Mark as unread
    .w-mark-as-read = Mark as read

d-ducv63vk-read = Read

d-iozr4xux = Remove star
    .doc = Stars an article, or takes the star back.
    .w-remove-star = Remove star
    .w-star-article = Star article

d-iozr4xux-starred = Starred

d-45kyblnz = Edit tags…
    .doc = Gives an article the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them.
    .did = tagged an article

d-45kyblnz-tags = Tags

d-wjiyczto = Refresh all feeds
    .doc = Reads every subscription again.
    .did = refreshed the feeds
    .w-there-are-no-subscriptions-to-refresh = There are no subscriptions to refresh
    .w-a-refresh-is-already-running = A refresh is already running
    .w-couldn-t-start-fetching = Couldn't start fetching: { $why }

d-wjiyczto-only = Only

d-io5ncepq = Stop refreshing
    .doc = Stops a refresh that is running.

d-dlqkkend = Poll refresh
    .doc = Takes what the device has read so far: each subscription's answer as it lands, and what the feed says of itself with it.
    .w-the-refresh-stopped = The refresh stopped: { $why }
    .w-a-subscription-s-answer-did-not = A subscription's answer did not read: { $why }
    .w-the-refresh-s-answer-did-not = The refresh's answer did not read: { $why }

d-ompsa4v6 = Import OPML…
    .doc = Subscribes to what an OPML file lists: every address the library does not have yet, in the folders the file names, made where missing.
    .did = imported subscriptions

d-ompsa4v6-file = File

d-2cdnd6w6 = Export OPML…
    .doc = The library's subscriptions as an OPML 2.0 file.

d-bklq4t5c = Text size
    .doc = Sets how large the article's text is set: 0.8 to 2 times the reading base.

d-bklq4t5c-scale = Scale

d-x3dxonwa = Increase reader text size
    .doc = A step larger in the reading itself.

d-n66gk5mg = Decrease reader text size
    .doc = A step smaller.

d-6if3si6p = Reading face
    .doc = The face the article is set in: `serif`, `sans` or `mono`.
    .w-a-reading-face-is-serif-sans = A reading face is serif, sans or mono

d-6if3si6p-face = Face

d-tntfvdsh = Reader paper
    .doc = The paper the article is printed on: `system` follows the theme, or a fixed page for long-form reading.
    .w-a-paper-is-system-light-sepia = A paper is system, light, sepia or dark

d-tntfvdsh-paper = Paper

d-5qtsbxfs = Why
    .w-couldn-t-reach-the-site = Couldn't reach the site
    .w-the-site-refused-the-request-http = The site refused the request (HTTP { $status })
    .w-the-site-refused-the-request = The site refused the request
    .w-the-answer-wasn-t-a-feed = The answer wasn't a feed
    .w-the-site-s-certificate-changed-so = The site's certificate changed, so someone may be impersonating it
    .w-couldn-t-start-fetching = Couldn't start fetching

d-mb7lwq2a = Add subscription~present

d-j45dc6en = New folder~present

d-y2laaqip = Rename folder~present

d-sehtg6ap = Rename feed~present

d-5nbrso4z = Set read~present

d-wymgthpw = Star~present

d-fuglynku = Refresh~present

d-llfretkx = Set face~present

d-wayoq2pk = Set paper~present

d-qpdqpwvk = Feeds
    .w-feeds = Feeds
    .w-no-feeds-yet = No feeds yet
    .w-subscribe-to-an-rss-atom-or = Subscribe to an RSS, Atom or Gemini feed to start reading.
    .w-add-feed = Add feed

d-3dhsfin6 = Library menu
    .w-library = Library
    .w-new-folder = New folder…
    .w-import-opml = Import OPML…
    .w-export-opml = Export OPML…

d-xudd2qzl = Places
    .w-all-articles = All articles
    .w-unread = Unread
    .w-starred = Starred
    .w-feeds = Feeds
    .w-folder = folder:{ $id }
    .w-tags = Tags

d-5ohvr24p = Feed place

d-k2jb46f2 = Refresh bar
    .w-refreshing-of-feeds = Refreshing { $done } of { $total } feeds
    .w-refreshing-feeds = Refreshing feeds
    .w-stop = Stop
    .w-1-feed-could-not-be-refreshed = 1 feed could not be refreshed
    .w-feeds-could-not-be-refreshed = { $failed } feeds could not be refreshed
    .w-1-new-article = 1 new article
    .w-new-articles = { $fresh } new articles
    .w-all-feeds-are-up-to-date = All feeds are up to date

d-2kgv5ybw = Place bar
    .w-open-site = Open site
    .w-rename = Rename
    .w-move-to-folder = Move to folder
    .w-unsubscribe = Unsubscribe
    .w-delete-folder = Delete folder

d-ydy7dml3 = Article list

d-4odicc56 = Article item
    .w-remove-star = Remove star
    .w-star-article = Star article

d-37yldcjy = Article row

d-ruo7nyba = Article page
    .w-remove-star = Remove star
    .w-star-article = Star article
    .w-mark-as-unread = Mark as unread
    .w-mark-as-read = Mark as read
    .w-edit-tags = Edit tags…
    .w-open-original = Open original
    .w-continuous = continuous

d-zmnwlend = Reading menu
    .w-reading-settings = Reading settings
    .w-larger-text = Larger text
    .w-smaller-text = Smaller text
    .w-serif = Serif
    .w-sans = Sans
    .w-mono = Mono
    .w-system-paper = System paper
    .w-light-paper = Light paper
    .w-sepia-paper = Sepia paper
    .w-dark-paper = Dark paper
