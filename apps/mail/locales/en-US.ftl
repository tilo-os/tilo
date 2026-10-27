# Mail — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

## Compose window
mail-compose-label-to = To
mail-compose-label-cc = Cc
mail-compose-label-bcc = Bcc
mail-compose-label-subject = Subject
mail-compose-placeholder-to = Name or email
mail-compose-cc-bcc = Cc/Bcc
mail-compose-send = Send
mail-compose-save-draft = Save draft
mail-compose-discard = Discard
# The link prompt above the buttons.
mail-compose-prompt-link = Link
mail-compose-prompt-add = Add link
mail-compose-prompt-cancel = Cancel

# Shared address-book choices. Hints are selected explicitly, never identity proof.
mail-contact-recipients = Contacts
mail-contact-write = Write to a contact
mail-contact-draft-choice = Draft to { $person } ({ $id }) at { $email } from { $sender } ({ $sender_account })
mail-contact-draft-prepared = Draft ready
mail-contact-sender-changed = Sending account changed
mail-contact-prepare = Prepare draft
mail-contact-existing-draft = A draft is already open
mail-contact-unavailable = Shared Contacts is unavailable
mail-contact-recipient-changed = Contact changed; select and review it again
mail-contact-account-changed = Sending account changed; select and review it again

## History: what each change did, by field and by verb

mail-mark-read = Marked messages { $read ->
    [true] read
   *[false] unread
}
mail-archive = Archived messages
mail-trash = Moved messages to Trash
mail-move-messages = Moved messages to { $to }
mail-purge = Deleted messages for good
mail-ingest = Fetched mail
mail-server-applied = The mail server took a change
mail-keep-copies = Kept fetched mail for your other devices
mail-forget-copies = Dropped mail its server no longer holds
mail-mail-copies-copies = Changed the mail kept for your other devices
mail-start-draft-body = Started writing a message
mail-discard-draft = Discarded the draft
mail-send-message = Sent a message from { $account }
mail-cancel-send = Cancelled sending a message
mail-message-state-starred = { $new ->
    [true] Starred a message
   *[false] Unstarred a message
}
mail-message-state-folder = Moved a message to { $new }
mail-message-state-labels = Changed the labels of a message
mail-local-folder-name = Renamed the folder { $item }
mail-mail-folders = Changed the folders
mail-mail-messages = Changed the mailbox
mail-mail-outbox = Recorded how a sent message went
mail-draft-state-sender = Chose who the draft is sent from
mail-draft-state-to = Changed who the draft goes to
mail-draft-state-cc = Changed who the draft is copied to
mail-draft-state-bcc = Changed who the draft is blind copied to
mail-draft-state-subject = Changed the subject of the draft
mail-draft-state-body = Edited the draft
mail-draft-state-html-body = Edited the draft
mail-draft-state-calendar-ics = Changed the invitation in the draft
mail-draft-state-attachments = Changed the attachments of the draft
mail-draft-state-rich-body = Edited the draft
mail-save-draft = Saved the draft
mail-attach = Attached { $name } to the draft
mail-mail-server = Recorded how a mail server took a change
mail-mail-held = Changed the mail held in the mailbox
mail-held-message-message-id = Changed the Message-ID of a held message
mail-held-message-from = Changed the sender of a held message
mail-held-message-to = Changed the recipients of a held message
mail-held-message-cc = Changed who a held message is copied to
mail-held-message-subject = Changed the subject of a held message
mail-held-message-body = Changed the text of a held message
mail-held-message-minutes = Changed when a held message was sent
mail-held-message-in-reply-to = Changed the message a held message answers
mail-held-message-conversation = Moved a held message to another conversation
mail-deliver-message = Delivered { $subject }
mail-draft-message = Drafted { $subject }
mail-draft-reply = Drafted a reply

mail-forward-not-downloaded = { $count ->
    [one] { $files } is not downloaded yet, so it is not attached
   *[other] { $files } are not downloaded yet, so they are not attached
}
mail-forward-too-large = { $count ->
    [one] { $files } is more than one message can carry, so it is not attached
   *[other] { $files } are more than one message can carry, so they are not attached
}

mail-import-sign-in-required = Imported account settings require authentication. Sign in before connecting.

## Reader: an HTML message's remote content, links and format
mail-remote-blocked = Remote images aren’t loaded
mail-remote-load = Load images
mail-remote-always = Always for this sender
mail-remote-loaded = Remote images are loaded for this message
mail-remote-sender-loads = Remote images from this sender always load
mail-remote-stop = Stop loading them
mail-link-open = Open link
mail-link-copy = Copy link address
mail-copy = Copy
mail-select-all = Select all
mail-show-quoted = Show quoted text
mail-show-plain = Show plain text
mail-show-formatted = Show formatted
