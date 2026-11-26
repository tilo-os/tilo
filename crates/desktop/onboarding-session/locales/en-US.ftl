## Setup

onboarding-resource = Setup
onboarding-back = Back
onboarding-cancel = Cancel
onboarding-continue = Continue
onboarding-recovery-key-different = These words belong to a different identity
onboarding-picker-import-title = Choose a Tilo identity
onboarding-picker-import-filter = Tilo identity
onboarding-nearby-ticket-empty = Type the code your other device printed.
onboarding-picker-backup-title = Save encrypted identity backup
onboarding-picker-backup-filter = Encrypted Tilo identity
onboarding-finish = Continue to desktop

## What setup says went wrong

# Adding this computer with a code: what failed and what to do next
onboarding-nearby-failed-title = This computer was not added
onboarding-nearby-unknown = Try again, or make a new code with tilo device add on your other device.
onboarding-nearby-detail = Details: { $detail }
onboarding-nearby-wrong-title = That code is not one this computer can use
onboarding-nearby-wrong = Copy the whole line tilo device add printed on your other device, starting with tilo1.
onboarding-nearby-used-title = That code was already used
onboarding-nearby-used = Each code works once. Make a new one with tilo device add on your other device.
onboarding-nearby-expired-title = That code has run out
onboarding-nearby-expired = A code works for ten minutes. Make a new one with tilo device add on your other device.
onboarding-nearby-unreachable-title = Your other device could not be reached
onboarding-nearby-unreachable = Check that it is on and connected, then try again. If its tilo device add ended, make a new code.
onboarding-nearby-storage-title = This computer could not save itself as a device
onboarding-nearby-storage = This computer's device certificate could not be written. Free up space or fix its identity folder's permissions, then try again.
onboarding-nearby-not-started-title = Tilo's background sync did not start
onboarding-nearby-not-started = This computer joins through it. Try again; if it keeps failing, run tilo status in a terminal.
onboarding-nearby-unavailable-title = Adding a computer is unavailable here
onboarding-nearby-unavailable = Adding this computer with a code is unavailable in this session
onboarding-nearby-cancelled = Adding this computer stopped. Choose Join to try again.

# Setup on this computer: what failed and what to do next
onboarding-progress-unreadable = The saved setup position could not be read ({ $error }). Your identity is safe; setup resumed at the completion step.
onboarding-progress-invalid-identity = Setup found unfinished progress but could not validate the existing identity: { $error }
onboarding-progress-save-failed = Setup could not save its restart position: { $error }. No identity action was repeated.
onboarding-progress-clear-failed = Setup finished, but its restart marker could not be removed: { $error }. Try Finish again so this screen does not return next login.
onboarding-recovery-key-count = A recovery key has 24 words; this has { $found }.
onboarding-recovery-key-unknown-word = Word { $position } isn't a recovery-key word. Check its spelling.
onboarding-recovery-key-typo = Word { $position }, “{ $word }”, isn't a recovery-key word. Check its spelling.
onboarding-recovery-key-suggestion = Word { $position }, “{ $word }”, isn't a recovery-key word. Did you mean “{ $suggestion }”?
onboarding-recovery-key-checksum = These words don't form a recovery key. One is probably mistyped or out of order.
onboarding-connect-recovery-message = Your existing identity couldn't be verified. Unlock the desktop keyring and try again, or restore an encrypted backup. Your identity stays as it is until then.
onboarding-connect-choose-file-first = Choose an encrypted backup or legacy identity file first.
onboarding-keyring-unavailable = The desktop keyring is still unavailable. Unlock it and retry, or restore an encrypted backup: { $error }
onboarding-passphrase-too-short = Use at least 12 characters. Make it long and unique; length alone does not make a common passphrase safe.
onboarding-passphrase-mismatch = The two passphrases do not match.
onboarding-passphrase-unavailable = The passphrase fields are temporarily unavailable.
onboarding-codecs-installer-failed = The download could not start: { $error }

## Setup's actions

so-tilo-onboarding-step-acknowledge = Acknowledge
so-tilo-onboarding-step-browse = Choose a file
so-tilo-onboarding-step-choose = Choose another way
so-tilo-onboarding-step-enter = Enter a value
so-tilo-onboarding-step-enter-secret = Enter a secret
so-tilo-onboarding-step-retry = Try again
so-tilo-onboarding-step-skip = Skip
