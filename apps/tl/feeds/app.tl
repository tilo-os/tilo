app so.tilo.feeds "Feeds" (
    icon: .rss,
    category: .network,
    tagline: "Reads RSS, Atom and Gemini feeds",
    keywords: ["rss", "atom", "feeds", "feed", "news", "newsreader", "newsfeed", "subscriptions", "akregator", "gemini", "gemtext", "nachrichten", "abos"],
    splash_budget: 250,
    web: ["*"],
)

/// The format a subscription is read in.
enum FeedFormat {
    /// An RSS feed.
    rss
    /// An Atom feed.
    atom
    /// A JSON Feed.
    json
    /// A Gemini capsule.
    gemini
}

/// A folder that keeps related subscriptions together in the sidebar.
type Folder (title: name) {
    name: Text
}

/// One subscription: the feed a person reads, and what this workspace knows of it.
type Feed (title: name) {
    /// What it is called on screen: its own title once a refresh brought one, and its site's
    /// address until then.
    name: Text
    /// The feed's address, which a refresh reads again.
    url: Text
    /// The site it comes from.
    site: Text = ""
    /// The folder it is listed in; none for the top of the sidebar.
    folder: Option<Folder> = .none
    format: FeedFormat = .rss
    /// The certificate a `gemini://` server presented first, as hex: the one thing that stops
    /// anyone on the path from serving the feed in its place. Only a refresh sets it.
    pin: Text = ""
    /// Why the last refresh failed, in words; empty while it stands well.
    trouble: Text = ""
}

/// One article a refresh brought in: what the feed says of it, and what its readers decided.
type Article (title: title, untitled: "Untitled article") {
    /// The subscription it came from.
    feed: Feed
    title: Text
    /// Who wrote it; empty when the feed does not say.
    author: Text = ""
    /// The address of the article's own page.
    url: Text = ""
    /// When it was published, where the feed says.
    published: Option<Time> = .none
    /// Its first line, as a row reads it.
    summary: Text = ""
    /// What it says, as the feed sent it.
    body: RichText
    read: Bool = false
    starred: Bool = false
    /// The readers' own labels: lower case, without `#`.
    tags: Set<Text>

    search {
        filter "Unread" = not read (icon: .inbox)
        filter starred (icon: .star)
        filter feed
        filter tags
        sort published newest
    }
}

/// One person's reading: the place they are reading in, and the size, face and paper the
/// article's page is set in. It is the reader's own, so every member of the workspace reads in
/// theirs and no one else's.
type Reader (readers: author) {
    /// The place in the sidebar they are reading: `all`, `unread`, `starred`, `feed:<id>`,
    /// `folder:<id>` or `tag:<name>`.
    place: Text = "all"
    /// The text's size as a multiple of the reading base, between 0.8 and 2.
    scale: Float = 1.0
    /// The face the article is set in: `serif`, `sans` or `mono`.
    face: Text = "serif"
    /// The page behind the words: `system` follows the theme, or a fixed `light`, `sepia` or
    /// `dark` page for long-form reading.
    paper: Text = "system"
}

/// How far a refresh has got, and how the last one ended.
type Refresh {
    /// The job the device is running, by its id; empty while nothing runs.
    job: Text = ""
    /// `running` while the device reads the subscriptions, and `done` once it has.
    state: Text = "done"
    done: Int = 0
    total: Int = 0
    /// How many articles the last refresh brought in.
    fresh: Int merge counter = 0
    /// How many subscriptions it could not read.
    failed: Int merge counter = 0
    /// Why it could not start, in words; empty otherwise.
    note: Text = ""
}

/// The workspace's feeds: its folders, its subscriptions, the articles they brought, and how
/// each member of the workspace reads them.
type Library {
    folders: Collection<Folder>
    feeds: Collection<Feed>
    articles: Collection<Article>
    /// The workspace's readers: one record each, its own to its reader alone.
    readers: Collection<Reader>
    refresh: Refresh
}

root library: Library

default view feeds for Library

default view article_page for Article

/// The subscriptions one refresh is asked for.
// ---------------------------------------------------------------- a refresh's answers
type Ask {
    feeds: List<AskFeed>
}

type AskFeed {
    url: Text
    pin: Text
}

/// How far the device has got.
type Job {
    state: Text
    done: Int
    total: Int
    ready: List<Text>
}

/// One feed's answer, taken as it lands.
type Taken {
    ok: Bool
    title: Text
    pin: Text
    reason: Text
    status: Int
    message: Text
    /// Each article's own JSON, as text: a list of records holds resources, which JSON text
    /// makes none, so each is read into its record here.
    articles: List<Text>
}

/// One article a feed lists.
type Pulled {
    title: Text
    author: Text
    url: Text
    published: Int
    summary: Text
    body: Text
}

type TakeAsk {
    job: Text
    url: Text
}

/// What an OPML file lists.
type Outline {
    title: Text
    url: Text
    site: Text
    folder: Text
}

type Outlines {
    /// Each outline's own JSON, as text.
    outlines: List<Text>
}

type OpmlAsk {
    text: Text
}

type OpmlWrite {
    feeds: List<OpmlFeed>
}

type OpmlFeed {
    title: Text
    url: Text
    site: Text
    format: Text
    folder: Text
}

type OpmlText {
    text: Text
}

/// Gives the person a reading of their own here, if they have none yet: the place they read in
/// and how the article's page is set.
// ---------------------------------------------------------------- opening the window
action begin_reading(lib: Library) -> Reader (label: "Start reading here") {
    lib.readers.first() ?? lib.readers.insert(Reader())
}

/// Opening Feeds gives the person a reading of their own.
opened action arrive(lib: Library) {
    begin_reading(lib)
}

/// Opening an article reads it, as every path to it does.
opened action read_on_open(a: Article) {
    if not a.read {
        a.read = true
    }
}

/// Reads from `place` in the sidebar. The place is the reader's own, so it follows them to their
/// other devices and no one else's reading moves with it.
// ---------------------------------------------------------------- the places a person reads
action show_place(reader: Reader, place: Text) (label: "Show a place") {
    reader.place = place
}

/// Shows the place a tag's articles stand in.
action show_tag(reader: Reader, tag: Text) (label: "Show tagged articles") {
    reader.place = "tag:{tag}"
}

/// Shows the place an article's feed lists it in, so a search hit or an agent's `open` lands its
/// row in the list beside it.
action show_article(a: Article) (label: "Show the article's feed") {
    let lib = root Library
    if let reader = lib.readers.first() {
        reader.place = "feed:{a.feed.id()}"
    }
}

/// Subscribes to a feed's address, or shows the subscription this library already has.
// ---------------------------------------------------------------- subscriptions
primary action add_subscription(lib: Library, url: Text, folder: Option<Folder> = .none) -> Feed (
    icon: .plus,
    label: "Add subscription",
    did: "subscribed to a feed",
) {
    require url.trim() != "" "A subscription needs a feed's address"
    let address = url.trim()
    if let had = lib.feeds.find(f -> f.url == address) {
        return had
    }
    let name = site_of(address)
    lib.feeds.insert(
        Feed(name: name, url: address, site: name, folder: folder, format: format_of(address)),
    )
}

/// Unsubscribes: the subscription leaves the sidebar, and what it brought in is taken away with
/// it. Undo brings both back.
destructive action unsubscribe(lib: Library, feed: Feed) (
    icon: .trash_2,
    label: "Unsubscribe",
    did: "unsubscribed from a feed",
) {
    for a in lib.articles.filter(x -> x.feed.id() == feed.id()) {
        lib.articles.remove(a)
    }
    lib.feeds.remove(feed)
}

/// Starts a folder that keeps related subscriptions together; its subscriptions stay when it
/// goes.
action new_folder(lib: Library, name: Text) (
    icon: .folder_plus,
    label: "New folder",
    did: "made a folder",
) {
    require name.trim() != "" "A folder needs a name"
    lib.folders.insert(Folder(name: name.trim()))
}

/// Renames a folder.
action rename_folder(folder: Folder, name: Text) (icon: .pencil, label: "Rename") {
    require name.trim() != "" "A folder needs a name"
    folder.name = name.trim()
}

/// Deletes a folder; its subscriptions stay, at the top of the sidebar.
destructive action delete_folder(lib: Library, folder: Folder) (
    icon: .trash_2,
    label: "Delete folder",
    did: "deleted a folder",
) {
    for feed in lib.feeds.filter(f -> f.folder == .some(folder)) {
        feed.folder = .none
    }
    lib.folders.remove(folder)
}

/// Renames a subscription, whatever its feed calls itself.
action rename_feed(feed: Feed, name: Text) (icon: .pencil, label: "Rename") {
    require name.trim() != "" "A subscription needs a name"
    feed.name = name.trim()
}

/// Moves a subscription into a folder, or out of every folder.
action move_feed(feed: Feed, folder: Option<Folder> = .none) (
    icon: .folder,
    label: "Move to folder",
) {
    feed.folder = folder
}

/// Marks everything the place `place` lists as read.
// ---------------------------------------------------------------- reading
action mark_read(lib: Library, place: Text = "") (
    icon: .check,
    label: "Mark all as read",
    key: "a",
    did: "marked articles read",
) {
    let reader = lib.readers.first() ?? Reader()
    let where = if place == "" { reader.place } else { place }
    for a in place_articles(lib, where) {
        if not a.read {
            a.read = true
        }
    }
}

/// Marks an article read, or unread.
action set_read(a: Article, read: Bool = not a.read) (
    icon: .mail,
    label: if read { "Mark as unread" } else { "Mark as read" },
) {
    a.read = read
}

/// Stars an article, or takes the star back.
action star(a: Article, starred: Bool = not a.starred) (
    icon: .star,
    label: if starred { "Remove star" } else { "Star article" },
    on: a.starred,
) {
    a.starred = starred
}

/// Gives an article the tags a person typed, comma-separated: each trimmed, lower case, without
/// `#`, at most 24 of them.
action set_tags(a: Article, tags: Text) (
    icon: .tag,
    label: "Edit tags…",
    did: "tagged an article",
) {
    let olds = a.tags.map(t -> t)
    for old in olds {
        a.tags.remove(old)
    }
    if tags.trim() == "" {
        return
    }
    let kept: List<Text> = tags
        .split(",")
        .map(t -> t.trim().replace("#", "").lower())
        .filter(t -> t != "")
        .take(24)
    for tag in kept {
        a.tags.add(tag)
    }
}

/// Reads every subscription again. The device fetches them away from the frame and the app takes
/// each answer as it lands (`poll_refresh`), so a slow site never holds the window.
// ---------------------------------------------------------------- refreshing
action refresh(lib: Library) (
    icon: .refresh,
    key: "r",
    label: "Refresh all feeds",
    did: "refreshed the feeds",
) {
    require not lib.feeds.is_empty() "There are no subscriptions to refresh"
    require lib.refresh.state != "running" "A refresh is already running"
    let asked = Ask(feeds: lib.feeds.map(f -> AskFeed(url: f.url, pin: f.pin)))
    match engine.start("refresh", json.encode(asked)) {
        .ok(job) => {
            for f in lib.feeds {
                f.trouble = ""
            }
            lib.refresh.job = job
            lib.refresh.state = "running"
            lib.refresh.done = 0
            lib.refresh.total = lib.feeds.len()
            // What the last refresh added up to goes back to nothing for this one.
            lib.refresh.fresh -= lib.refresh.fresh
            lib.refresh.failed -= lib.refresh.failed
            lib.refresh.note = ""
        }
        .err(why) => {
            lib.refresh.state = "done"
            lib.refresh.job = ""
            lib.refresh.note = std.say("Couldn't start fetching: {why}")
        }
    }
}

/// Stops a refresh that is running.
action stop_refresh(lib: Library) (icon: .x, label: "Stop refreshing") {
    if lib.refresh.job != "" { engine.cancel(lib.refresh.job) }
    lib.refresh.state = "done"
    lib.refresh.job = ""
}

/// Takes what the device has read so far: each subscription's answer as it lands, and what the
/// feed says of itself with it. What a refresh brought in is nobody's step of their own, so
/// landing it says nothing and offers no undo of its own; an article the library already keeps is
/// not added twice. Landing writes, so it stands here rather than in a call of its own: a run that
/// calls another action is run twice, and the device's answers are not its to answer twice.
action poll_refresh(lib: Library) {
    if lib.refresh.job == "" {
        return
    }
    match engine.status(lib.refresh.job) {
        .err(why) => {
            lib.refresh.state = "done"
            lib.refresh.job = ""
            lib.refresh.note = std.say("The refresh stopped: {why}")
        }
        .ok(text) => match json.decode<Job>(text) {
            .ok(job) => {
                lib.refresh.done = job.done
                lib.refresh.total = job.total
                for url in job.ready {
                    let ask = TakeAsk(job: lib.refresh.job, url: url)
                    let answer = match engine.call("take", json.encode(ask)) {
                        .ok(said) => json.decode<Taken>(said),
                        .err(why) => .err(why),
                    }
                    match answer {
                        .err(why) => {
                            lib.refresh.note = std.say(
                                "A subscription's answer did not read: {why}",
                            )
                        }
                        .ok(taken) => {
                            if let feed = lib.feeds.find(f -> f.url == url) {
                                if not taken.ok {
                                    feed.trouble = why(taken)
                                    lib.refresh.failed += 1
                                } else {
                                    feed.trouble = ""
                                    if taken.title != "" {
                                        feed.name = taken.title
                                    }
                                    if feed.pin == "" and taken.pin != "" {
                                        feed.pin = taken.pin
                                    }
                                    for said in taken.articles {
                                        match json.decode<Pulled>(said) {
                                            .ok(pulled) => {
                                                let known = lib.articles.any(
                                                    a -> a.feed.id() == feed.id()
                                                        and same(a, pulled),
                                                )
                                                if not known {
                                                    lib.articles.insert(
                                                        Article(
                                                            feed: feed,
                                                            title: pulled.title,
                                                            author: pulled.author,
                                                            url: pulled.url,
                                                            published: at(pulled.published),
                                                            summary: pulled.summary,
                                                            body: RichText.from_markdown(
                                                                pulled.body,
                                                            ),
                                                        ),
                                                    )
                                                    lib.refresh.fresh += 1
                                                }
                                            }
                                            .err(_) => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if job.state == "done" or job.state == "cancelled" {
                    lib.refresh.state = "done"
                    lib.refresh.job = ""
                }
            }
            .err(why) => {
                lib.refresh.note = std.say("The refresh's answer did not read: {why}")
            }
        },
    }
}

/// A refresh reads the subscriptions while the window is open: what has landed is taken each
/// second, so the list fills as the feeds answer.
trigger watch_refresh(lib: Library) every 1s: poll_refresh(lib)

/// The answer an engine gave, when it is one.
fn landable(answer: Result<Taken, Text>) -> Option<Taken> {
    match answer {
        .ok(taken) => .some(taken),
        .err(_) => .none,
    }
}

/// Whether the article already kept is the one a feed lists: by its address, or, for a feed whose
/// articles have none, by its title.
fn same(a: Article, pulled: Pulled) -> Bool {
    if pulled.url == "" { a.url == "" and a.title == pulled.title } else { a.url == pulled.url }
}

/// Why a subscription could not be read, as its row says it: a site that refused this reader is
/// not a broken connection, and neither is a page that turned out not to be a feed.
fn why(taken: Taken) -> Text {
    if taken.reason == "unreachable" {
        std.say("Couldn't reach the site")
    } else if taken.reason == "refused" {
        if taken.status > 0 {
            std.say("The site refused the request (HTTP {taken.status})")
        } else { std.say("The site refused the request") }
    } else if taken.reason == "not_a_feed" {
        std.say("The answer wasn't a feed")
    } else if taken.reason == "untrusted" {
        std.say("The site's certificate changed, so someone may be impersonating it")
    } else if taken.reason == "unavailable" {
        std.say("Couldn't start fetching")
    } else { taken.message }
}

/// Subscribes to what an OPML file lists: every address the library does not have yet, in the
/// folders the file names, made where missing.
// ---------------------------------------------------------------- OPML
action import_opml(lib: Library, file: File) (
    icon: .upload,
    label: "Import OPML…",
    did: "imported subscriptions",
    opens: [.opml, .xml, "text/x-opml", "application/xml", "text/xml"],
) {
    let text = match file.text() {
        .ok(said) => said,
        .err(_) => "",
    }
    if text == "" {
        return
    }
    let answer = match engine.call("opml", json.encode(OpmlAsk(text: text))) {
        .ok(said) => json.decode<Outlines>(said),
        .err(_) => .err(""),
    }
    match answer {
        .ok(read) => {
            for said in read.outlines {
                match json.decode<Outline>(said) {
                    .ok(outline) => {
                        let address = outline.url.trim()
                        if address != "" and not lib.feeds.any(f -> f.url == address) {
                            let folder: Option<Folder> = if outline.folder == "" {
                                .none
                            } else {
                                .some(
                                    lib.folders.find(f -> f.name == outline.folder)
                                        ?? lib.folders.insert(Folder(name: outline.folder)),
                                )
                            }
                            let title = if outline.title.trim() == "" {
                                site_of(address)
                            } else { outline.title.trim() }
                            let site = if outline.site.trim() == "" {
                                site_of(address)
                            } else { outline.site.trim() }
                            lib.feeds.insert(
                                Feed(
                                    name: title,
                                    url: address,
                                    site: site,
                                    folder: folder,
                                    format: format_of(address),
                                ),
                            )
                        }
                    }
                    .err(_) => {}
                }
            }
        }
        .err(_) => {}
    }
}

/// The library's subscriptions as an OPML 2.0 file.
action export_opml(lib: Library) -> File (icon: .download, label: "Export OPML…") {
    let wanted = OpmlWrite(
        feeds: lib.feeds.map(
            f -> OpmlFeed(
                title: f.name,
                url: f.url,
                site: f.site,
                format: if f.format == .atom { "atom" } else { "rss" },
                folder: f.folder?.name ?? "",
            ),
        ),
    )
    let text = match engine.call("opml-export", json.encode(wanted)) {
        .ok(said) => match json.decode<OpmlText>(said) {
            .ok(written) => written.text,
            .err(_) => "",
        },
        .err(_) => "",
    }
    File.of_text("feeds.opml", text)
}

/// Sets how large the article's text is set: 0.8 to 2 times the reading base.
// ---------------------------------------------------------------- reading settings
action set_scale(reader: Reader, scale: Float) (label: "Text size") {
    reader.scale = scale.clamp(0.8, 2.0)
}

/// A step larger in the reading itself.
action larger_text(reader: Reader) (icon: .a_arrow_up, label: "Increase reader text size") {
    reader.scale = (reader.scale + 0.1).clamp(0.8, 2.0)
}

/// A step smaller.
action smaller_text(reader: Reader) (icon: .a_arrow_down, label: "Decrease reader text size") {
    reader.scale = (reader.scale - 0.1).clamp(0.8, 2.0)
}

/// The face the article is set in: `serif`, `sans` or `mono`.
action set_face(reader: Reader, face: Text) (label: "Reading face") {
    require face == "serif" or face == "sans" or face == "mono" "A reading face is serif, sans or mono"
    reader.face = face
}

/// The paper the article is printed on: `system` follows the theme, or a fixed page for long-form
/// reading.
action set_paper(reader: Reader, paper: Text) (label: "Reader paper") {
    require paper == "system" or paper == "light" or paper == "sepia" or paper == "dark" "A paper is system, light, sepia or dark"
    reader.paper = paper
}

/// What a feed is called until it says: the site it comes from, "theguardian.com" rather than the
/// whole address.
// ---------------------------------------------------------------- what a person reads
fn site_of(url: Text) -> Text {
    let rest = after_scheme(url)
    let host = rest.split("/").first() ?? rest
    let bare = if host.starts_with("www.") { host.slice(4, host.len()) } else { host }
    if bare == "" { url } else { bare }
}

/// A `gemini://` address is a capsule and reads Gemtext; every other address is an HTTP feed.
fn format_of(url: Text) -> FeedFormat {
    if url.starts_with("gemini://") { .gemini } else { .rss }
}

/// What follows the scheme in an address, or the whole address when it names none.
fn after_scheme(url: Text) -> Text {
    match url.find("://") {
        .some(at) => url.slice(at + 3, url.len()),
        .none => url,
    }
}

/// The articles the place `place` lists, newest first.
fn place_articles(lib: Library, place: Text) -> List<Article> {
    if place == "unread" {
        lib.articles.matching("is:unread").sort_by(.published, desc)
    } else if place == "starred" {
        lib.articles.matching("is:starred").sort_by(.published, desc)
    } else if place.starts_with("feed:") {
        let id = place.slice(5, place.len())
        lib.articles.filter(a -> a.feed.id() == id).sort_by(.published, desc)
    } else if place.starts_with("folder:") {
        let id = place.slice(7, place.len())
        let folder = lib.folders.find(f -> f.id() == id)
        lib.articles.filter(a -> a.feed.folder == folder).sort_by(.published, desc)
    } else if place.starts_with("tag:") {
        let tag = place.slice(4, place.len())
        lib.articles.filter(a -> a.tags.contains(tag)).sort_by(.published, desc)
    } else { lib.articles.sort_by(.published, desc) }
}

/// Every tag the library's articles carry, each once, alphabetized.
fn tags_of(lib: Library) -> List<Text> {
    let seen: Map<Bool> = Map<Bool>()
    let out: List<Text> = []
    for a in lib.articles {
        for tag in a.tags.sort_by(t -> t) {
            if seen.get(tag) == .none {
                seen.put(tag, true)
                out = [out, [tag]].flat_map(t -> t)
            }
        }
    }
    out
}

/// What the sidebar calls a place.
fn place_label(lib: Library, place: Text) -> Text {
    if place == "unread" {
        "Unread"
    } else if place == "starred" {
        "Starred"
    } else if place.starts_with("feed:") {
        let id = place.slice(5, place.len())
        match lib.feeds.find(f -> f.id() == id) {
            .some(feed) => feed.name,
            .none => "Feed",
        }
    } else if place.starts_with("folder:") {
        let id = place.slice(7, place.len())
        match lib.folders.find(f -> f.id() == id) {
            .some(folder) => folder.name,
            .none => "Folder",
        }
    } else if place.starts_with("tag:") { place.slice(4, place.len()) } else { "All articles" }
}

/// What the list says when the place holds nothing.
fn empty_words(place: Text) -> Text {
    if place == "unread" {
        "No unread articles"
    } else if place == "starred" {
        "No starred articles"
    } else if place.starts_with("tag:") { "No articles with this tag" } else { "No articles yet" }
}

/// How many articles carry `tag`.
fn tagged(lib: Library, tag: Text) -> Int {
    lib.articles.filter(a -> a.tags.contains(tag)).len()
}

/// How many places wait to be read, everywhere.
fn unread_count(lib: Library) -> Int {
    lib.articles.matching("is:unread").len()
}

/// How many articles are starred.
fn star_count(lib: Library) -> Int {
    lib.articles.matching("is:starred").len()
}

/// How many of a feed's articles wait to be read.
fn unread_in_feed(lib: Library, feed: Feed) -> Int {
    lib.articles.filter(a -> a.feed == feed).count(a -> not a.read)
}

/// How many of a folder's articles wait to be read.
fn unread_in_folder(lib: Library, folder: Folder) -> Int {
    lib.articles.filter(a -> a.feed.folder == .some(folder)).count(a -> not a.read)
}

/// A moment from the epoch seconds an engine answers with, when it names one.
fn at(seconds: Int) -> Option<Time> {
    if seconds <= 0 {
        .none
    } else {
        .some(Time("1970-01-01T00:00:00Z") + Duration(seconds))
    }
}

/// What a place lists a row by, under its title: one feed's list shows the article's first line,
/// and every other place names the feed it came from.
fn row_secondary(a: Article, place: Text) -> Text {
    if place.starts_with("feed:") { a.summary } else { a.feed.name }
}

/// The page an article is printed on, by the paper its reader chose; `system` follows the theme.
fn paper_of(reader: Reader) -> Option<Text> {
    if reader.paper == "light" {
        .some("#fbfbfa")
    } else if reader.paper == "sepia" {
        .some("#f4ecdc")
    } else if reader.paper == "dark" { .some("#151515") } else { .none }
}

/// The ink printed on that page.
fn ink_of(reader: Reader) -> Option<Text> {
    if reader.paper == "light" {
        .some("#1b1b1b")
    } else if reader.paper == "sepia" {
        .some("#33291d")
    } else if reader.paper == "dark" { .some("#e8e8e6") } else { .none }
}

/// Feeds: the places a person reads in the sidebar, the articles of one, and the article open
/// beside them. An article's own page is its type's default view, so a search hit and an agent's
/// `open` land in the same window a row does.
// ---------------------------------------------------------------- the window
view feeds(lib: Library) {
    // What the window's search holds: it narrows the chosen place's articles.
    state found: Text = ""
    let reader = lib.readers.first() ?? Reader()
    let place = reader.place
    on show(x: Article): show_article(x)
    let listed = place_articles(lib, place)
    let shown = if found.is_empty() { listed } else { listed.matching(found) }
    ui.NavShell(
        content_kind: .reading,
        title: place_label(lib, place),
        sidebar_title: "Feeds",
        overview_title: place_label(lib, place),
        overview_count: shown.len(),
        nothing_listed: lib.feeds.is_empty(),
        actions: [refresh, add_subscription, mark_read],
    ) {
        on search(t: Text): set(found, t)
        sidebar_actions: {
            library_menu(lib)
        }
        sidebar: {
            places(lib, reader)
        }
        overview: {
            if lib.feeds.is_empty() {
                ui.EmptyState(
                    "No feeds yet",
                    message: "Subscribe to an RSS, Atom or Gemini feed to start reading.",
                    icon: .rss,
                    action: "Add feed",
                ) {
                    on act: add_subscription(lib)
                }
            } else {
                stack(fill, gap: .none) {
                    refresh_bar(lib)
                    place_bar(lib, place)
                    article_list(lib, place, shown)
                }
            }
        }
    }
}

/// The library's own commands, in the sidebar's header.
view library_menu(lib: Library) {
    ui.MenuButton("Library", icon: .folder_plus) {
        ui.MenuItem("New folder…", icon: .folder_plus) {
            on choose: new_folder(lib)
        }
        ui.MenuItem("Import OPML…", icon: .upload) {
            on choose: file.choose(
                import_opml(lib),
                accept: [".opml", ".xml", "text/x-opml", "application/xml", "text/xml"],
            )
        }
        ui.MenuItem("Export OPML…", icon: .download) {
            on choose: file.save(export_opml(lib), name: "feeds.opml")
        }
    }
}

/// The sidebar's places: everything, unread and starred, then the folders with their
/// subscriptions under them, then the subscriptions in no folder, then the tags. The list folds a
/// folder away, and keeps that with the person reading.
view places(lib: Library, reader: Reader) {
    ui.NavList(selected: reader.place) {
        ui.NavItem("all", "All articles", icon: .layers) {
            on select: show_place(reader, "all")
        }
        ui.NavItem("unread", "Unread", icon: .inbox, live: unread_count(lib)) {
            on select: show_place(reader, "unread")
        }
        ui.NavItem("starred", "Starred", icon: .star, count: star_count(lib)) {
            on select: show_place(reader, "starred")
        }
        ui.NavSection("Feeds", add: new_folder(lib)) {
            for folder in lib.folders {
                ui.NavItem(
                    "folder:{folder.id()}",
                    folder.name,
                    icon: .folder,
                    count: unread_in_folder(lib, folder),
                ) {
                    on select: show_place(reader, "folder:{folder.id()}")
                }
            }
            for folder in lib.folders {
                for feed in lib.feeds.filter(f -> f.folder == .some(folder)) {
                    feed_place(lib, reader, feed, .some("folder:{folder.id()}"))
                }
            }
            for feed in lib.feeds.filter(f -> f.folder == .none) {
                feed_place(lib, reader, feed, .none)
            }
        }
        if not tags_of(lib).is_empty() {
            ui.NavSection("Tags") {
                for tag in tags_of(lib) {
                    ui.NavItem("tag:{tag}", tag, icon: .tag, count: tagged(lib, tag)) {
                        on select: show_place(reader, "tag:{tag}")
                    }
                }
            }
        }
    }
}

/// One subscription in the sidebar, under its folder when it has one, with what waits in it, and
/// why its last refresh failed.
view feed_place(lib: Library, reader: Reader, feed: Feed, under: Option<Text>) {
    ui.NavItem(
        "feed:{feed.id()}",
        feed.name,
        icon: if feed.format == .gemini { .file_text } else { .rss },
        live: unread_in_feed(lib, feed),
        parent: under,
        item: feed,
    ) {
        on select: show_place(reader, "feed:{feed.id()}")
    }
}

/// What a refresh is doing, over the list: how far it has got, and how the last one ended.
view refresh_bar(lib: Library) {
    let job = lib.refresh
    if job.state == "running" {
        stack(padding: .group, gap: .tight) {
            ui.Text(
                std.say("Refreshing {job.done} of {job.total} feeds"),
                role: .caption,
                secondary: true,
            )
            ui.ProgressBar(
                value: if job.total == 0 {
                    0.0
                } else {
                    job.done.to_float() / job.total.to_float()
                },
                label: "Refreshing feeds",
            )
            row {
                spacer()
                ui.Button("Stop", icon: .x) {
                    on press: stop_refresh(lib)
                }
            }
        }
    } else if job.note != "" {
        ui.Banner(job.note, kind: .error, icon: .triangle_alert)
    } else if job.failed > 0 {
        let failed = lib.feeds.filter(f -> f.trouble != "")
        ui.Banner(
            if job.failed == 1 {
                "1 feed could not be refreshed"
            } else {
                "{job.failed} feeds could not be refreshed"
            },
            message: failed.map(f -> "{f.name}: {f.trouble}").join(" · "),
            kind: .warning,
            icon: .triangle_alert,
        )
    }
}

/// The chosen place's own commands: a subscription's site, name, folder and presence, or a
/// folder's.
view place_bar(lib: Library, place: Text) {
    if place.starts_with("feed:") {
        let id = place.slice(5, place.len())
        if let feed = lib.feeds.find(f -> f.id() == id) {
            row(gap: .tight, wrap: true) {
                if feed.site != "" {
                    ui.Link(feed.site, label: "Open site", role: .caption)
                }
                ui.Button("Rename", icon: .pencil) {
                    on press: rename_feed(feed)
                }
                ui.Button("Move to folder", icon: .folder) {
                    on press: move_feed(feed)
                }
                ui.Button("Unsubscribe", icon: .trash_2, role: .destructive) {
                    on press: unsubscribe(lib, feed)
                }
            }
        }
    } else if place.starts_with("folder:") {
        let id = place.slice(7, place.len())
        if let folder = lib.folders.find(f -> f.id() == id) {
            row(gap: .tight, wrap: true) {
                ui.Button("Rename", icon: .pencil) {
                    on press: rename_folder(folder)
                }
                ui.Button("Delete folder", icon: .trash_2, role: .destructive) {
                    on press: delete_folder(lib, folder)
                }
            }
        }
    }
}

/// The place's articles, newest first, each a row that opens beside the list; a narrow window
/// opens it in front of the list instead.
view article_list(lib: Library, place: Text, shown: List<Article>) {
    if shown.is_empty() {
        ui.EmptyState(empty_words(place), icon: .inbox)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true, narrow: space.compact) {
            on select(x: Article): open(x, front: false)
            on activate(x: Article): open(x)
            for a in shown {
                ui.ListItem(item: a, presence: lenses(a).find(l -> not l.mine)?.who) {
                    article_item(a, place)
                }
            }
        }
    }
}

/// One article's item in the list: its row, and the star it can be given while hovered, so a
/// person stars an article without opening it.
view article_item(a: Article, place: Text) {
    article_row(a, place)
    ui.HoverToolbar {
        ui.Button(
            if a.starred { "Remove star" } else { "Star article" },
            icon: .star,
            icon_only: true,
        ) {
            on press: star(a)
        }
    }
}

/// One article's row: its title, where it came from or what it says, when it came, and a mark on
/// its edge — a star where the person starred it, a dot where it still waits to be read.
view article_row(a: Article, place: Text) {
    row(
        a.title,
        secondary: row_secondary(a, place),
        meta: a.published,
        mark: if a.starred { .star } else if not a.read { .circle },
    )
}

/// An article's page: its title, byline, date, tags and body, in the reading face, size and paper
/// its reader chose, with the article's own commands above it.
view article_page(a: Article) {
    let lib = root Library
    let reader = lib.readers.first() ?? Reader()
    page {
        stack(gap: .group) {
            row(gap: .related, wrap: true) {
                row(gap: .tight, wrap: true) {
                    ui.Button(
                        if a.starred { "Remove star" } else { "Star article" },
                        icon: .star,
                        role: .plain,
                    ) {
                        on press: star(a)
                    }
                    ui.Button(
                        if a.read { "Mark as unread" } else { "Mark as read" },
                        icon: .mail,
                        role: .plain,
                    ) {
                        on press: set_read(a)
                    }
                    ui.Button("Edit tags…", icon: .tag, role: .plain) {
                        on press: set_tags(a)
                    }
                    reading_menu(reader)
                }
                spacer()
                if a.url != "" {
                    ui.Link(a.url, label: "Open original", role: .caption)
                }
            }
            ui.Text(a.title, role: .large_title)
            row(gap: .related, wrap: true) {
                if a.author != "" {
                    ui.Text(a.author, role: .caption, secondary: true)
                }
                if let published = a.published {
                    ui.Time(published, role: .caption, secondary: true)
                }
                ui.Text(a.feed.name, role: .caption, secondary: true)
            }
            if not a.tags.is_empty() {
                row(gap: .tight, wrap: true) {
                    for tag in a.tags.sort_by(t -> t) {
                        ui.Chip(tag, icon: .tag) {
                            on press: show_tag(reader, tag)
                        }
                    }
                }
            }
            divider()
            ui.Reading(
                a.body,
                mode: "continuous",
                scale: reader.scale,
                face: reader.face,
                paper: paper_of(reader),
                ink: ink_of(reader),
            )
        }
    }
}

/// The reading settings: larger or smaller text, the face and the paper.
view reading_menu(reader: Reader) {
    ui.MenuButton("Reading settings", icon: .settings_2, icon_only: true, role: .plain) {
        ui.MenuItem("Larger text", icon: .a_arrow_up) {
            on choose: larger_text(reader)
        }
        ui.MenuItem("Smaller text", icon: .a_arrow_down) {
            on choose: smaller_text(reader)
        }
        ui.MenuItem("Serif", checked: reader.face == "serif") {
            on choose: set_face(reader, "serif")
        }
        ui.MenuItem("Sans", checked: reader.face == "sans") {
            on choose: set_face(reader, "sans")
        }
        ui.MenuItem("Mono", checked: reader.face == "mono") {
            on choose: set_face(reader, "mono")
        }
        ui.MenuItem("System paper", checked: reader.paper == "system") {
            on choose: set_paper(reader, "system")
        }
        ui.MenuItem("Light paper", checked: reader.paper == "light") {
            on choose: set_paper(reader, "light")
        }
        ui.MenuItem("Sepia paper", checked: reader.paper == "sepia") {
            on choose: set_paper(reader, "sepia")
        }
        ui.MenuItem("Dark paper", checked: reader.paper == "dark") {
            on choose: set_paper(reader, "dark")
        }
    }
}

/// A workspace that has just started reading: a folder, three subscriptions and what they
/// brought in, so a capture shows the reader as a person sees it.
// ---------------------------------------------------------------- what a person starts with
scenario "a library of feeds" -> Library {
    let lib = root Library
    let reading = lib.folders.insert(Folder(name: "Reading"))
    let guardian = lib.feeds.insert(
        Feed(
            name: "The Guardian: World",
            url: "https://www.theguardian.com/world/rss",
            site: "https://www.theguardian.com",
            folder: .some(reading),
        ),
    )
    let lwn = lib.feeds.insert(
        Feed(
            name: "LWN",
            url: "https://lwn.net/headlines/rss",
            site: "https://lwn.net",
            folder: .some(reading),
        ),
    )
    let capsule = lib.feeds.insert(
        Feed(
            name: "A gemlog",
            url: "gemini://gemini.circumlunar.space/",
            site: "gemini://gemini.circumlunar.space",
            format: .gemini,
        ),
    )
    let world = lib.articles.insert(
        Article(
            feed: guardian,
            title: "A quiet morning in the city",
            author: "Ada Ferrer",
            url: "https://www.theguardian.com/world/2026/nov/24/a-quiet-morning",
            published: .some(2026-11-24T07:40),
            summary: "The streets are empty and the light is good.",
            body: RichText.from_markdown(
                "## The streets are empty\n\nThe light is good, and the coffee is **hot**.\n\n- First\n- Second\n\n> A quiet morning.\n",
            ),
        ),
    )
    world.tags.add("morning")
    let kernel = lib.articles.insert(
        Article(
            feed: lwn,
            title: "A new scheduler for the kernel",
            author: "Jonathan Corbet",
            url: "https://lwn.net/Articles/900000/",
            published: .some(2026-11-23T16:02),
            summary: "The patch set that would change how the kernel picks a task.",
            body: RichText.from_markdown(
                "The patch set that would change how the kernel picks a task has been posted for review.\n",
            ),
        ),
    )
    kernel.read = true
    lib.articles.insert(
        Article(
            feed: capsule,
            title: "A capsule",
            url: "gemini://gemini.circumlunar.space/",
            published: .some(2026-11-22T09:12),
            summary: "Notes from a small capsule.",
            body: RichText.from_markdown("# A capsule\n\nNotes from a small capsule.\n"),
        ),
    )
    lib
}
