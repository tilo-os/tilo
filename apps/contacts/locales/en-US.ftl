contacts-accounts = Account associations
contacts-accounts-unavailable = Account associations are unavailable in this session.
contacts-account-person = Contact
contacts-account-service = Service
contacts-account-recipient = Account address or identifier
contacts-account-add = Review association
contacts-account-close = Close accounts
contacts-account-remove = Remove association
contacts-account-review = Review account association
contacts-account-cancel = Cancel
contacts-account-confirm = Confirm
contacts-account-invalid = Enter a valid account identifier for the selected service.
contacts-account-stale = This contact or account changed. Open the account editor and review it again.
contacts-account-conflict = This account already has an association. Review and remove the old association first.
contacts-account-unverified = Identity reference unavailable
contacts-account-linked = Associated
contacts-account-conflicted = Needs review
contacts-account-hint = Add an address to this contact. A phone or name match never confirms an identity.
contacts-account-confirm-hint = Confirm that this account belongs to the selected contact. This does not verify ownership, share files, or grant permissions.
contacts-account-revoke-hint = Remove this association from future routing. Existing message attribution stays unchanged.
contacts-account-reference = Identity reference
contacts-account-old-reference = Previous identity reference
contacts-account-edit = Edit association
contacts-account-edited-unverified = Changing this address clears its old identity reference. Sending stays unavailable until the new account identity is reviewed.

contacts-account-scope = Service account
contacts-account-review-description = { $action }: Contact { $person }. Service { $service }. Account { $recipient }. Service account { $scope }. Identity reference { $revision }. Previous identity reference { $previous }.

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
