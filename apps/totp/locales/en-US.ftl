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
totp-add-link-title = Paste a setup link
totp-add-link-description = Fill everything from an otpauth link
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
