app so.tilo.contacts "Contacts" (
    icon: .users,
    category: .office,
    tagline: "The people of the workspace, with how to reach them",
    keywords: ["contacts", "address book", "people", "phone", "email", "vcard"],
)

/// One way to reach someone, and what kind it is: a phone number that is theirs at home, a
/// mailbox that is their work's.
type Labeled {
    /// What kind of number or address it is: home, work, mobile.
    label: Text
    value: Text
}

/// Where a person's post goes, written the way post is addressed here.
type Address {
    /// What kind of place it is: home, work.
    label: Text
    street: Text
    city: Text
    region: Text
    postal: Text
    country: Text
}

/// Someone the workspace keeps in touch with.
///
/// A contact belongs to the workspace: everyone there reads the same book, and a change to a
/// contact is everyone's to see, take back and find.
type Contact (untitled: "No name yet") {
    name: Text
    /// Where they work.
    org: Text
    /// What they do there.
    role: Text
    website: Text
    /// The day they were born; the year may be their birth year or left out of it.
    birthday: Option<Date> = .none
    /// Whether the birthday's year is part of it: a birthday kept without its year is the day it
    /// falls on every year, as many address books keep one.
    birthday_year: Bool = true
    /// Anything worth remembering about them.
    notes: RichText
    phones: Collection<Labeled>
    emails: Collection<Labeled>
    addresses: Collection<Address>
    /// The groups they are in, by name: a group stands while its name is on someone.
    groups: Set<Text>
    favorite: Bool = false
    /// Their picture, by where its bytes are: a `data:` address a vCard's photo carried, or a
    /// content address someone put here. Empty for none; the initials show instead.
    photo: Text = ""
    /// The id of the person the workspace's directory holds for this card, once it is shared
    /// with the apps; empty until then, or after it stops being shared.
    person: Text = ""
    /// The card's own id in the format that carried it here (a vCard's `UID`): what reading the
    /// same file again finds it by, so a book is imported once however often its file is read.
    uid: Text = ""

    search {
        filter "Favorites" = favorite (icon: .star)
        filter groups (icon: .tag)
        sort name a_z
        // A picture is bytes in an address, not words anyone searches for.
        hide photo
    }
}

/// A group the book offers as a place: the people in it, gathered by a word they share.
type Group {
    name: Text
}

/// A card a vCard file held whose id a contact of the book already has with other content: the
/// book keeps it until someone says which card is the person's, and nothing of the contact the
/// book holds is changed before then.
type ImportCard (untitled: "A card to review") {
    /// The words the file wrote for the card, read again when someone takes it.
    card: Text
    /// What the card calls the person, so a review names it.
    name: Text
    /// The id the card carries in its format: the contact of the book it says it is.
    uid: Text
    /// The contact of the book it names the same id as, while the book still holds it.
    existing: Option<Contact>
}

/// What an import did: how many cards the book took, how many it already held, and how many wait
/// for someone to say which card is the person's.
type ImportReport {
    /// How many cards became contacts.
    imported: Int
    /// How many cards the book already held, unchanged.
    unchanged: Int
    /// How many cards changed a contact of the book, and wait in the book's review.
    awaiting: Int
    /// How many cards the file held that this app could not take.
    skipped: Int
    /// Whether the person has read the outcome: a report already read is no banner.
    seen: Bool = false
}

/// An import reading its file and taking its cards, so the window says where it is: one card
/// set at a time, the file's words kept only until the last card is dealt with.
type ImportRun {
    /// What the file is called, as the window's row says it.
    filename: Text
    /// The file being read.
    file: File
    /// The cards the file holds; empty until it was read.
    cards: List<Text>
    /// How many cards the file holds: `-1` until the file was read.
    total: Int = -1
    /// How many cards were dealt with.
    done: Int merge counter = 0
    /// What became of them so far.
    imported: Int merge counter = 0
    unchanged: Int merge counter = 0
    awaiting: Int merge counter = 0
    skipped: Int merge counter = 0
}

/// What one card of a file is to the book.
enum CardFate {
    /// The card is a contact now.
    taken
    /// The book already held it, unchanged.
    held
    /// The card changes a contact the book holds, and waits in the book's review.
    review
    /// The card names nobody this app can take.
    skipped
}

/// What a merge may take from the card that goes: the values each card keeps one of. Numbers,
/// mailboxes, addresses and groups always come over; these are the person's to choose.
enum MergeField {
    /// Their name
    name (icon: .user)
    /// Where they work
    org (icon: .globe)
    /// What they do there
    role (icon: .user)
    /// Their website
    website (icon: .globe)
    /// The day they were born
    birthday (icon: .calendar)
    /// What is written about them
    notes (icon: .file_text)
}

/// The workspace's address book.
type Book {
    contacts: Collection<Contact>
    groups: Collection<Group>
    /// Cards a file held whose id the book already has with other content: each waits for someone
    /// to say which card is the person's.
    review: Collection<ImportCard>
    /// What the last import did, until the person has read it.
    last_import: ImportReport
    /// The import running now, if one is: its row says how far the file was read.
    imports: Collection<ImportRun>
}

root book: Book

default view book_window for Book

default view contact_page for Contact

row view contact_row for Contact

place view contacts for Book

/// A new contact, named by what is typed where they were made; a book's entry gives the name
/// it takes, so nothing is left called "No name yet" by a person who typed one.
primary action new_contact(book: Book, name: Text = "") -> Contact (
    icon: .plus,
    label: "New contact",
    did: "added a contact",
) {
    book.contacts.insert(Contact(name: name))
}

/// Finds the contact the workspace keeps under `name`, or adds one: what brings someone in from
/// another service — a task given to them before they joined — to one card, however many imports
/// name them.
action named_or_new(book: Book, name: Text) -> Contact (
    icon: .user_plus,
    label: "Find or add a contact",
    did: "found a contact",
) {
    require not name.trim().is_empty() "Name the contact"
    let trimmed = name.trim()
    if let found = book.contacts.find(c -> c.name.lower() == trimmed.lower()) {
        found
    } else {
        book.contacts.insert(Contact(name: trimmed))
    }
}

/// Renames a contact.
action rename(c: Contact, name: Text) (icon: .pencil, label: "Rename", did: "renamed a contact") {
    c.name = name
}

/// Adds a contact to favorites, or takes them off.
action favorite(c: Contact, on: Bool = not c.favorite) (
    label: if on { "Add to favorites" } else { "Remove from favorites" },
    icon: .star,
    on: c.favorite,
) {
    c.favorite = on
}

/// Gives a contact a phone number, as the number `number` and the kind `label` where the caller
/// knows them already. Addressed from the card's own button, both are empty for the person to
/// fill in.
action add_phone(c: Contact, number: Text = "", label: Text = "") -> Labeled (
    icon: .phone,
    label: "Add phone",
    did: "added a phone number",
) {
    c.phones.insert(Labeled(label: label, value: number))
}

/// Takes one of a contact's phone numbers away.
action remove_phone(c: Contact, phone: Labeled) (icon: .trash, label: "Remove phone") {
    c.phones.remove(phone)
}

/// Gives a contact a mailbox, as the `address` and the kind `label` where the caller knows them
/// already. Addressed from the card's own button, both are empty for the person to fill in.
action add_email(c: Contact, address: Text = "", label: Text = "") -> Labeled (
    icon: .mail,
    label: "Add email",
    did: "added an email address",
) {
    c.emails.insert(Labeled(label: label, value: address))
}

/// Takes one of a contact's mailboxes away.
action remove_email(c: Contact, mail: Labeled) (icon: .trash, label: "Remove email") {
    c.emails.remove(mail)
}

/// Gives a contact a postal address to fill in.
action add_address(c: Contact) -> Address (
    icon: .map_pin,
    label: "Add address",
    did: "added an address",
) {
    c.addresses.insert(
        Address(label: "", street: "", city: "", region: "", postal: "", country: ""),
    )
}

/// Takes one of a contact's addresses away.
action remove_address(c: Contact, address: Address) (icon: .trash, label: "Remove address") {
    c.addresses.remove(address)
}

/// Keeps a birthday with its year or without it: a birthday kept without one is the day it falls
/// on every year, as many address books keep one.
action keep_birthday_year(c: Contact, on: Bool) (
    icon: .calendar,
    label: "The birthday's year",
    on: c.birthday_year,
    did: "changed a birthday's year",
) {
    c.birthday_year = on
}

/// Puts a contact in a group, making the group the book's place if it is new: the same group
/// however it is spelled, so "Work" and "work" are one place.
action group(book: Book, c: Contact, name: Text) (icon: .tag, label: "Add to group") {
    require not name.trim().is_empty() "A group needs a name"
    let trimmed = name.trim()
    c.groups.add(trimmed)
    if book.groups.find(g -> g.name.lower() == trimmed.lower()) == .none {
        book.groups.insert(Group(name: trimmed))
    }
}

/// Takes a contact out of a group.
action ungroup(c: Contact, name: Text) (icon: .tag, label: "Remove from group") {
    c.groups.remove(name)
}

/// Starts a group the book offers as a place, with nobody in it yet.
action new_group(book: Book, name: Text) -> Group (icon: .plus, label: "New group") {
    require not name.trim().is_empty() "A group needs a name"
    let trimmed = name.trim()
    if let had = book.groups.find(g -> g.name.lower() == trimmed.lower()) {
        return had
    }
    book.groups.insert(Group(name: trimmed))
}

/// Deletes a contact from the book, and with it the person the workspace's apps know of it where
/// the window hands the directory: what Mail, Calendar and Messages reach it by goes too.
destructive action delete(book: Book, c: Contact, directory: Option<contacts.Identities> = .none) (
    icon: .trash,
    key: "Delete",
    did: "deleted a contact",
) {
    if let d = directory {
        if c.person != "" { contacts.remove_contact(d, c.person) }
    }
    book.contacts.remove(c)
}

/// Merges the card that goes into the one that stays: every number, mailbox, address and group of
/// both is the card that stays, the fields `take` names come from the card that goes, and the
/// card that goes is deleted. One change, so undo brings both cards back.
destructive action merge_into(
    book: Book,
    keep: Contact,
    goes: Contact,
    take: List<MergeField> = [],
    directory: Option<contacts.Identities> = .none,
) (icon: .layers, label: "Merge contacts", did: "merged a duplicate") {
    require keep != goes "A contact cannot be merged into itself"
    if take.any(f -> f == .name) and not goes.name.trim().is_empty() {
        keep.name = goes.name
    }
    if take.any(f -> f == .org) and not goes.org.trim().is_empty() {
        keep.org = goes.org
    }
    if take.any(f -> f == .role) and not goes.role.trim().is_empty() {
        keep.role = goes.role
    }
    if take.any(f -> f == .website) and not goes.website.trim().is_empty() {
        keep.website = goes.website
    }
    if take.any(f -> f == .birthday) and goes.birthday != .none {
        keep.birthday = goes.birthday
    }
    if take.any(f -> f == .notes) and not goes.notes.is_empty() {
        keep.notes = RichText(keep.notes, goes.notes)
    }
    if goes.favorite {
        keep.favorite = true
    }
    // Every row of the card that goes is the card that stays: its own row there, with the values
    // the other card wrote and its own id, so the two cards' rows are never one row between them.
    for p in goes.phones {
        keep.phones.insert(Labeled(label: p.label, value: p.value))
    }
    for e in goes.emails {
        keep.emails.insert(Labeled(label: e.label, value: e.value))
    }
    for a in goes.addresses {
        keep.addresses.insert(
            Address(
                label: a.label,
                street: a.street,
                city: a.city,
                region: a.region,
                postal: a.postal,
                country: a.country,
            ),
        )
    }
    for g in goes.groups {
        group(book, keep, g)
    }
    // One person stands in the directory where the cards shared one: the person of the card that
    // goes is tombstoned, and a kept card the apps know is projected again with what it now holds.
    if let d = directory {
        if goes.person != "" { contacts.remove_contact(d, goes.person) }
        if keep.person != "" {
            contacts.project_contact(d, keep.person, keep.name)
            contacts.project_phones(d, keep.person, keep.phones.map(p -> p.value))
            contacts.project_emails(d, keep.person, keep.emails.map(e -> e.value))
        }
    }
    book.contacts.remove(goes)
}

/// The contacts that may be the same person as `c`: another card of the book that shares a mailbox
/// or a phone number with it, whatever the spelling. A name that reads alike is not one: only a
/// number or an address says two cards are someone.
fn twins(book: Book, c: Contact) -> List<Contact> {
    book.contacts
        .filter(
            other -> other != c
                and (other.emails.any(e -> c.emails.any(m -> same_address(m.value, e.value)))
                    or other.phones.any(p -> c.phones.any(m -> same_number(m.value, p.value)))),
        )
        .take(8)
}

/// Whether two mailboxes are the same one, ignoring case and the space around them.
fn same_address(one: Text, other: Text) -> Bool {
    let a = one.trim().lower()
    let b = other.trim().lower()
    not a.is_empty() and a == b
}

/// Whether two phone numbers name the same number: the digits and the `+`, whatever the spacing,
/// the dashes and the brackets people write them with.
fn same_number(one: Text, other: Text) -> Bool {
    let a = number_digits(one)
    not a.is_empty() and a == number_digits(other)
}

/// A phone number's dialable characters: digits, and the `+` that names a country.
fn number_digits(value: Text) -> Text {
    value
        .trim()
        .replace(" ", "")
        .replace("-", "")
        .replace("(", "")
        .replace(")", "")
        .replace(".", "")
}

/// The versions of the card format another address book reads.
enum VCardVersion {
    /// vCard 4.0, the current format: dates in its compact form and kinds in lower case.
    v4
    /// vCard 3.0, which older address books read: dates with their dashes and kinds in capitals.
    v3
}

/// The characters a dial string carries: digits and the separators people write numbers with.
fn dial_char(ch: Text) -> Bool {
    "0123456789".contains(ch)
        or ch == "+"
        or ch == "*"
        or ch == "#"
        or ch == "-"
        or ch == "."
        or ch == "("
        or ch == ")"
}

/// A number as the dial string a `tel:` or `sms:` link carries, or nothing when no digit is left:
/// a number with no digit names nobody.
fn dialable(value: Text) -> Text {
    let dial = value.split("").filter(ch -> dial_char(ch)).join("")
    if dial.split("").any(ch -> "0123456789".contains(ch)) { dial } else { "" }
}

/// A number as the `tel:` address the app that takes a call opens, or empty for a value with
/// nothing to dial.
fn tel_uri(value: Text) -> Text {
    let dial = dialable(value)
    if dial.is_empty() { "" } else { "tel:{dial}" }
}

/// A number as the `sms:` address the app that takes a message opens, or empty.
fn sms_uri(value: Text) -> Text {
    let dial = dialable(value)
    if dial.is_empty() { "" } else { "sms:{dial}" }
}

/// Whether a character may stand in an address a message goes to.
fn address_char(ch: Text) -> Bool {
    "abcdefghijklmnopqrstuvwxyz0123456789".contains(ch.lower())
        or ch == "-"
        or ch == "."
        or ch == "_"
        or ch == "~"
        or ch == "@"
        or ch == "+"
        or ch == "!"
}

/// A mailbox as the `mailto:` address the app that takes mail opens, or empty when it is not an
/// address: an `@` with something on each side, and nothing a recipient cannot be.
fn mailto_uri(value: Text) -> Text {
    let said = value.trim()
    if said.is_empty() or not said.split("").all(ch -> address_char(ch)) {
        return ""
    }
    let at = said.find("@") ?? -1
    if at <= 0 or at == said.len() - 1 or mailto_uri_tail(said, at) {
        return ""
    }
    "mailto:{said}"
}

/// Whether a mailbox's domain carries another `@`: an address has one.
fn mailto_uri_tail(value: Text, at: Int) -> Bool {
    value.slice(at + 1, value.len()).contains("@")
}

/// One contact as a card of `version`: the text a reader of the format takes, its lines folded
/// at the length the format allows and ended the way it ends them.
fn vcard_of(c: Contact, version: VCardVersion) -> Text {
    let lines = ["BEGIN:VCARD", if version == .v3 { "VERSION:3.0" } else { "VERSION:4.0" }]
    if not c.uid.is_empty() {
        lines = [lines, ["UID:{vcard_text(c.uid)}"]].flat_map(part -> part)
    }
    if not c.name.trim().is_empty() {
        lines = [lines, ["FN:{vcard_text(c.name)}"]].flat_map(part -> part)
        // The structured name older readers show in place of the full one: the family name after
        // the last space, the given name before it.
        lines = [lines, ["N:{vcard_text(family_of(c.name))};{vcard_text(given_of(c.name))};;;"]]
            .flat_map(part -> part)
    }
    if not c.org.trim().is_empty() {
        lines = [lines, ["ORG:{vcard_text(c.org)}"]].flat_map(part -> part)
    }
    if not c.role.trim().is_empty() {
        lines = [lines, ["TITLE:{vcard_text(c.role)}"]].flat_map(part -> part)
    }
    if not c.website.trim().is_empty() {
        // A link is a URI, not text: what would escape words is its own characters here.
        lines = [lines, ["URL:{link_of(c.website)}"]].flat_map(part -> part)
    }
    if let day = c.birthday {
        lines = [lines, [birthday_line(day, c.birthday_year, version)]].flat_map(part -> part)
    }
    if not c.groups.is_empty() {
        lines = [lines, ["CATEGORIES:{categories(c)}"]].flat_map(part -> part)
    }
    if not c.notes.is_empty() {
        lines = [lines, ["NOTE:{vcard_text(c.notes.text)}"]].flat_map(part -> part)
    }
    if not c.photo.trim().is_empty() {
        lines = [lines, [photo_line(c)]].flat_map(part -> part)
    }
    let numbers = c.phones.map(p -> "TEL;TYPE={vcard_type(p.label, version)}:{vcard_text(p.value)}")
    let mails = c.emails.map(e -> "EMAIL;TYPE={vcard_type(e.label, version)}:{vcard_text(e.value)}")
    let places = c.addresses.map(
        a -> "ADR;TYPE={vcard_type(a.label, version)}:;;{vcard_text(a.street)};{vcard_text(a.city)};{vcard_text(a.region)};{vcard_text(a.postal)};{vcard_text(a.country)}",
    )
    let all = [lines, numbers, mails, places, ["END:VCARD"]].flat_map(part -> part)
    "{all.flat_map(line -> folded(line)).join("\r\n")}\r\n"
}

/// A vCard line folded at the length the format allows: the first 73 characters, and each run of
/// 72 after it, which begins with the space that folds it onto the one before.
fn folded(line: Text) -> List<Text> {
    let parts: List<Text> = []
    let piece = ""
    for piece_character in line.split("") {
        if piece.len() >= 73 {
            parts = [parts, [piece]].flat_map(each -> each)
            piece = " "
        }
        piece = "{piece}{piece_character}"
    }
    [parts, [piece]].flat_map(each -> each)
}

/// The given name a full name carries: what stands before its last word.
fn given_of(name: Text) -> Text {
    let words = name.trim().split(" ")
    if words.len() <= 1 { "" } else { words.take(words.len() - 1).join(" ") }
}

/// The family name a full name carries: its last word.
fn family_of(name: Text) -> Text {
    let words = name.trim().split(" ")
    if words.len() <= 1 { name.trim() } else { words.last() ?? "" }
}

/// A link as a card writes it: its own characters, without a line break.
fn link_of(value: Text) -> Text {
    value.trim().replace("\n", "")
}

/// A contact's birthday as a card of `version` writes it: the day with its year, or the day
/// alone for one kept without the year.
fn birthday_line(day: Date, with_year: Bool, version: VCardVersion) -> Text {
    if with_year {
        if version == .v3 {
            "BDAY:{day.iso()}"
        } else {
            "BDAY:{day.year()}{pad2(day.month())}{pad2(day.day())}"
        }
    } else if version == .v3 {
        "BDAY:--{pad2(day.month())}-{pad2(day.day())}"
    } else {
        "BDAY:--{pad2(day.month())}{pad2(day.day())}"
    }
}

/// A contact as a file: their card, named for them. The system's one Export… lists it and saves
/// it where the person says.
action export_contact(c: Contact, version: VCardVersion = .v4) -> File (
    icon: .download,
    label: "Export as vCard",
    export: "vCard",
) {
    File.of_text("{file_name(c)}.vcf", vcard_of(c, version))
}

/// Every contact as one vCard file, which another address book reads whole.
action export_book(book: Book, version: VCardVersion = .v4) -> File (
    icon: .download,
    label: "Export all as vCard",
    export: "vCard 4.0",
) {
    let cards = book.contacts.sort_by(c -> c.name.lower()).map(c -> vcard_of(c, version))
    File.of_text("Contacts.vcf", cards.join(""))
}

/// Every contact as one vCard 3.0 file, which an address book that reads no 4.0 card opens.
action export_book_v3(book: Book) -> File (
    icon: .download,
    label: "Export all as vCard 3.0",
    export: "vCard 3.0",
) {
    export_book(book, version: .v3)
}

/// Every contact as the text of one card file, or the one `name` calls: for an agent or a script
/// that hands the cards on where a person's file chooser cannot, as the address its bytes are
/// kept at. A name the book holds twice is refused rather than guessed.
action export_vcard_content(book: Book, name: Text = "") -> File (
    icon: .download,
    label: "Export the vCard text",
) {
    require name.trim().is_empty()
        or book.contacts.count(c -> c.name.trim().lower() == name.trim().lower()) == 1 "Name one contact the book keeps under that name, or leave the name out"
    let said = name.trim().lower()
    let called = book.contacts.filter(c -> c.name.trim().lower() == said)
    if said.is_empty() {
        export_book(book)
    } else if let one = called.first() { export_contact(one) } else { export_book(book) }
}

/// The most one card file may be: a larger file is refused before it is read, rather than read
/// into the window in part.
fn max_import_bytes() -> Int {
    8388608
}

/// How many cards one file may hold.
fn max_import_cards() -> Int {
    2048
}

/// How many cards may wait for review at once: a file that changes more than this of the book's
/// contacts is taken in part, and says so.
fn max_review_cards() -> Int {
    64
}

/// Starts importing a vCard file: the file is kept as the import running now, and the window's
/// row says where it stands while `step_import` deals its cards a set at a time. A file beyond
/// what this app reads is refused whole: what the refusal says is why.
action begin_import(book: Book, file: File) (
    icon: .file_plus,
    label: "Import vCards",
    opens: ["text/vcard", "text/x-vcard", .vcf, .vcard],
) {
    require file.size <= max_import_bytes() "The file is larger than the 8 MB this app imports"
    for old in book.imports.map(run -> run) {
        book.imports.remove(old)
    }
    book.imports.insert(ImportRun(filename: file.name, file: file))
}

/// Deals the import running now: reads the file once, then takes a set of its cards, so the
/// window paints where it stands between sets. What the view runs while its row shows.
action step_import(book: Book) uses time, random (label: "Take the next cards of an import") {
    if let run = book.imports.first() {
        if run.total < 0 {
            match run.file.text() {
                .ok(text) => {
                    run.cards = cards_of(text)
                    run.total = run.cards.len()
                }
                // The file's bytes could be read a moment ago and not now: no card was taken.
                .err(_) => {
                    book.imports.remove(run)
                    report_import(book, 0, 0, 0, 1)
                }
            }
            return
        }
        for card in rest_of(run.cards, run.done).take(200) {
            let fate = file_card(book, card)
            run.done += 1
            if fate == .taken {
                run.imported += 1
            }
            if fate == .held {
                run.unchanged += 1
            }
            if fate == .review {
                run.awaiting += 1
            }
            if fate == .skipped {
                run.skipped += 1
            }
        }
        if run.done >= run.total {
            report_import(book, run.imported, run.unchanged, run.awaiting, run.skipped)
            book.imports.remove(run)
        }
    }
}

/// The cards a vCard file's words hold, as each card's own text: one that ends no card is kept
/// as the empty card, which the importer refuses.
fn cards_of(text: Text) -> List<Text> {
    let out: List<Text> = []
    for piece in text.split("BEGIN:VCARD") {
        let card = piece.trim()
        if not card.is_empty() {
            let whole = if card.contains("END:VCARD") { card } else { "" }
            out = [out, [whole]].flat_map(x -> x)
        }
    }
    out
}

/// What is left of `items` from `from` on.
fn rest_of(items: List<Text>, from: Int) -> List<Text> {
    let out: List<Text> = []
    let at: Int = 0
    for item in items {
        if at >= from {
            out = [out, [item]].flat_map(x -> x)
        }
        at = at + 1
    }
    out
}

/// Imports the contacts a vCard file holds into the book, each new card a contact of its own, and
/// answers what it did. A card whose id the book already holds unchanged is nobody new; one that
/// says something else about the person waits in the book's review, and the contact stays as it
/// is until someone says which card is theirs.
action import_vcard(book: Book, file: File) -> ImportReport (
    icon: .file_plus,
    label: "Import vCards now",
    did: "imported contacts",
) {
    require file.size <= max_import_bytes() "The file is larger than the 8 MB this app imports"
    match file.text() {
        .ok(text) => add_contacts(book, text),
        // The file's bytes could be read a moment ago and not now: what the file holds is not
        // here, so no card was taken.
        .err(_) => refuse_import(book, 1),
    }
}

/// The cards `cards`, the words of a vCard file, as contacts of this book, and what became of
/// each: what an importer, an agent or a script hands a book in one call.
action add_contacts(book: Book, cards: Text) -> ImportReport uses time, random (
    icon: .plus,
    label: "Add contacts",
    did: "added contacts",
) {
    require card_count(cards) <= max_import_cards() "The file holds more than the 2048 cards this app imports"
    let taken: Int = 0
    let held: Int = 0
    let waiting: Int = 0
    let skipped: Int = 0
    for piece in cards.split("BEGIN:VCARD") {
        let card = piece.trim()
        // The words before the first card are the file's, not a card's; a piece that ends no card
        // is a card this app cannot read.
        if not card.is_empty() {
            if not card.contains("END:VCARD") {
                skipped = skipped + 1
            } else {
                let fate = file_card(book, card)
                if fate == .taken {
                    taken = taken + 1
                }
                if fate == .held {
                    held = held + 1
                }
                if fate == .review {
                    waiting = waiting + 1
                }
                if fate == .skipped {
                    skipped = skipped + 1
                }
            }
        }
    }
    report_import(book, taken, held, waiting, skipped)
}

/// The report of an import of `skipped` cards, and no others: what a file that could not be read
/// answers.
action refuse_import(book: Book, skipped: Int) -> ImportReport {
    report_import(book, 0, 0, 0, skipped)
}

/// What an import did, kept on the book for the window's banner to read and answer.
action report_import(
    book: Book,
    imported: Int,
    unchanged: Int,
    awaiting: Int,
    skipped: Int,
) -> ImportReport {
    book.last_import.imported = imported
    book.last_import.unchanged = unchanged
    book.last_import.awaiting = awaiting
    book.last_import.skipped = skipped
    book.last_import.seen = false
    book.last_import
}

/// The person has read what the last import did: the banner stops showing it.
action dismiss_import(book: Book) (icon: .x, label: "Dismiss the import report") {
    book.last_import.seen = true
}

/// Brings one card of a vCard file to the book: takes it as a contact, finds the book already
/// holds it unchanged, puts it up for review where it says something else about a contact the
/// book holds by the same id, or refuses a card that names nobody. `uid` is the id the caller
/// gives a card whose file names none, so the caller can find the contact it makes.
action file_card(book: Book, card: Text, uid: Text = "") -> CardFate uses time, random (
    icon: .file_plus,
    label: "File a vCard's card",
    did: "filed a card",
) {
    let lines = card_lines(card)
    // A card that names nobody is not a person: what it holds is the card's format, not someone
    // to keep.
    if not names_anyone(lines) {
        return .skipped
    }
    let id = if uid != "" { uid } else { card_uid(lines) ?? random.id() }
    if let held = book.contacts.find(c -> c.uid == id) {
        // The card the book already holds, unchanged: reading a file again changes nothing.
        if same_card(held, lines, now) {
            return .held
        }
        // A card that says something else about the person waits for someone to say which card
        // is theirs: the contact stays as it is until they do.
        if book.review.len() >= max_review_cards() {
            return .skipped
        }
        // Reading a file once more replaces what the last reading put up, rather than adding to
        // it: the same id has one card waiting for the person.
        if let waiting = book.review.find(p -> p.uid == id) { book.review.remove(waiting) }
        book.review.insert(
            ImportCard(card: card, name: card_name(lines), uid: id, existing: .some(held)),
        )
        return .review
    }
    take_card(book, card, id: id)
    return .taken
}

/// The contact a card makes in the book, under the id `id`: what a new card becomes, and what a
/// card someone chose over the one the book held becomes. Its rows are filled after it stands in
/// the book, as every resource's are.
action take_card(book: Book, card: Text, id: Text = "") -> Contact uses time, random (
    icon: .file_plus,
    label: "Take a vCard's card",
    did: "took a card",
) {
    let lines = card_lines(card)
    let uid = if id != "" { id } else { card_uid(lines) ?? random.id() }
    let person = book.contacts.insert(
        Contact(
            name: card_name(lines),
            org: card_org(lines),
            role: card_role(lines),
            website: card_site(lines),
            notes: RichText(card_notes(lines)),
            photo: photo_of(lines.find(line -> starts(line, "PHOTO")), card_name(lines)),
            birthday: card_birthday(lines, now),
            birthday_year: card_birthday_has_year(lines),
            uid: uid,
        ),
    )
    for line in lines.filter(line -> starts(line, "TEL")) {
        person.phones.insert(Labeled(label: vcard_kind(line), value: tel_value(vcard_read(line))))
    }
    for line in lines.filter(line -> starts(line, "EMAIL")) {
        person.emails.insert(Labeled(label: vcard_kind(line), value: vcard_read(line)))
    }
    for line in lines.filter(line -> starts(line, "ADR")) {
        let parts = parts_unescaped(vcard_raw(line), ";")
        person.addresses.insert(
            Address(
                label: vcard_kind(line),
                street: street_of(parts),
                city: parts.at(3) ?? "",
                region: parts.at(4) ?? "",
                postal: parts.at(5) ?? "",
                country: parts.at(6) ?? "",
            ),
        )
    }
    for name in card_groups(lines) {
        person.groups.add(name)
        group(book, person, name)
    }
    person
}

/// The contact a vCard card says, made in the book, or none for a card that names nobody and for
/// one the book already holds: the single-card form of importing a file.
action add_card(book: Book, card: Text) -> Option<Contact> uses time, random (
    icon: .file_plus,
    label: "Add a vCard's card",
    did: "added a card",
) {
    let id = card_uid(card_lines(card)) ?? random.id()
    if file_card(book, card, uid: id) == .taken {
        book.contacts.find(c -> c.uid == id)
    } else { .none }
}

/// Takes a card that waited for review as a contact of its own, beside the one the book holds
/// under the id it named: the file's card is the person's, and the card the book kept keeps
/// everything of its own. Its id is new, because the two cards are two now.
action import_as_new(book: Book, p: ImportCard) -> Contact uses time, random (
    icon: .file_plus,
    label: "Import as a new contact",
    did: "imported a card as a new contact",
) {
    let made = take_card(book, p.card, id: random.id())
    book.review.remove(p)
    made
}

/// Leaves the card the book holds as it is, and takes the file's card out of the review.
action keep_existing(book: Book, p: ImportCard) (
    icon: .check,
    label: "Keep the existing contact",
    did: "kept an existing contact",
) {
    book.review.remove(p)
}

/// How many cards the words of a vCard file hold.
fn card_count(cards: Text) -> Int {
    cards.split("BEGIN:VCARD").len() - 1
}

/// Whether the card a file wrote says the same things the book's contact holds: the same name,
/// work, title, website, day and notes, and the same numbers, mailboxes, addresses and groups,
/// however either side writes them.
fn same_card(c: Contact, lines: List<Text>, now: Time) -> Bool {
    same_words(c.name, card_name(lines))
        and same_words(c.org, card_org(lines))
        and same_words(c.role, card_role(lines))
        and same_words(c.website, card_site(lines))
        and same_words(c.notes.text, card_notes(lines))
        and (c.birthday?.iso() ?? "") == (card_birthday(lines, now)?.iso() ?? "")
        and c.birthday_year == card_birthday_has_year(lines)
        and contact_rows(c) == card_rows(lines)
}

/// Whether two words are the same: what is around them is not part of either.
fn same_words(one: Text, other: Text) -> Bool {
    one.trim() == other.trim()
}

/// A contact's numbers, mailboxes, addresses and groups as one sorted list of rows, each the kind
/// and the values it holds: what a card must say the same to be the card the book holds.
fn contact_rows(c: Contact) -> List<Text> {
    let rows = c.phones.map(p -> "tel|{p.label}|{p.value}")
    rows = [rows, c.emails.map(e -> "email|{e.label}|{e.value}")].flat_map(part -> part)
    rows = [
        rows,
        c.addresses.map(
            a -> "address|{a.label}|{a.street}|{a.city}|{a.region}|{a.postal}|{a.country}",
        ),
    ]
        .flat_map(part -> part)
    rows = [rows, c.groups.map(g -> "group|{g}")].flat_map(part -> part)
    rows.map(row -> row.trim().lower()).sort_by(row -> row)
}

/// The numbers, mailboxes, addresses and groups a card's words say, as `contact_rows` reads the
/// contact's: the card's side of the same comparison.
fn card_rows(lines: List<Text>) -> List<Text> {
    let rows = lines
        .filter(line -> starts(line, "TEL"))
        .map(line -> "tel|{vcard_kind(line)}|{tel_value(vcard_read(line))}")
    rows = [
        rows,
        lines
            .filter(line -> starts(line, "EMAIL"))
            .map(line -> "email|{vcard_kind(line)}|{vcard_read(line)}"),
    ]
        .flat_map(part -> part)
    rows = [rows, lines.filter(line -> starts(line, "ADR")).map(line -> card_address_row(line))]
        .flat_map(part -> part)
    rows = [rows, card_groups(lines).map(name -> "group|{name}")].flat_map(part -> part)
    rows.map(row -> row.trim().lower()).sort_by(row -> row)
}

/// One of a card's `ADR` lines as a row of `card_rows`: its kind, and the box, extended address,
/// street, city, region, postal code and country it writes.
fn card_address_row(line: Text) -> Text {
    let parts = parts_unescaped(vcard_raw(line), ";")
    "address|{vcard_kind(line)}|{street_of(parts)}|{parts.at(3) ?? ""}|{parts.at(4) ?? ""}|{parts.at(5) ?? ""}|{parts.at(6) ?? ""}"
}

/// Whether a card's words name a person: a name, where they work or what they do, a website, a
/// day, a note, a group, or a number, mailbox or address that says something.
fn names_anyone(lines: List<Text>) -> Bool {
    not card_name(lines).is_empty()
        or not card_org(lines).is_empty()
        or not card_role(lines).is_empty()
        or not card_site(lines).is_empty()
        or not card_notes(lines).is_empty()
        or not card_groups(lines).is_empty()
        or lines.any(line -> starts(line, "BDAY"))
        or lines.any(
            line -> (starts(line, "TEL") or starts(line, "EMAIL") or starts(line, "ADR"))
                and not vcard_read(line).is_empty(),
        )
}

/// The logical lines of a card: a long line folded over the next, which starts with a space or a
/// tab, read as the one line it is, and a line ending in either kind of break.
fn card_lines(card: Text) -> List<Text> {
    unfolded(card)
        .split("\n")
        .map(line -> line.replace("\r", ""))
        .map(line -> line.trim())
        .filter(line -> not line.is_empty())
}

/// `text` with the line breaks of its folded lines taken out: a line written on two, the second
/// starting with the whitespace that folds it onto the first.
fn unfolded(text: Text) -> Text {
    text.replace("\r\n ", "").replace("\r\n\t", "").replace("\n ", "").replace("\n\t", "")
}

/// The name a card carries: the full name it gives, and where it gives none, the parts of its
/// structured name read as a person's name.
fn card_name(lines: List<Text>) -> Text {
    if let full = lines.find(line -> starts(line, "FN")) {
        let read = vcard_read(full)
        if not read.is_empty() {
            return read
        }
    }
    if let parts = lines.find(line -> starts(line, "N")) {
        return structured_name(vcard_raw(parts))
    }
    ""
}

/// A structured `N` as a person's name: `Family;Given;Additional;Prefix;Suffix` reads as the
/// given name and the family name, the rest being how a card addresses them.
fn structured_name(value: Text) -> Text {
    let parts = parts_unescaped(value, ";")
    let family = parts.at(0) ?? ""
    let given = parts.at(1) ?? ""
    if given.is_empty() { family } else if family.is_empty() { given } else { "{given} {family}" }
}

/// Where a card says they work: `ORG` may name a unit after the organization's name, and the
/// organization is the one.
fn card_org(lines: List<Text>) -> Text {
    if let line = lines.find(line -> starts(line, "ORG")) {
        return parts_unescaped(vcard_raw(line), ";").at(0) ?? ""
    }
    ""
}

/// What a card says they do there.
fn card_role(lines: List<Text>) -> Text {
    if let line = lines.find(line -> starts(line, "TITLE")) { vcard_read(line) } else { "" }
}

/// The website a card names.
fn card_site(lines: List<Text>) -> Text {
    if let line = lines.find(line -> starts(line, "URL")) { vcard_read(line) } else { "" }
}

/// What is written about the person.
fn card_notes(lines: List<Text>) -> Text {
    if let line = lines.find(line -> starts(line, "NOTE")) { vcard_read(line) } else { "" }
}

/// The id a card carries in its format, `urn:uuid:` spelling read as the id it holds: what the
/// book finds the same card by when the file is read again.
fn card_uid(lines: List<Text>) -> Option<Text> {
    if let line = lines.find(line -> starts(line, "UID")) {
        let said = vcard_read(line)
        let lower = said.lower()
        if lower.starts_with("urn:uuid:") and said.len() > 9 {
            return .some(said.slice(9, said.len()))
        }
        if not said.is_empty() {
            return .some(said)
        }
    }
    .none
}

/// The groups a card puts the person in: every name of its `CATEGORIES`, however many such lines
/// it writes.
fn card_groups(lines: List<Text>) -> List<Text> {
    let names: List<Text> = []
    for line in lines.filter(line -> starts(line, "CATEGORIES")) {
        for name in parts_unescaped(vcard_raw(line), ",") {
            if not name.is_empty() {
                names = [names, [name]].flat_map(part -> part)
            }
        }
    }
    names
}

/// The day a card's `BDAY` names, or none for a card without one or with a day no calendar
/// holds. A day a card keeps without its year is kept without one here too, in the year that
/// holds every day of the calendar.
fn card_birthday(lines: List<Text>, now: Time) -> Option<Date> {
    if let line = lines.find(found -> starts(found, "BDAY")) {
        let raw = vcard_raw(line)
        let said = if raw.starts_with("--") {
            yearless_date(raw)
        } else { vcard_date(raw.split("T").at(0) ?? "") }
        if let parsed = Date.parse(said, near: now) {
            if not card_birthday_has_year(lines) {
                return Date.parse("2000-{pad2(parsed.month())}-{pad2(parsed.day())}", near: now)
            }
            return .some(parsed)
        }
    }
    .none
}

/// Whether a card's `BDAY` keeps the year: `--` writes a day without one, and Apple names a
/// placeholder year in `X-APPLE-OMIT-YEAR` for a birthday whose year is unknown.
fn card_birthday_has_year(lines: List<Text>) -> Bool {
    if let line = lines.find(found -> starts(found, "BDAY")) {
        let head = line.slice(0, line.find(":") ?? line.len()).lower()
        if head.contains("x-apple-omit-year") {
            return false
        }
        return not vcard_raw(line).starts_with("--")
    }
    true
}

/// A day written without its year (`--0315`, `--03-15`) as the day it names in the year that
/// holds every one of them.
fn yearless_date(text: Text) -> Text {
    let digits = text.replace("-", "")
    if digits.len() == 4 {
        "2000-{digits.slice(0, 2)}-{digits.slice(2, 4)}"
    } else { text }
}

/// The first part of a postal address that says something: the box, the extended address and the
/// street are one delivery line here, as the card's own lines are read.
fn street_of(parts: List<Text>) -> Text {
    [parts.at(0) ?? "", parts.at(1) ?? "", parts.at(2) ?? ""]
        .filter(part -> not part.is_empty())
        .join("\n")
}

/// A number as people dial it: a `tel:` URI's scheme taken off, so `tel:+1 555` reads as the
/// number it names.
fn tel_value(value: Text) -> Text {
    if value.lower().starts_with("tel:") and value.len() > 4 {
        value.slice(4, value.len()).trim()
    } else { value }
}

/// The picture a card's PHOTO line names, or empty for a card without one: a `data:` or
/// `ENCODING=b` picture is kept as the workspace's content and this answers its address, and a
/// link is kept as it is. A card never makes the contact hold bytes.
fn photo_of(picture: Option<Text>, name: Text) -> Text {
    let value = if let line = picture { vcard_read(line) } else { "" }
    if value.is_empty() {
        return ""
    }
    let base64 = if value.starts_with("data:") {
        after_comma(value)
    } else if value.starts_with("http") or value.starts_with("tilo-") { "" } else { value }
    if base64.is_empty() {
        return value
    }
    kept_picture(name, base64)
}

/// `value` after its first comma, and nothing where it has none: the bytes a `data:` address
/// carries.
fn after_comma(value: Text) -> Text {
    if value.find(",") == .none {
        return ""
    }
    let comma = value.find(",") ?? 0
    value.slice(comma + 1, value.len())
}

/// The picture `base64` holds, kept as the workspace's content: the address another app and this
/// contact's avatar read it by.
fn kept_picture(name: Text, base64: Text) -> Text {
    let stem = if name.trim().is_empty() { "contact" } else { name.trim() }
    File.of_bytes("{stem}.jpg", Bytes.from_base64(base64)).url
}

/// The value of a vCard line, its name and parameters taken off, its escapes still in it.
fn vcard_raw(line: Text) -> Text {
    let colon = line.find(":") ?? line.len()
    let raw = if line.find(":") == .none { "" } else { line.slice(colon + 1, line.len()) }
    raw.trim()
}

/// A vCard value with its escapes read back: `\n` a line break, and `\\`, `\,` and `\;` the
/// characters they stand for, read in one pass so an escaped backslash before an `n` stays one.
fn vcard_unescape(value: Text) -> Text {
    let out = ""
    let escaped = false
    for piece in value.split("") {
        if escaped {
            out = if piece == "n" or piece == "N" { "{out}\n" } else { "{out}{piece}" }
            escaped = false
        } else if piece == "\\" {
            escaped = true
        } else {
            out = "{out}{piece}"
        }
    }
    out.trim()
}

/// The parts a structured value is written in, split at `separator` where it is not escaped and
/// each read back: `Family;Given` and a street written `12\;A` in a card's own reading.
fn parts_unescaped(value: Text, separator: Text) -> List<Text> {
    let parts: List<Text> = []
    let part = ""
    let escaped = false
    for piece in value.split("") {
        if escaped {
            part = if piece == "n" or piece == "N" { "{part}\n" } else { "{part}{piece}" }
            escaped = false
        } else if piece == "\\" {
            escaped = true
        } else if piece == separator {
            parts = [parts, [part.trim()]].flat_map(each -> each)
            part = ""
        } else {
            part = "{part}{piece}"
        }
    }
    [parts, [part.trim()]].flat_map(each -> each)
}

/// How a vCard writes a value: the characters that would end it or start a parameter escaped,
/// and a line break a `\n`.
fn vcard_text(value: Text) -> Text {
    value.replace("\\", "\\\\").replace("\n", "\\n").replace(";", "\\;").replace(",", "\\,")
}

/// A contact's groups as one CATEGORIES line: its names, escaped as the format writes them,
/// with a comma between them.
fn categories(c: Contact) -> Text {
    c.groups.map(g -> vcard_text(g)).join(",")
}

/// A vCard type tag: what kind of number, address or mailbox a line holds, or "voice" for one
/// the person did not say.
fn vcard_type(label: Text, version: VCardVersion) -> Text {
    let kind = label.trim().lower().replace(" ", "-")
    let kind = if kind.is_empty() { "voice" } else { kind }
    // 3.0 writes a kind in capitals; 4.0 in the case a person reads it.
    if version == .v3 { kind.upper() } else { kind }
}

/// `number` with a leading zero below ten, as a card writes the parts of a day.
fn pad2(number: Int) -> Text {
    if number < 10 { "0{number}" } else { "{number}" }
}

/// The value of a vCard line, its name and parameters taken off and its escapes read back.
fn vcard_read(line: Text) -> Text {
    vcard_unescape(vcard_raw(line))
}

/// What kind a vCard line says it is: the values of its `TYPE=` parameter, or its older bare tag
/// (`TEL;CELL:`) where it names no parameter. A kind that says how the value is written rather
/// than where it belongs (`voice`, `internet`, `pref`) is not one, so a card written
/// `TYPE=work,voice` reads as one whose number is their work's.
fn vcard_kind(line: Text) -> Text {
    let head = line.slice(0, line.find(":") ?? line.len())
    let parts = head.split(";")
    if let typed = parts.find(part -> part.trim().lower().starts_with("type=")) {
        let said = typed.trim().lower()
        let kinds = said.slice(5, said.len()).split(",")
        if let kind = kinds.find(k -> not format_only(k)) {
            return kind.trim()
        }
    }
    // The property's own name stands before the tags, and is no kind.
    let tags = parts.map((part, at) -> if at == 0 { "" } else { part.trim().lower() })
    let bare = tags.filter(part -> not part.is_empty() and not part.contains("="))
    if let kind = bare.filter(part -> not format_only(part)).last() {
        return kind
    }
    "other"
}

/// Whether a vCard kind says how a value is written rather than where it belongs: the tags the
/// format allows beside a place's own.
fn format_only(kind: Text) -> Bool {
    let said = kind.trim()
    said == "voice" or said == "internet" or said == "pref" or said == "text"
}

/// Whether a vCard line is the named one: the name up to its first `;` or `:` is `name`.
fn starts(line: Text, name: Text) -> Bool {
    line.starts_with("{name}:") or line.starts_with("{name};")
}

/// A vCard's BDAY as the day it names: `2026-03-04`, or the compact `20260304`. A day without
/// its year names no day of the calendar, so it stays one no birthday is made of.
fn vcard_date(text: Text) -> Text {
    let digits = text.replace("-", "")
    if digits.len() == 8 {
        "{digits.slice(0, 4)}-{digits.slice(4, 6)}-{digits.slice(6, 8)}"
    } else { text }
}

/// A contact's picture as the PHOTO line a reader of the format takes: the address the picture
/// stands at, which another reader of the card fetches as it fetches any link.
fn photo_line(c: Contact) -> Text {
    "PHOTO;VALUE=uri:{vcard_text(c.photo.trim())}"
}

/// The name a contact's file is offered as: their name without what a file name cannot keep.
fn file_name(c: Contact) -> Text {
    let safe = c.name
        .trim()
        .replace("/", "-")
        .replace("\\", "-")
        .replace(":", "-")
        .replace("\n", "-")
    if safe.is_empty() { "Contact" } else { safe }
}

/// The contact the book holds for `address`, as an action: what a person, an agent or another
/// app asks before they write to it. Nothing is changed and nothing is guessed from a name.
action find_for_address(book: Book, address: Text) -> Option<Contact> (
    icon: .search,
    label: "Find the contact for an address",
) {
    for_address(book, address)
}

/// The contact the book holds for a mail `address`, or none: what another app asks to open
/// someone it shows, and never a guess from a name.
fn for_address(book: Book, address: Text) -> Option<Contact> {
    let asked = address.trim().lower()
    if asked.is_empty() {
        return .none
    }
    book.contacts.find(c -> c.emails.any(e -> e.value.trim().lower() == asked))
}

/// The group whose place is `place`, where it is a group's.
fn group_place(book: Book, place: Text) -> Option<Group> {
    book.groups.find(g -> Contact.groups(g.name).query == place)
}

/// The key of the place that lists the cards waiting for review: no query of a contact's
/// answers it.
fn review_place() -> Text {
    "review"
}

/// Whether the last import left something to say: a report no one has answered yet.
fn import_shows(book: Book) -> Bool {
    let r = book.last_import
    not r.seen and r.imported + r.unchanged + r.awaiting + r.skipped > 0
}

/// What the last import did, in its title: how many cards the book took.
fn import_title(r: ImportReport) -> Text {
    if r.imported == 0 {
        std.say("No contacts were imported")
    } else if r.imported == 1 {
        std.say("Imported 1 contact")
    } else {
        std.say("Imported {r.imported} contacts")
    }
}

/// What the last import did, in a line under its title: the cards the book already held, the ones
/// waiting for review, and the ones it could not take.
fn import_words(r: ImportReport) -> Text {
    let said: List<Text> = []
    if r.unchanged > 0 {
        said = [said, [std.say("{r.unchanged} already current")]].flat_map(part -> part)
    }
    if r.awaiting == 1 {
        said = [said, [std.say("1 card needs review")]].flat_map(part -> part)
    } else if r.awaiting > 1 {
        said = [said, [std.say("{r.awaiting} cards need review")]].flat_map(part -> part)
    }
    if r.skipped > 0 {
        said = [said, [std.say("{r.skipped} could not be read")]].flat_map(part -> part)
    }
    said.join("; ")
}

/// One of a contact's rows as a line a review reads: the kinds and the values it holds.
/// A contact's rows as one line a review reads: each kind and the value it holds.
fn said_rows(rows: List<Labeled>) -> Text {
    let said = rows.map(row -> "{row.label}: {row.value}")
    said.join(", ")
}

/// The orders the window lists a book's people in: by name, and by where they work.
enum SortOrder {
    /// By name, ignoring case.
    name
    /// By the organization they work at, and by name within one.
    organization
}

/// The contacts in the order the window lists them: by name, or by where they work and then by
/// name within one organization.
fn listed(book: Book, order: SortOrder) -> List<Contact> {
    if order == .organization {
        book.contacts.sort_by(c -> "{c.org.lower()}~{c.name.lower()}")
    } else {
        book.contacts.sort_by(c -> c.name.lower())
    }
}

/// How many contacts of the book are in the group `name`, whatever its spelling: what a group's
/// place counts.
fn in_group(book: Book, name: Text) -> Int {
    book.contacts.count(c -> c.groups.any(g -> g.lower() == name.trim().lower()))
}

/// The letter a contact stands under in the list: the first letter of their name, as capitals
/// group an address book, and nothing for a card nobody named yet.
fn first_letter(name: Text) -> Option<Text> {
    let trimmed = name.trim()
    if trimmed.is_empty() {
        return .none
    }
    .some(trimmed.slice(0, 1).upper())
}

/// The name of a month, as a birthday without its year reads it.
fn month_word(month: Int) -> Text {
    if month == 1 {
        std.say("January")
    } else if month == 2 {
        std.say("February")
    } else if month == 3 {
        std.say("March")
    } else if month == 4 {
        std.say("April")
    } else if month == 5 {
        std.say("May")
    } else if month == 6 {
        std.say("June")
    } else if month == 7 {
        std.say("July")
    } else if month == 8 {
        std.say("August")
    } else if month == 9 {
        std.say("September")
    } else if month == 10 {
        std.say("October")
    } else if month == 11 { std.say("November") } else { std.say("December") }
}

/// The line under a contact's name: where they work and what they do there, then how to reach
/// them, which is what a person looks for in the list.
fn about(c: Contact) -> Option<Text> {
    if not c.org.trim().is_empty() and not c.role.trim().is_empty() {
        return "{c.org} · {c.role}"
    }
    if not c.org.trim().is_empty() {
        return c.org
    }
    if not c.role.trim().is_empty() {
        return c.role
    }
    if let mail = c.emails.first() {
        return mail.value
    }
    if let phone = c.phones.first() {
        return phone.value
    }
    .none
}

/// The contacts window: the book's places in the sidebar, the people in the one chosen beside
/// it, and the person opened in the column to their right.
view book_window(book: Book) {
    // The place chosen, by the query it asks (empty for every contact), the person opened, and
    // what the bar's search holds.
    state place: Text = ""
    state opened: Option<Contact> = .none
    state searched: Text = ""
    // The card the letter rail last asked the list to show: the row the list brings up.
    state revealing: Text = ""
    // The order the list stands in: by name, or by where people work.
    state order: SortOrder = .name
    // A contact a reference, an agent or another app asks for opens where the list stands: the
    // person the window shows is the one it was asked for.
    on show(c: Contact): set(opened, c)
    // The place chosen, named by the group it is where it is one and by the filter where it is
    // favorites: a group's name is the group's, never the field's.
    let favorites = Contact.favorites
    let group = group_place(book, place)
    let reviewing = place == review_place()
    let title = if reviewing {
        "To review"
    } else if let g = group {
        g.name
    } else {
        if place == favorites.query { favorites.label } else { "All contacts" }
    }
    let all = listed(book, order)
    let here = if place.is_empty() { all } else { all.matching(place) }
    let found = if searched.is_empty() { here } else { here.matching(searched) }
    ui.NavShell(
        content_kind: .document,
        sidebar_title: "Contacts",
        overview_title: title,
        overview_count: if reviewing { book.review.len() } else { found.len() },
    ) {
        on search(q: Text): set(searched, q)
        sidebar: {
            ui.NavList(selected: if place.is_empty() { "all" } else { place }) {
                ui.NavItem("all", "All contacts", icon: .users, count: book.contacts.len()) {
                    on select: set(place, "").then(set(opened, .none))
                }
                ui.NavItem(
                    favorites.query,
                    favorites.label,
                    icon: .star,
                    count: book.contacts.count(c -> c.favorite),
                ) {
                    on select: set(place, favorites.query).then(set(opened, .none))
                }
                // The cards a file held whose id the book already has, until the person says
                // which card is theirs: the badge counts what waits.
                if not book.review.is_empty() {
                    ui.NavItem(
                        review_place(),
                        "To review",
                        icon: .file_search,
                        live: book.review.len(),
                    ) {
                        on select: set(place, review_place()).then(set(opened, .none))
                    }
                }
                for g in book.groups.sort_by(g -> g.name.lower()) {
                    ui.NavItem(
                        Contact.groups(g.name).query,
                        g.name,
                        icon: .tag,
                        count: in_group(book, g.name),
                    ) {
                        on select: set(place, Contact.groups(g.name).query).then(set(opened, .none))
                    }
                }
            }
        }
        sidebar_actions: {
            ui.MenuButton("File", icon: .file_plus) {
                ui.MenuItem("Import vCards…", icon: .file_plus) {
                    on choose: file.choose(
                        begin_import(book),
                        accept: ["text/vcard", "text/x-vcard"],
                    )
                }
            }
            ui.MenuButton("Sort", icon: .arrow_up_down) {
                ui.MenuItem("By name", checked: order == .name) {
                    on choose: set(order, .name)
                }
                ui.MenuItem("By organization", checked: order == .organization) {
                    on choose: set(order, .organization)
                }
            }
        }
        overview: {
            ui.Docked {
                foot: {
                    stack {
                        std.add_entry("New contact", new_contact(book))
                    }
                }
                stack(fill, gap: .none) {
                    // A file being read and taken: the row says where it stands, and the timer
                    // deals its next set of cards so the window paints between them.
                    if let run = book.imports.first() {
                        stack(padding: .related) {
                            ui.ListRow(
                                title: "Importing {run.filename}",
                                secondary: if run.total < 0 {
                                    "Reading the file…"
                                } else { "{run.done} of {run.total} cards" },
                                icon: .file_plus,
                            )
                        }
                        every 1s: step_import(book)
                    }
                    // What the last import did, until the person answers it: the cards the book
                    // took, the ones it already held, and the ones waiting for review.
                    if import_shows(book) {
                        stack(padding: .related) {
                            ui.Banner(
                                import_title(book.last_import),
                                message: import_words(book.last_import),
                                kind: if book.last_import.skipped > 0 {
                                    .warning
                                } else if book.last_import.imported > 0 {
                                    .success
                                } else { .info },
                                icon: .file_plus,
                                action: "Dismiss",
                            ) {
                                on act: dismiss_import(book)
                            }
                        }
                    }
                    if reviewing {
                        review_cards(book)
                    } else if found.is_empty() {
                        ui.EmptyState(
                            if all.is_empty() { "No contacts yet" } else { "Nobody here" },
                            icon: .users,
                            inline: true,
                        )
                    } else {
                        // The cards and, beside them, the letters they stand under: a press brings
                        // the first card under its letter up, at the list's own motion.
                        // The list is the column's scroller: the stack takes the room the foot
                        // leaves, the row spans it, and the rail stands beside the list.
                        stack(fill, gap: .none) {
                            row(fill, align: .stretch, gap: .none) {
                                // A press asks for the first card under its letter; the list brings
                                // it up, and brings it up again when the person has scrolled away
                                // since.
                                std.list(
                                    found,
                                    opened,
                                    ungrouped: .some("Other"),
                                    reveal: if revealing.is_empty() {
                                        .none
                                    } else { .some(revealing) },
                                ) { (c, at) ->
                                    ui.ListItem(
                                        item: c,
                                        presence: at,
                                        group: first_letter(c.name),
                                    ) {
                                        contact_row(c)
                                    }
                                }
                                letter_rail(found, revealing)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The letters the cards stand under, each a press that brings the first card under it up: the
/// list moves at its own motion, and the ask is taken back once it has. One letter is no rail —
/// a short book reads whole.
view letter_rail(found: List<Contact>, revealing: State<Text>) {
    let at = first_by_letter(found)
    let letters = at.keys().sort_by(letter -> letter)
    if letters.len() > 1 {
        stack(gap: .none, align: .center) {
            for letter in letters {
                ui.Button(letter, role: .plain, size: .small) {
                    on press: set(revealing, at.get(letter) ?? "")
                }
            }
        }
    }
}

/// The first card under each letter of the list, by the letter: the row a letter press shows.
fn first_by_letter(found: List<Contact>) -> Map<Text> {
    let at: Map<Text> = Map<Text>()
    for c in found {
        if let letter = first_letter(c.name) {
            if at.get(letter) == .none { at.put(letter, c.id) }
        }
    }
    at
}

/// The cards a file held whose id a contact of the book already has: what each says beside what
/// the book keeps, and the two answers. Nothing of the book's contact changes until one is taken.
view review_cards(book: Book) {
    scroll {
        stack(padding: .related, gap: .related) {
            if book.review.is_empty() {
                ui.EmptyState("Nothing waits for review", icon: .file_search, inline: true)
            }
            text(
                "These cards name a contact the book keeps, and say something else about them. Take a card as a contact of its own, or leave the book's card as it is.",
                role: .caption,
                secondary: true,
            )
            for p in book.review.sort_by(p -> p.name.lower()) {
                review_card(book, p)
            }
        }
    }
}

/// One card waiting for review: the contact it names, the fields where the two say different
/// things, and the answers it offers.
view review_card(book: Book, p: ImportCard) {
    let lines = card_lines(p.card)
    ui.Card(title: p.name) {
        if let other = p.existing {
            text("The book keeps {other.name} under the same id.", role: .caption, secondary: true)
            if not same_words(other.name, card_name(lines)) {
                compared("Name", other.name, "In the book", card_name(lines), "In the card")
            }
            if not same_words(other.org, card_org(lines)) {
                compared("Organization", other.org, "In the book", card_org(lines), "In the card")
            }
            if not same_words(other.role, card_role(lines)) {
                compared("Job title", other.role, "In the book", card_role(lines), "In the card")
            }
            if not same_words(other.website, card_site(lines)) {
                compared("Website", other.website, "In the book", card_site(lines), "In the card")
            }
            if not same_words(other.notes.text, card_notes(lines)) {
                compared("Notes", other.notes.text, "In the book", card_notes(lines), "In the card")
            }
            if not same_words(said_rows(other.phones), card_phone_words(lines)) {
                compared(
                    "Phone numbers",
                    said_rows(other.phones),
                    "In the book",
                    card_phone_words(lines),
                    "In the card",
                )
            }
            if not same_words(said_rows(other.emails), card_mail_words(lines)) {
                compared(
                    "Email addresses",
                    said_rows(other.emails),
                    "In the book",
                    card_mail_words(lines),
                    "In the card",
                )
            }
            if not same_words(
                card_groups(lines).join(", "),
                other.groups.sort_by(g -> g.lower()).join(", "),
            ) {
                compared(
                    "Groups",
                    other.groups.sort_by(g -> g.lower()).join(", "),
                    "In the book",
                    card_groups(lines).join(", "),
                    "In the card",
                )
            }
        } else {
            text(
                "The contact it named is gone from the book; the card can still come in on its own.",
                role: .caption,
                secondary: true,
            )
        }
        row(gap: .related, wrap: true) {
            ui.Button("Import as new", icon: .file_plus) {
                on press: import_as_new(book, p)
            }
            if p.existing != .none {
                ui.Button("Keep existing", icon: .check) {
                    on press: keep_existing(book, p)
                }
            }
        }
    }
}

/// One field of two cards, as a review reads it: what the label calls it, and each side's value
/// under what names that side.
view compared(label: Text, mine: Text, my_side: Text, theirs: Text, their_side: Text) {
    stack(gap: .tight) {
        text(label, role: .subheading)
        ui.PropertyList {
            ui.Property(my_side, says(mine))
            ui.Property(their_side, says(theirs))
        }
    }
}

/// `value`, or a dash where it is empty: a review shows that a side says nothing rather than a
/// blank line.
fn says(value: Text) -> Text {
    if value.trim().is_empty() { "—" } else { value }
}

/// The numbers a card's words hold, as a line a review reads: each kind and its number.
fn card_phone_words(lines: List<Text>) -> Text {
    lines
        .filter(line -> starts(line, "TEL"))
        .map(line -> "{vcard_kind(line)}: {tel_value(vcard_read(line))}")
        .join(", ")
}

/// The mailboxes a card's words hold, as a line a review reads: each kind and its address.
fn card_mail_words(lines: List<Text>) -> Text {
    lines
        .filter(line -> starts(line, "EMAIL"))
        .map(line -> "{vcard_kind(line)}: {vcard_read(line)}")
        .join(", ")
}

/// One contact in a list: their name, where they work and how to reach them, and a star for a
/// favorite. A contact nobody named reads by the words the type gives one, as everywhere else.
view contact_row(c: Contact) {
    ui.ListRow(title: c.name, secondary: about(c), item: c, mark: if c.favorite { .star }) {
        avatar: {
            ui.Avatar(name: c.name, picture: c.photo)
        }
    }
}

/// A contact's own page: their name, how to reach them, where they live and what they are part
/// of, every part of it a control that writes the contact as it is typed.
view contact_page(c: Contact) {
    let book = root Book
    std.document(c) {
        stack(gap: .group) {
            stack(gap: .related) {
                row(gap: .related, align: .center) {
                    ui.Avatar(name: c.name, picture: c.photo)
                    ui.TextInput(bind: c.name, title: true, placeholder: "Name", fill)
                    ui.Button(
                        "Favorite",
                        icon: .star,
                        icon_only: true,
                        role: .plain,
                        toggled: c.favorite,
                    ) {
                        on press: favorite(c)
                    }
                }
                std.origin(c)
            }
            std.body() {
                field("Organization") {
                    ui.TextInput(bind: c.org, placeholder: "Where they work")
                }
                field("Job title") {
                    ui.TextInput(bind: c.role, placeholder: "What they do there")
                }
                field("Website") {
                    stack(gap: .tight) {
                        ui.TextInput(bind: c.website, placeholder: "https://")
                        if not c.website.trim().is_empty() {
                            ui.Link(c.website, label: c.website)
                        }
                    }
                }
                field("Birthday") {
                    ui.DatePicker(bind: c.birthday)
                }
                // A birthday many address books keep without its year: the day is the person's,
                // and it falls on that day every year.
                if c.birthday != .none {
                    stack(gap: .tight) {
                        ui.ToggleGroup(
                            selected: if c.birthday_year { 0 } else { 1 },
                            caption: "The year",
                        ) {
                            ui.Segment("Part of it") {
                                on press: keep_birthday_year(c, true)
                            }
                            ui.Segment("Left out") {
                                on press: keep_birthday_year(c, false)
                            }
                        }
                        // A day kept without its year reads as the day it falls on every year.
                        if not c.birthday_year {
                            if let day = c.birthday {
                                let month = month_word(day.month())
                                let on = day.day().to_text()
                                text("{month} {on}", role: .caption, secondary: true)
                            }
                        }
                    }
                }
                section("Phones") {
                    phones(c)
                }
                section("Email") {
                    emails(c)
                }
                section("Addresses") {
                    addresses(c)
                }
                section("Groups") {
                    groups(book, c)
                }
                // Who this card is in the workspace's directory, and the accounts the apps reach
                // them on: the review that binds one is the directory's own.
                section("Connected accounts") {
                    associations(c)
                }
                if not twins(book, c).is_empty() {
                    // A merge takes the review away and moves the card's rows: the section
                    // leaves at the calm pace, and the list follows the rows at its own.
                    section("Possibly the same person", motion: .calm) {
                        duplicates(book, c)
                    }
                }
                section("Notes") {
                    ui.BlockEditor(
                        bind: c.notes,
                        placeholder: "Anything worth remembering about them",
                    )
                }
                std.after_body {
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Button("Delete this contact", icon: .trash, role: .destructive) {
                            on press: delete(book, c, contacts.directory)
                        }
                    }
                    std.linked(c, compact: true)
                    std.history(c)
                }
            }
        }
    }
}

/// A contact's phone numbers, each with what kind it is, and the control that adds one.
view phones(c: Contact) {
    stack(gap: .tight) {
        if c.phones.is_empty() {
            text("No numbers yet.", role: .caption, secondary: true)
        }
        for p in c.phones {
            stack(gap: .tight) {
                row(gap: .tight, align: .center) {
                    ui.TextInput(bind: p.label, placeholder: "Home", min: 6rem, max: 10rem)
                    ui.TextInput(bind: p.value, placeholder: "Phone number", fill)
                    ui.Button("Remove phone", icon: .x, icon_only: true, role: .plain) {
                        on press: remove_phone(c, p)
                    }
                }
                // The app on this device that takes a call or a message opens it, as the system's
                // link vocabulary names it; a value with nothing to dial offers neither, and says
                // so rather than sending a person to a link that opens nothing.
                if not tel_uri(p.value).is_empty() {
                    row(gap: .related) {
                        ui.Link(tel_uri(p.value), label: "Call", role: .caption)
                        ui.Link(sms_uri(p.value), label: "Text", role: .caption)
                    }
                } else if not p.value.trim().is_empty() {
                    text("Nothing to dial in this number", role: .caption, secondary: true)
                }
            }
        }
        ui.Button("Add phone", icon: .plus, role: .plain) {
            on press: add_phone(c)
        }
    }
}

/// A contact's mailboxes, each with what kind it is, and the control that adds one.
view emails(c: Contact) {
    stack(gap: .tight) {
        if c.emails.is_empty() {
            text("No mailboxes yet.", role: .caption, secondary: true)
        }
        for e in c.emails {
            stack(gap: .tight) {
                row(gap: .tight, align: .center) {
                    ui.TextInput(bind: e.label, placeholder: "Work", min: 6rem, max: 10rem)
                    ui.TextInput(bind: e.value, placeholder: "Email address", fill)
                    ui.Button("Remove email", icon: .x, icon_only: true, role: .plain) {
                        on press: remove_email(c, e)
                    }
                }
                // A mailto: address opens the mail app that takes it, with a message to them; one
                // that is not an address opens nothing, and says so here.
                if not mailto_uri(e.value).is_empty() {
                    ui.Link(mailto_uri(e.value), label: "Write", role: .caption)
                } else if not e.value.trim().is_empty() {
                    text("Not an address a message can go to", role: .caption, secondary: true)
                }
            }
        }
        ui.Button("Add email", icon: .plus, role: .plain) {
            on press: add_email(c)
        }
    }
}

/// A contact's postal addresses, each a line under the kind it is, and the control that adds one.
view addresses(c: Contact) {
    stack(gap: .related) {
        if c.addresses.is_empty() {
            text("No addresses yet.", role: .caption, secondary: true)
        }
        for a in c.addresses {
            ui.Card {
                row(gap: .tight, align: .center) {
                    ui.TextInput(bind: a.label, placeholder: "Home", min: 6rem, max: 10rem)
                    ui.TextInput(bind: a.street, placeholder: "Street", fill)
                    ui.Button("Remove address", icon: .x, icon_only: true, role: .plain) {
                        on press: remove_address(c, a)
                    }
                }
                row(gap: .tight, align: .center) {
                    ui.TextInput(bind: a.city, placeholder: "City", fill)
                    ui.TextInput(bind: a.region, placeholder: "Region", fill)
                    ui.TextInput(bind: a.postal, placeholder: "Postal code", min: 6rem, max: 10rem)
                    ui.TextInput(bind: a.country, placeholder: "Country", fill)
                }
            }
        }
        ui.Button("Add address", icon: .plus, role: .plain) {
            on press: add_address(c)
        }
    }
}

/// The cards this one may be the same person as: what they share is why they are offered, and a
/// merge takes the fields ticked from the other card, with every number, mailbox, address and
/// group of both kept on this one.
view duplicates(book: Book, c: Contact) {
    state take_name: Bool = false
    state take_org: Bool = false
    state take_role: Bool = false
    state take_website: Bool = false
    state take_birthday: Bool = false
    state take_notes: Bool = false
    stack(gap: .related) {
        for other in twins(book, c) {
            ui.Card {
                row(gap: .related, align: .center) {
                    ui.Avatar(name: other.name, picture: other.photo)
                    ui.Text(other.name, role: .subheading)
                    spacer()
                    ui.Button("Merge into this card", icon: .layers) {
                        on press: merge_into(
                            book,
                            c,
                            other,
                            chosen(
                                take_name,
                                take_org,
                                take_role,
                                take_website,
                                take_birthday,
                                take_notes,
                            ),
                            contacts.directory,
                        )
                    }
                }
                text("They share {shared_with(c, other)}.", role: .caption, secondary: true)
                row(gap: .related) {
                    ui.Button("Select all", role: .plain, size: .small) {
                        on press: {
                            set(take_name, true)
                            set(take_org, true)
                            set(take_role, true)
                            set(take_website, true)
                            set(take_birthday, true)
                            set(take_notes, true)
                        }
                    }
                    ui.Button("Select none", role: .plain, size: .small) {
                        on press: {
                            set(take_name, false)
                            set(take_org, false)
                            set(take_role, false)
                            set(take_website, false)
                            set(take_birthday, false)
                            set(take_notes, false)
                        }
                    }
                }
                row(gap: .related, wrap: true) {
                    toggle(bind: take_name, label: "Take their name")
                    toggle(bind: take_org, label: "Take their organization")
                    toggle(bind: take_role, label: "Take their job title")
                    toggle(bind: take_website, label: "Take their website")
                    toggle(bind: take_birthday, label: "Take their birthday")
                    toggle(bind: take_notes, label: "Take their notes")
                }
                // What each card says, so the person chooses with both in front of them: the
                // values one of each comes from, and the rows a merge keeps of both.
                stack(gap: .related) {
                    compared("Name", c.name, "This card", other.name, "The other card")
                    compared("Organization", c.org, "This card", other.org, "The other card")
                    compared("Job title", c.role, "This card", other.role, "The other card")
                    compared("Website", c.website, "This card", other.website, "The other card")
                    compared(
                        "Birthday",
                        c.birthday?.iso() ?? "",
                        "This card",
                        other.birthday?.iso() ?? "",
                        "The other card",
                    )
                    compared("Notes", c.notes.text, "This card", other.notes.text, "The other card")
                    compared(
                        "Phone numbers",
                        said_rows(c.phones),
                        "This card",
                        said_rows(other.phones),
                        "The other card",
                    )
                    compared(
                        "Email addresses",
                        said_rows(c.emails),
                        "This card",
                        said_rows(other.emails),
                        "The other card",
                    )
                    compared(
                        "Groups",
                        c.groups.sort_by(g -> g.lower()).join(", "),
                        "This card",
                        other.groups.sort_by(g -> g.lower()).join(", "),
                        "The other card",
                    )
                }
            }
        }
    }
}

/// The fields a merge takes, from the toggles the person set.
fn chosen(
    name: Bool,
    org: Bool,
    role: Bool,
    website: Bool,
    birthday: Bool,
    notes: Bool,
) -> List<MergeField> {
    let fields: List<MergeField> = []
    if name {
        fields = [fields, [.name]].flat_map(each -> each)
    }
    if org {
        fields = [fields, [.org]].flat_map(each -> each)
    }
    if role {
        fields = [fields, [.role]].flat_map(each -> each)
    }
    if website {
        fields = [fields, [.website]].flat_map(each -> each)
    }
    if birthday {
        fields = [fields, [.birthday]].flat_map(each -> each)
    }
    if notes {
        fields = [fields, [.notes]].flat_map(each -> each)
    }
    fields
}

/// What two cards say alike: the mailbox or the number that names them as one person.
fn shared_with(c: Contact, other: Contact) -> Text {
    if let mail = c.emails.find(e -> other.emails.any(o -> same_address(e.value, o.value))) {
        return mail.value
    }
    if let phone = c.phones.find(p -> other.phones.any(o -> same_number(p.value, o.value))) {
        return phone.value
    }
    ""
}

/// The groups a contact is in, each a chip that takes them out, and the field that starts
/// another: naming a group nobody made yet makes it a place of the book.
view groups(book: Book, c: Contact) {
    state typed: Text = ""
    stack(gap: .tight) {
        if c.groups.is_empty() {
            text("Not in a group.", role: .caption, secondary: true)
        }
        if not c.groups.is_empty() {
            row(gap: .related, wrap: true) {
                for name in c.groups.sort_by(n -> n.lower()) {
                    ui.Chip(name, icon: .tag) {
                        on remove: ungroup(c, name)
                    }
                }
            }
        }
        ui.TextInput(bind: typed, placeholder: "Add to a group") {
            on submit: group(book, c, typed).then(set(typed, ""))
        }
    }
}

/// The contacts of the workspace as a place another app's page shows them.
view contacts(book: Book) {
    if book.contacts.is_empty() {
        ui.EmptyState("No contacts yet", icon: .users)
    } else {
        std.list(listed(book, .name)) { (c, at) ->
            ui.ListItem(item: c, presence: at) {
                contact_row(c)
            }
        }
    }
}

/// Two people, a week into a project: one colleague, one friend, one group between them.
scenario "a shared address book" {
    let ada = book.contacts.insert(
        Contact(
            name: "Ada Lovelace",
            org: "Analytical Engines",
            role: "Mathematician",
            website: "https://example.org/ada",
            favorite: true,
        ),
    )
    ada.emails.insert(Labeled(label: "work", value: "ada@example.org"))
    ada.phones.insert(Labeled(label: "mobile", value: "+44 20 7946 0018"))
    ada.addresses.insert(
        Address(
            label: "home",
            street: "12 St James's Square",
            city: "London",
            region: "",
            postal: "SW1Y 4JH",
            country: "United Kingdom",
        ),
    )
    ada.groups.add("Work")
    book.groups.insert(Group(name: "Work"))
    let grace = book.contacts.insert(
        Contact(name: "Grace Hopper", org: "US Navy", role: "Rear admiral"),
    )
    grace.emails.insert(Labeled(label: "work", value: "grace@example.org"))
    book.contacts.insert(Contact(name: "Sam Okafor", notes: RichText("Met at the market.")))
}

/// A book nobody has written in yet, as the window opens on it for the first time.
scenario "an empty address book" -> Book {
    book
}

/// One contact on their own, open: the page a person reads of them.
scenario "a contact" -> Contact {
    let ada = book.contacts.insert(
        Contact(
            name: "Ada Lovelace",
            org: "Analytical Engines",
            role: "Mathematician",
            favorite: true,
        ),
    )
    ada.emails.insert(Labeled(label: "work", value: "ada@example.org"))
    ada.phones.insert(Labeled(label: "mobile", value: "+44 20 7946 0018"))
    ada.groups.add("Work")
    book.groups.insert(Group(name: "Work"))
    ada
}
