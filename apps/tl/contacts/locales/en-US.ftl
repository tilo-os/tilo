# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-contacts = Contacts

d-so-tilo-contacts-keywords = contacts, address book, people, phone, email, vcard

d-g4vf2are = Labeled
    .doc = One way to reach someone, and what kind it is: a phone number that is theirs at home, a mailbox that is their work's.
    .noun = { $count ->
        [one] labeled
       *[other] labeleds
    }

d-fzyd2a52 = Label
    .doc = What kind of number or address it is: home, work, mobile.

d-b4cilqkx = Value

d-cxjt3f6x = Address
    .doc = Where a person's post goes, written the way post is addressed here.
    .noun = { $count ->
        [one] address
       *[other] addresses
    }

d-5iipld72 = Label
    .doc = What kind of place it is: home, work.

d-zpxsouby = Street

d-txsphfy2 = City

d-g55zwvsh = Region

d-xuayok2l = Postal

d-xlbwdlve = Country

d-w5adcbsy = Contact
    .doc = Someone the workspace keeps in touch with.
    .noun = { $count ->
        [one] contact
       *[other] contacts
    }
    .untitled = No name yet

d-zut4kvim = Name

d-rfhxkccf = Org
    .doc = Where they work.

d-7wq5grnt = Role
    .doc = What they do there.

d-dg7b4wfh = Website

d-fc62qk5v = Birthday
    .doc = The day they were born; the year may be their birth year or left out of it.

d-4lfazbdq = Notes
    .doc = Anything worth remembering about them.

d-lurugxra = Phones

d-hfbs4gxe = Emails

d-orxuftuv = Addresses

d-vea52w55 = Groups
    .doc = The groups they are in, by name: a group stands while its name is on someone.

d-ur3qshor = Favorite

d-kvhikhu4 = Photo
    .doc = Their picture, by where its bytes are: a `data:` address a vCard's PHOTO carried, or a content address someone put here.

d-szrijd7k = Person
    .doc = The id of the person the workspace's directory holds for this card, once it is shared with the apps; empty until then, or after it stops being shared.

d-q3um3t7j = Uid
    .doc = The card's own id in the format that carried it here (a vCard's `UID`): what reading the same file again finds it by, so a book is imported once however often its file is read.

d-w5adcbsy-filter-favorites = Favorites

d-w5adcbsy-filter-groups = Groups

d-qeo6ddcm = Group
    .doc = A group the book offers as a place: the people in it, gathered by a word they share.
    .noun = { $count ->
        [one] group
       *[other] groups
    }

d-jn6l7z6v = Name

d-etcwj7o5 = Book
    .doc = The workspace's address book.
    .noun = { $count ->
        [one] book
       *[other] books
    }

d-qfj7l6eu = Contacts

d-uyyz5pg2 = Groups

d-glpbyz4c = Merge field
    .doc = What a merge may take from the card that goes: the values each card keeps one of.

d-ftc5rtae = Their name

d-44vcd3nc = Where they work

d-flwktecf = What they do there

d-foct2e7s = Their website

d-feye5m7e = The day they were born

d-5m5rhfwk = What is written about them

d-smciqs6j = New contact
    .doc = A new contact, named by what is typed where they were made; a book's entry gives the name it takes, so nothing is left called "No name yet" by a person who typed one.
    .did = added a contact

d-smciqs6j-name = Name

d-bfuqzfia = Find or add a contact
    .doc = Finds the contact the workspace keeps under `name`, or adds one: what brings someone in from another service — a task given to them before they joined — to one card, however many imports name them.
    .did = found a contact
    .w-name-the-contact = Name the contact

d-bfuqzfia-name = Name

d-zwp7xkyi = Rename
    .doc = Renames a contact.
    .did = renamed a contact

d-zwp7xkyi-name = Name

d-kp6evyhz = Add to favorites
    .doc = Adds a contact to favorites, or takes them off.
    .w-add-to-favorites = Add to favorites
    .w-remove-from-favorites = Remove from favorites

d-kp6evyhz-on = On

d-2ctxgtyd = Add phone
    .doc = Gives a contact a phone number, as the number `number` and the kind `label` where the caller knows them already.
    .did = added a phone number

d-2ctxgtyd-number = Number

d-2ctxgtyd-label = Label

d-spe2pxo4 = Remove phone
    .doc = Takes one of a contact's phone numbers away.

d-spe2pxo4-phone = Phone

d-ts6f5mgs = Add email
    .doc = Gives a contact a mailbox, as the `address` and the kind `label` where the caller knows them already.
    .did = added an email address

d-ts6f5mgs-address = Address

d-ts6f5mgs-label = Label

d-infpb6bb = Remove email
    .doc = Takes one of a contact's mailboxes away.

d-infpb6bb-mail = Mail

d-qgoswgkz = Add address
    .doc = Gives a contact a postal address to fill in.
    .did = added an address

d-xr3q4wsk = Remove address
    .doc = Takes one of a contact's addresses away.

d-xr3q4wsk-address = Address

d-pa7wwp6r = Add to group
    .doc = Puts a contact in a group, making the group the book's place if it is new: the same group however it is spelled, so "Work" and "work" are one place.
    .w-a-group-needs-a-name = A group needs a name

d-pa7wwp6r-c = C

d-pa7wwp6r-name = Name

d-qafligvf = Remove from group
    .doc = Takes a contact out of a group.

d-qafligvf-name = Name

d-vi6ajqma = New group
    .doc = Starts a group the book offers as a place, with nobody in it yet.
    .w-a-group-needs-a-name = A group needs a name

d-vi6ajqma-name = Name

d-ivtvy7sy = Delete
    .doc = Deletes a contact from the book, and with it the person the workspace's apps know of it where the window hands the directory: what Mail, Calendar and Messages reach it by goes too.
    .did = deleted a contact

d-ivtvy7sy-c = C

d-ivtvy7sy-directory = Directory

d-76ulubho = Merge contacts
    .doc = Merges the card that goes into the one that stays: every number, mailbox, address and group of both is the card that stays, the fields `take` names come from the card that goes, and the card that goes is deleted.
    .did = merged a duplicate
    .w-a-contact-cannot-be-merged-into = A contact cannot be merged into itself

d-76ulubho-keep = Keep

d-76ulubho-goes = Goes

d-76ulubho-take = Take

d-76ulubho-directory = Directory

d-4jncxnhw = Export as vCard
    .doc = A contact as a file: their card, named for them.

d-5ceccmwk = Export all as vCard
    .doc = Every contact as one vCard file, which another address book reads whole.

d-btp4fqga = Import vCards
    .doc = Imports the contacts a vCard file holds into the book, each card a contact of its own, and answers how many were added.
    .did = imported contacts
    .w-the-file-holds-no-card-this = The file holds no card this app can read

d-btp4fqga-file = File

d-lcnj23sa = Add contacts
    .doc = The cards `cards`, the words of a vCard file, as contacts of this book, and how many named somebody: what an importer, an agent or a script hands a book in one call.
    .did = added contacts

d-lcnj23sa-cards = Cards

d-l64bjt6g = Add a vCard's card
    .doc = The contact a vCard card says, made in the book, or none for a card that names nobody.
    .did = added a card

d-l64bjt6g-card = Card

d-xdqrpoil = Find the contact for an address
    .doc = The contact the book holds for `address`, as an action: what a person, an agent or another app asks before they write to it.

d-xdqrpoil-address = Address

d-jz6v4veg = Associate account
    .doc = Binds `account` of `service` to the person: the directory's `confirm-endpoint`, or `confirm-account` where the service names an account scope.
    .did = associated an account
    .w-a-service-names-the-account-s = A service names the account's kind
    .w-an-account-address-or-identifier-is = An account address or identifier is needed

d-jz6v4veg-d = D

d-jz6v4veg-person = Person

d-jz6v4veg-service = Service

d-jz6v4veg-scope = Scope

d-jz6v4veg-account = Account

d-o2qwok2w = Change association
    .doc = Changes the account an association names, under the directory's rule that a changed address needs review before anything is sent to it.
    .did = changed an association
    .w-an-account-address-or-identifier-is = An account address or identifier is needed

d-o2qwok2w-d = D

d-o2qwok2w-person = Person

d-o2qwok2w-endpoint = Endpoint

d-o2qwok2w-service = Service

d-o2qwok2w-scope = Scope

d-o2qwok2w-account = Account

d-rlngk4rk = Remove association
    .doc = Takes an association away: messages from that account stop reaching this contact, and what was already received is unchanged.
    .did = removed an association

d-rlngk4rk-d = D

d-rlngk4rk-person = Person

d-rlngk4rk-endpoint = Endpoint

d-nhasu5my = Share this card
    .doc = Shares this card with the workspace's apps, under the person it is: the name, the numbers and the mailboxes another app reads of a contact before it writes to them.
    .did = shared a card

d-nhasu5my-d = D

d-nhasu5my-person = Person

d-nhasu5my-c = C

d-4oatdkw2 = Share this card
    .doc = Shares a card the workspace's apps do not know yet: a person of its own is made for it, so Mail, Calendar and Messages can name it and find it by its numbers and mailboxes.
    .did = shared a card

d-4oatdkw2-d = D

d-4oatdkw2-c = C

d-kqf6yzpo = Stop sharing this card
    .doc = Takes the contact out of the directory, leaving its card in the book: what the apps read of it stops naming it, and the card is no person of the directory's until it is shared again.
    .did = stopped sharing a card

d-kqf6yzpo-d = D

d-kqf6yzpo-person = Person

d-kqf6yzpo-c = C

d-zfqz2htt = Named or new~present

d-ts4v4mxm = Favorite~present

d-oqpua326 = Group~present

d-dysygpdo = New group~present

d-qwoobxzv = Merge into~present

d-dd4gcdnc = Import vcard~present

d-6o6vmpkz = Book window
    .w-all-contacts = All contacts
    .w-contacts = Contacts
    .w-file = File
    .w-import-vcards = Import vCards…
    .w-export-all-contacts = Export all contacts…
    .w-new-contact = New contact
    .w-no-contacts-yet = No contacts yet
    .w-nobody-here = Nobody here
    .w-other = Other

d-dfmc6gmk = Contact row

d-nhhj3wnt = Contact page
    .w-name = Name
    .w-favorite = Favorite
    .w-organization = Organization
    .w-where-they-work = Where they work
    .w-job-title = Job title
    .w-what-they-do-there = What they do there
    .w-website = Website
    .w-https = https://
    .w-birthday = Birthday
    .w-phones = Phones
    .w-email = Email
    .w-addresses = Addresses
    .w-groups = Groups
    .w-connected-accounts = Connected accounts
    .w-possibly-the-same-person = Possibly the same person
    .w-notes = Notes
    .w-anything-worth-remembering-about-them = Anything worth remembering about them
    .w-export-this-contact = Export this contact
    .w-delete-this-contact = Delete this contact

d-6ltzv265 = Phones
    .w-no-numbers-yet = No numbers yet.
    .w-home = Home
    .w-phone-number = Phone number
    .w-remove-phone = Remove phone
    .w-add-phone = Add phone

d-qkxolihp = Emails
    .w-no-mailboxes-yet = No mailboxes yet.
    .w-work = Work
    .w-email-address = Email address
    .w-remove-email = Remove email
    .w-add-email = Add email

d-5dnmy6vv = Addresses
    .w-no-addresses-yet = No addresses yet.
    .w-home = Home
    .w-street = Street
    .w-remove-address = Remove address
    .w-city = City
    .w-region = Region
    .w-postal-code = Postal code
    .w-country = Country
    .w-add-address = Add address

d-znqezbnm = Duplicates
    .w-merge-into-this-card = Merge into this card
    .w-they-share = They share { $shared_with }.
    .w-take-their-name = Take their name
    .w-take-their-organization = Take their organization
    .w-take-their-job-title = Take their job title
    .w-take-their-website = Take their website
    .w-take-their-birthday = Take their birthday
    .w-take-their-notes = Take their notes

d-425344oj = Groups
    .w-not-in-a-group = Not in a group.
    .w-add-to-a-group = Add to a group

d-7ka3b3ve = Contacts
    .w-no-contacts-yet = No contacts yet

d-4mv5jww4 = Associations
    .w-no-account-of-the-workspace-is = No account of the workspace is this contact yet.
    .w-associated = Associated
    .w-needs-review = Needs review
    .w-service-account = Service account { $scope }
    .w-change = Change
    .w-remove-association = Remove association
    .w-service-such-as-messages = Service, such as Messages
    .w-service-account-where-the-service-names = Service account, where the service names one
    .w-account-address-or-identifier = Account address or identifier
    .w-keep-this-account = Keep this account
    .w-cancel = Cancel
    .w-share-this-card-with-the-apps = Share this card with the apps
    .w-stop-sharing-this-card = Stop sharing this card
    .w-the-workspace-s-apps-do-not = The workspace's apps do not know this contact yet. Sharing the card lets them name it; an account is associated by reviewing it.

d-wio6wz72 = Review
    .w-service-such-as-messages = Service, such as Messages
    .w-service-account-where-the-service-names = Service account, where the service names one
    .w-account-address-or-identifier = Account address or identifier
    .w-review-this-account = Review this account

d-doi7lzy6 = Consent
    .w-tilo-does-not-check-that-the = Tilo does not check that the account is really theirs. Associating it shares no files and grants no permissions.

d-6kvgemtj = Confirm~present

d-bhmovs5j = Change~present
