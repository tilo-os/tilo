contacts-accounts = Account associations
contacts-accounts-unavailable = Account associations are unavailable in this session.
contacts-account-person = Contact
contacts-account-service = Service
contacts-account-recipient = Account address or identifier
contacts-account-add = Review association
contacts-account-remove = Remove association
contacts-account-review = Review account association
contacts-account-cancel = Cancel
contacts-account-confirm = Associate account
contacts-account-invalid = Enter a valid account identifier for the selected service.
contacts-account-stale = This contact or account changed. Open the account editor and review it again.
contacts-account-conflict = This account already has an association. Review and remove the old association first.
contacts-account-unverified = Not confirmed
contacts-account-linked = Associated
contacts-account-conflicted = Needs review
contacts-account-hint = A matching phone number or name does not confirm an account.
contacts-account-confirm-hint = Tilo does not check that the account is really theirs. Associating it shares no files and grants no permissions.
contacts-account-revoke-hint = Messages from this account stop reaching this contact. Messages already received are unchanged.
contacts-account-reference = Identity reference
contacts-account-old-reference = Previous identity reference
contacts-account-edit = Edit association
contacts-account-edited-unverified = The changed address needs review before messages are sent to it.

contacts-account-scope = Service account
contacts-account-review-description = { $action } for { $person }: { $recipient } at { $service }

## History: what each change to the address book did

contacts-new-contact = Added { $name }
contacts-add-contacts = Added to the address book
contacts-delete-contact = Deleted { $item }
contacts-merge-contacts = Merged a duplicate into { $item }
contacts-contact-first = Changed the first name of { $item }
contacts-contact-last = Changed the last name of { $item }
contacts-contact-org = Changed the organization of { $item }
contacts-contact-title = Changed the title of { $item }
contacts-contact-website = Changed the website of { $item }
contacts-contact-birthday = Changed the birthday of { $item }
contacts-contact-notes = Changed the notes on { $item }
contacts-contact-favorite = { $new ->
    [true] Made { $item } a favorite
   *[false] Took { $item } off the favorites
}
contacts-contact-phones = { $op ->
    [insert] Added a phone number to { $item }
    [remove] Removed a phone number from { $item }
   *[other] Reordered the phone numbers of { $item }
}
contacts-contact-emails = { $op ->
    [insert] Added an email address to { $item }
    [remove] Removed an email address from { $item }
   *[other] Reordered the email addresses of { $item }
}
contacts-contact-addresses = { $op ->
    [insert] Added a postal address to { $item }
    [remove] Removed a postal address from { $item }
   *[other] Reordered the postal addresses of { $item }
}
contacts-contact-groups = { $op ->
    [insert] Added { $item } to a group
    [remove] Took { $item } out of a group
   *[other] Reordered the groups of { $item }
}
contacts-labeled-label = Changed the type of { $item }
contacts-labeled-value = Changed { $item }
contacts-address-label = Changed the type of an address
contacts-address-street = Changed the street of an address
contacts-address-city = Changed the city of an address
contacts-address-region = Changed the region of an address
contacts-address-postal = Changed the postal code of an address
contacts-address-country = Changed the country of an address
contacts-group-entry-value = Renamed the group { $item }

## What the app calls itself and its list

contacts-title = Contacts

## Window and accessibility copy
contacts-a11y-groups = Groups
contacts-a11y-sort = Sort contacts
contacts-a11y-export-list = Contacts to export
contacts-a11y-birthday-calendar = Birthday calendar
contacts-a11y-favorite = Favorite

## Facts, property labels and section titles
contacts-other-card = Other card
contacts-this-card = This card
contacts-keep-field = { $field } to keep
contacts-notes = Notes
contacts-website = Website
contacts-birthday = Birthday

contacts-keep-this-card = Keep this card's
contacts-keep-other-card = Keep the other card's
contacts-importing = Importing contacts
contacts-exporting = Exporting contacts

## What the window shows
contacts-changes-lost = Your changes are lost if you don't save them.
contacts-import-same-identity = The imported card has the same identity as { $existing } in your book, but different details.
contacts-cards-share = These cards share { $facts }. This card stays, with every phone number, email, address and group of both.
contacts-select-all = Select all
contacts-select-none = Select none
contacts-remove-field = Remove { $field }
contacts-add-phone = Add phone
contacts-add-email = Add email
contacts-add-address = Add address
contacts-hide-calendar = Hide calendar
contacts-choose-from-calendar = Choose from calendar
contacts-fact-email = an email
contacts-fact-phone = a phone number
contacts-fact-name = a name
contacts-facts-details = their details
contacts-facts-and = { $first } and { $last }

## What a form field is called
contacts-field-first-name = First name
contacts-field-last-name = Last name
contacts-field-organization = Organization
contacts-field-job-title = Job title
contacts-field-website = Website
contacts-field-birthday = Birthday
contacts-field-notes = Notes
contacts-field-groups = Groups
contacts-field-phone = Phone { $number }
contacts-field-email = Email { $number }
contacts-field-street = Address { $number } street
contacts-field-city = Address { $number } city
contacts-field-region = Address { $number } region
contacts-field-postal-code = Address { $number } postal code
contacts-field-country = Address { $number } country

contacts-unnamed = Unnamed contact

## The window bar's slots
contacts-new-contact-title = New contact
contacts-all-contacts = All contacts
contacts-save = Save
contacts-call = Call
contacts-text = Text
contacts-email = Email
contacts-edit-contact = Edit contact
contacts-remove-contact = Delete contact
contacts-bar-importing = Importing…
contacts-import = Import contacts
contacts-export = Export contacts
contacts-review-duplicate = Review possible duplicate

## A book someone may only read
contacts-view-only-message = You can view this address book, but not add to it.

## Actions

contacts-accounts-open-accounts = Manage connected accounts
contacts-accounts-close-accounts = Return to contacts
contacts-accounts-select-account-person = Choose contact
contacts-accounts-review-account = Review connected account
contacts-accounts-edit-account = Edit connected account
contacts-accounts-review-account-revocation = Review account removal
contacts-accounts-confirm-account-review = Confirm account change
contacts-accounts-cancel-account-review = Cancel account change
contacts-effect-open-group = Open group
contacts-effect-open-favorites = Open favorites
contacts-effect-sort-contacts = Sort contacts
contacts-effect-open-review = Review possible duplicate
contacts-effect-edit-field = Edit field
contacts-effect-open-website = Open website
contacts-effect-read-duplicate = Read possible duplicate
contacts-effect-import-vcard = Import vCard
contacts-effect-export-all-vcard = Export all contacts
contacts-effect-export-contact = Export contact
contacts-effect-export-vcard-content = Export vCard content
contacts-effect-keep-existing = Keep existing contact
contacts-effect-import-as-new = Import as new contact
contacts-book = Contacts
contacts-book-new-contact = New contact
contacts-book-add-contacts = Add contacts
contacts-book-delete-contact = Delete contact
contacts-book-merge-contacts = Merge contacts
