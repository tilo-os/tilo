

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
onboarding-snapshot-restore-error = That backup could not be opened. Check the file and passphrase, then try again.
onboarding-snapshot-create-error = The identity could not be saved. Nothing was created; check available storage and try again.
onboarding-codecs-installer-failed = The download could not start: { $error }
onboarding-prepared-other-person = This computer already has another person’s identity, so the prepared setup was left untouched. Restore that person’s own backup or connect their account instead.
onboarding-portable-home-unavailable = Your transfer is not available on this computer yet. Try again or continue without bringing your files over.
onboarding-migration-resume-unavailable = There is no deferred file migration available for your current identity.
onboarding-portable-applications-unavailable = Your apps could not be restored. You can retry without replacing your existing files.
onboarding-browser-unavailable = Your browser data is still waiting. Try again or continue for now.
onboarding-backup-unavailable = A personal identity backup can’t be created on this device.
