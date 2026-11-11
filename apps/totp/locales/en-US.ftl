# Authenticator — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

## Add-account sheet: its title per flow
totp-add-title-add = Add account
totp-add-title-edit = Edit account
totp-add-title-provision = Set up on this device

## Ways to import an account
totp-add-scan-title = Scan a QR image
totp-add-scan-description = Read the code from a screenshot or photo
totp-scan-unreadable = Couldn’t read a code from that image
totp-add-link-title = Paste a setup link
totp-add-link-description = Fills in the service, account and key
totp-add-link-label = Setup link

## Manual fields
totp-add-details = Or enter details
totp-add-placeholder-issuer = Service name
totp-add-placeholder-account = Email or username
totp-add-placeholder-secret = Key from the service
totp-add-label-issuer = Issuer
totp-add-label-account = Account name
totp-add-label-secret = Secret key
totp-add-advanced = Advanced options
totp-add-label-algorithm = Algorithm
totp-add-label-digits = Digits
totp-add-label-period = Period in seconds
totp-add-invalid-period = Enter a whole number of seconds between 5 and 300

## Footer
totp-add-cancel = Cancel
totp-add-save-add = Add account
totp-add-save-edit = Save changes
totp-add-save-provision = Set up

## History

totp-stored-account-issuer = Renamed the account { $item }
totp-stored-account-label = Changed the account name of { $item }
totp-stored-account-color = Recoloured { $item }
totp-stored-account-algorithm = Changed how { $item } computes its codes
totp-stored-account-digits = { $item } now shows { $new }-digit codes
totp-stored-account-period = { $item } now changes its code every { $new } seconds
totp-totp-accounts = { $op ->
    [insert] Added an account
   *[other] Changed the accounts
}
totp-move-up = Moved { $item } up
totp-move-down = Moved { $item } down
totp-delete = Deleted the account { $item }

## What the window and its account list are called for assistive technology

totp-a11y-app = Authenticator
totp-a11y-key-status = Key status
totp-a11y-retry-keys = Retry saving keys
totp-a11y-hide-code = Hide code for { $name }
totp-a11y-reveal-code = Reveal code for { $name }
totp-a11y-codes-hidden = Codes are hidden until you ask to see one; they stay readable for a few seconds.
totp-a11y-copy-code = Copy code for { $name }
totp-a11y-copy-description = Copies the current one-time code to the clipboard
totp-a11y-set-up-first = Set up this account's key on this device before copying a code
totp-a11y-edit = Edit { $name }
totp-a11y-move-up = Move { $name } up
totp-a11y-move-down = Move { $name } down
totp-a11y-delete = Delete { $name }
totp-a11y-set-up = Set up { $name } on this device
totp-a11y-set-up-description = Paste or scan the key without sharing it with collaborators
totp-a11y-account = Authenticator account
totp-a11y-accounts = Authenticator accounts
totp-a11y-none = No accounts
totp-a11y-accounts-hint = { $total } accounts. Arrow keys change the selected account.
totp-empty-message = Scan a QR code or paste a setup link to see its one-time codes here.

## A shared account whose key is not on this device
totp-a11y-unkeyed = Not set up on this device

## Actions

totp-effect-open-account = Open account
totp-effect-copy-code = Copy code
totp-effect-retry-save = Save again
totp-vault = Authenticator
totp-vault-move-up = Move up
totp-vault-move-down = Move down
totp-vault-delete = Delete
