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
    /// Anything worth remembering about them.
    notes: RichText
    phones: Collection<Labeled>
    emails: Collection<Labeled>
    addresses: Collection<Address>
    /// The groups they are in, by name: a group stands while its name is on someone.
    groups: Set<Text>
    favorite: Bool = false
    /// Their picture, by where its bytes are: a `data:` address a vCard's PHOTO carried, or a
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
    key: "Ctrl+N",
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

/// One contact as vCard 4.0, the text a reader of the format takes.
fn vcard_of(c: Contact) -> Text {
    let lines = ["BEGIN:VCARD", "VERSION:4.0"]
    if not c.uid.is_empty() {
        lines = [lines, ["UID:{vcard_text(c.uid)}"]].flat_map(part -> part)
    }
    if not c.name.trim().is_empty() {
        lines = [lines, ["FN:{vcard_text(c.name)}"]].flat_map(part -> part)
    }
    if not c.org.trim().is_empty() {
        lines = [lines, ["ORG:{vcard_text(c.org)}"]].flat_map(part -> part)
    }
    if not c.role.trim().is_empty() {
        lines = [lines, ["TITLE:{vcard_text(c.role)}"]].flat_map(part -> part)
    }
    if not c.website.trim().is_empty() {
        lines = [lines, ["URL:{vcard_text(c.website)}"]].flat_map(part -> part)
    }
    if let day = c.birthday {
        lines = [lines, ["BDAY:{day.iso()}"]].flat_map(part -> part)
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
    let numbers = c.phones.map(p -> "TEL;TYPE={vcard_type(p.label)}:{vcard_text(p.value)}")
    let mails = c.emails.map(e -> "EMAIL;TYPE={vcard_type(e.label)}:{vcard_text(e.value)}")
    let places = c.addresses.map(
        a -> "ADR;TYPE={vcard_type(a.label)}:;;{vcard_text(a.street)};{vcard_text(a.city)};{vcard_text(a.region)};{vcard_text(a.postal)};{vcard_text(a.country)}",
    )
    let all = [lines, numbers, mails, places, ["END:VCARD"]].flat_map(part -> part)
    all.join("\n")
}

/// A contact as a file: their card, named for them.
action export_contact(c: Contact) -> File (icon: .download, label: "Export as vCard") {
    File.of_text("{file_name(c)}.vcf", vcard_of(c))
}

/// Every contact as one vCard file, which another address book reads whole.
action export_book(book: Book) -> File (icon: .download, label: "Export all as vCard") {
    let cards = book.contacts.sort_by(c -> c.name.lower()).map(c -> vcard_of(c))
    File.of_text("Contacts.vcf", cards.join("\n"))
}

/// Imports the contacts a vCard file holds into the book, each card a contact of its own, and
/// answers how many were added. A file whose cards cannot be read adds nothing and changes
/// nothing: what it says is the refusal.
action import_vcard(book: Book, file: File) -> Int (
    icon: .file_plus,
    label: "Import vCards",
    opens: ["text/vcard", "text/x-vcard", .vcf, .vcard],
    did: "imported contacts",
) {
    require cards_read(file) "The file holds no card this app can read"
    match file.text() {
        .ok(text) => add_contacts(book, text),
        // The require above already refused a file whose words are not here, so this arm answers
        // what nothing imported is.
        .err(_) => 0,
    }
}

/// The cards `cards`, the words of a vCard file, as contacts of this book, and how many named
/// somebody: what an importer, an agent or a script hands a book in one call.
action add_contacts(book: Book, cards: Text) -> Int (
    icon: .plus,
    label: "Add contacts",
    did: "added contacts",
) {
    let pieces = cards.split("BEGIN:VCARD")
    let made: Int = 0
    for card in pieces {
        if add_card(book, card) != .none {
            made = made + 1
        }
    }
    made
}

/// Whether a vCard file's words are here: what an import needs before it reads any card.
fn cards_read(file: File) -> Bool {
    match file.text() {
        .ok(text) => not text.trim().is_empty(),
        .err(_) => false,
    }
}

/// The contact a vCard card says, made in the book, or none for a card that names nobody. Its
/// rows are filled after it stands in the book, as every resource's are.
action add_card(book: Book, card: Text) -> Option<Contact> uses time, random (
    icon: .file_plus,
    label: "Add a vCard's card",
    did: "added a card",
) {
    let lines = card.split("\n").map(line -> line.trim()).filter(line -> not line.is_empty())
    // The name a card carries: the full name it gives, or the parts of its structured name.
    let given = lines.find(line -> starts(line, "FN"))
    let named = or_find(given, lines.find(line -> starts(line, "N")))
    let org = lines.find(line -> starts(line, "ORG"))
    let role = lines.find(line -> starts(line, "TITLE"))
    let site = lines.find(line -> starts(line, "URL"))
    let said = lines.find(line -> starts(line, "NOTE"))
    let kinds = lines.find(line -> starts(line, "CATEGORIES"))
    let picture = lines.find(line -> starts(line, "PHOTO"))
    let born = lines.find(line -> starts(line, "BDAY"))
    let said_id = lines.find(line -> starts(line, "UID"))
    let uid = if let said = said_id { vcard_read(said) } else { random.id() }
    let name = vcard_read(named ?? "")
    let work = vcard_read(org ?? "")
    let numbers = lines.filter(line -> starts(line, "TEL"))
    let mails = lines.filter(line -> starts(line, "EMAIL"))
    // A card that names nobody is not a person: what it holds is the card's format, not someone
    // to keep.
    if name.is_empty() and work.is_empty() and numbers.is_empty() and mails.is_empty() {
        return .none
    }
    // A card the book already holds is nobody new: reading a file again changes nothing.
    if book.contacts.any(c -> c.uid == uid) {
        return .none
    }
    let person = book.contacts.insert(
        Contact(
            name: name,
            org: work,
            role: vcard_read(role ?? ""),
            website: vcard_read(site ?? ""),
            notes: RichText(vcard_read(said ?? "")),
            photo: photo_of(picture, name),
            birthday: birth_of(born, now),
            uid: uid,
        ),
    )
    for line in numbers {
        person.phones.insert(Labeled(label: vcard_kind(line), value: vcard_read(line)))
    }
    for line in mails {
        person.emails.insert(Labeled(label: vcard_kind(line), value: vcard_read(line)))
    }
    for line in lines.filter(line -> starts(line, "ADR")) {
        let parts = vcard_read(line).split(";")
        person.addresses.insert(
            Address(
                label: vcard_kind(line),
                street: parts.at(2) ?? "",
                city: parts.at(3) ?? "",
                region: parts.at(4) ?? "",
                postal: parts.at(5) ?? "",
                country: parts.at(6) ?? "",
            ),
        )
    }
    if let names = kinds {
        for group_name in vcard_read(names).split(",") {
            if not group_name.trim().is_empty() {
                person.groups.add(group_name.trim())
                group(book, person, group_name.trim())
            }
        }
    }
    .some(person)
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

/// The day a card's BDAY names, or none for a card without one or with a day no calendar holds.
fn birth_of(born: Option<Text>, now: Time) -> Option<Date> {
    if let day = born {
        if let parsed = Date.parse(vcard_date(vcard_read(day)), near: now) {
            return .some(parsed)
        }
    }
    .none
}

/// The first of two texts that is here, and none where neither is.
fn or_find(one: Option<Text>, other: Option<Text>) -> Option<Text> {
    if let found = one { found } else { other }
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
fn vcard_type(label: Text) -> Text {
    let kind = label.trim().lower().replace(" ", "-")
    if kind.is_empty() { "voice" } else { kind }
}

/// The value of a vCard line, its name and parameters taken off and its escapes read back.
fn vcard_read(line: Text) -> Text {
    let colon = line.find(":") ?? line.len()
    let raw = if line.find(":") == .none { "" } else { line.slice(colon + 1, line.len()) }
    raw.replace("\\n", "\n").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\").trim()
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
    let bare = parts.filter(part -> not part.contains("="))
    if let kind = bare.last() { kind.trim().lower() } else { "" }
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

/// The contacts in the order the window lists them: by name, ignoring case.
fn listed(book: Book) -> List<Contact> {
    book.contacts.sort_by(c -> c.name.lower())
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
    // A contact a reference, an agent or another app asks for opens where the list stands: the
    // person the window shows is the one it was asked for.
    on show(c: Contact): set(opened, c)
    // The place chosen, named by the group it is where it is one and by the filter where it is
    // favorites: a group's name is the group's, never the field's.
    let favorites = Contact.favorites
    let group = group_place(book, place)
    let title = if let g = group {
        g.name
    } else {
        if place == favorites.query { favorites.label } else { "All contacts" }
    }
    let all = listed(book)
    let here = if place.is_empty() { all } else { all.matching(place) }
    let found = if searched.is_empty() { here } else { here.matching(searched) }
    ui.NavShell(
        content_kind: .document,
        sidebar_title: "Contacts",
        overview_title: title,
        overview_count: found.len(),
    ) {
        on search(q: Text): set(searched, q)
        sidebar: {
            ui.NavList(selected: if place.is_empty() { "all" } else { place }) {
                ui.NavItem("all", "All contacts", icon: .users) {
                    on select: set(place, "").then(set(opened, .none))
                }
                ui.NavItem(favorites.query, favorites.label, icon: .star) {
                    on select: set(place, favorites.query).then(set(opened, .none))
                }
                for g in book.groups.sort_by(g -> g.name.lower()) {
                    ui.NavItem(Contact.groups(g.name).query, g.name, icon: .tag) {
                        on select: set(place, Contact.groups(g.name).query).then(set(opened, .none))
                    }
                }
            }
        }
        sidebar_actions: {
            ui.MenuButton("File", icon: .file_plus) {
                ui.MenuItem("Import vCards…", icon: .file_plus) {
                    on choose: file.choose(
                        import_vcard(book),
                        accept: ["text/vcard", "text/x-vcard"],
                    )
                }
                ui.MenuItem("Export all contacts…", icon: .download) {
                    on choose: file.save(export_book(book), name: "Contacts.vcf")
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
                if found.is_empty() {
                    ui.EmptyState(
                        if all.is_empty() { "No contacts yet" } else { "Nobody here" },
                        icon: .users,
                        inline: true,
                    )
                } else {
                    // The cards and, beside them, the letters they stand under: a press brings
                    // the first card under its letter up, at the list's own motion.
                    // The list is the column's scroller: the stack takes the room the foot leaves,
                    // the row spans it, and the rail stands beside the list.
                    stack(fill, gap: .none) {
                        row(fill, align: .stretch, gap: .none) {
                            // A press asks for the first card under its letter; the list brings it up,
                            // and brings it up again when the person has scrolled away since.
                            std.list(
                                found,
                                opened,
                                ungrouped: .some("Other"),
                                reveal: if revealing.is_empty() {
                                    .none
                                } else { .some(revealing) },
                            ) { (c, at) ->
                                ui.ListItem(item: c, presence: at, group: first_letter(c.name)) {
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
                        ui.Button("Export this contact", icon: .download) {
                            on press: export_contact(c)
                        }
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
            row(gap: .tight, align: .center) {
                ui.TextInput(bind: p.label, placeholder: "Home", min: 6rem, max: 10rem)
                ui.TextInput(bind: p.value, placeholder: "Phone number", fill)
                ui.Button("Remove phone", icon: .x, icon_only: true, role: .plain) {
                    on press: remove_phone(c, p)
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
            row(gap: .tight, align: .center) {
                ui.TextInput(bind: e.label, placeholder: "Work", min: 6rem, max: 10rem)
                ui.TextInput(bind: e.value, placeholder: "Email address", fill)
                ui.Button("Remove email", icon: .x, icon_only: true, role: .plain) {
                    on press: remove_email(c, e)
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
                row(gap: .related, wrap: true) {
                    toggle(bind: take_name, label: "Take their name")
                    toggle(bind: take_org, label: "Take their organization")
                    toggle(bind: take_role, label: "Take their job title")
                    toggle(bind: take_website, label: "Take their website")
                    toggle(bind: take_birthday, label: "Take their birthday")
                    toggle(bind: take_notes, label: "Take their notes")
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
        std.list(listed(book)) { (c, at) ->
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
