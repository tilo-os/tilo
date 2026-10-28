# The sign-in screen.

greeter-title = Sign in to Tilo
greeter-username = Username
greeter-passcode = Password
greeter-sign-in = Sign in
greeter-signing-in = Signing in…
greeter-starting = Starting your session…
greeter-use-fingerprint = Use fingerprint
greeter-caps-lock = Caps Lock is on
greeter-session = Session
greeter-restart = Restart
greeter-shut-down = Shut down

# Something to fix in the form before signing in.
greeter-hint-username = Enter your username.
greeter-hint-passcode = Enter your password.
greeter-hint-no-fingerprint = This account has no fingerprint set up.

# A sign-in attempt that did not succeed.
greeter-error-passcode = Incorrect password. Try again.
greeter-error-credentials = Incorrect username or password. Try again.
greeter-error-locked = Too many failed attempts. Wait a few minutes, then try again.
greeter-error-account-expired = This account has expired. Ask an administrator to renew it.
greeter-error-passcode-expired = Your password has expired. Ask an administrator to reset it.
greeter-error-not-permitted = This account can't sign in on this computer.
greeter-error-unavailable = Your password couldn't be checked. Try again.
greeter-error-session = Your desktop couldn't start. Try again, or restart the computer.
greeter-error-service = The sign-in service isn't responding. Restart the computer to try again.
greeter-error-no-session = No desktop is installed to sign in to.
greeter-error-passcode-again = Incorrect password, { $count } times in a row. Try again.

# The status corner and the screen's own controls.
greeter-suspend = Suspend
greeter-power = Power
greeter-larger-text = Larger text
greeter-high-contrast = High contrast
greeter-screen-reader = Screen reader
greeter-accounts = Accounts on this computer

greeter-auth-response = Authentication response
greeter-auth-respond = Respond to authentication challenge
greeter-auth-cancel = Cancel sign-in
greeter-auth-cancelling = Cancelling sign-in…
greeter-auth-cancelled = Sign-in cancelled

greeter-session-diagnostics = Configure timing diagnostics for this sign-in

# The sign-in screen before the system volume opens, in the initramfs.
greeter-pin = PIN
greeter-cant-sign-in = Can't sign in
greeter-hint-pin = Enter your PIN.
greeter-error-pin = Incorrect PIN. Try again.
greeter-error-pin-tries = Incorrect PIN. { $count ->
        [one] 1 try left.
       *[other] { $count } tries left.
    }
greeter-error-passcode-tries = Incorrect password. { $count ->
        [one] 1 try left.
       *[other] { $count } tries left.
    }
greeter-error-pin-repeated = You already tried that PIN. Try another.
greeter-error-pin-repeated-tries = You already tried that PIN. { $count ->
        [one] 1 try left.
       *[other] { $count } tries left.
    }
greeter-error-passcode-repeated = You already tried that password. Try another.
greeter-error-passcode-repeated-tries = You already tried that password. { $count ->
        [one] 1 try left.
       *[other] { $count } tries left.
    }
greeter-error-wait-minutes = Too many failed attempts. Try again in { $count ->
        [one] 1 minute.
       *[other] { $count } minutes.
    }
greeter-error-wait-hours = Too many failed attempts. Try again in { $count } hours.
