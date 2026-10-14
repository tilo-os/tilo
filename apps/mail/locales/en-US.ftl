# Mail — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

## Account sheet
mail-setup-heading-add = Add account
mail-setup-heading-edit = Account settings
mail-setup-label-display-name = Name
mail-setup-label-email = Email
mail-setup-label-username = Username
mail-setup-label-password = Password
mail-setup-label-imap-host = Incoming server
mail-setup-label-smtp-host = Outgoing server
mail-setup-label-sign-in = Sign in with
mail-setup-label-account = Account
mail-setup-auth-password = Password
mail-setup-auth-oauth = Browser
# { $provider } is the provider's own name (Gmail, Outlook, …).
mail-setup-oauth-sign-in = Sign in with { $provider }
mail-setup-oauth-provider-fallback = your provider
mail-setup-oauth-note-unavailable = Browser sign-in unavailable
mail-setup-tls = TLS
mail-setup-advanced = Advanced
mail-setup-save = Save
mail-setup-verifying = Checking…
mail-setup-finding-provider = Finding servers…
mail-setup-cancel = Cancel
mail-setup-remove = Remove account

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
mail-contact-recipients = Contacts recipients
mail-contact-draft-choice = Draft to { $person } ({ $id }) at { $email } from { $sender } ({ $sender_account })
mail-contact-draft-prepared = Draft ready
mail-contact-sender-changed = Sending account changed
mail-contact-prepare = Prepare draft
mail-contact-existing-draft = A draft is already open
mail-contact-unavailable = Shared Contacts is unavailable
mail-contact-person-unavailable = Contact is no longer available
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
mail-mail-server = Recorded how a mail server took a change

mail-import-sign-in-required = Imported account settings require authentication. Sign in before connecting.
