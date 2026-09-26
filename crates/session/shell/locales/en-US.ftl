# Continuing on this desktop what you were just doing on another device.
continuity-continue = Continue { $app } from { $device }
continuity-continue-subtitle = Focus the tab you were just using on { $device }
continuity-dismiss = Dismiss
continuity-dismiss-title = Dismiss “Continue { $app } from { $device }”
continuity-dismiss-subtitle = Hidden until you switch tabs on { $device } again
continuity-other-device = your other device

# Undoing a tab close.
tab-closed-toast = Closed { $title }
tab-closed-group-toast = Closed { $count } tabs
tab-closed-undo = Undo
tab-closed-undo-hint = Select to undo, or press { $chord } later
tab-closed-undo-hint-unbound = Select to undo

# Display layout sheet (Super+P)
display-sheet-title = Display
display-sheet-subtitle = Choose where your desktop appears. Super+P moves to the next choice.
display-sheet-a11y-description = Up and Down move between choices, Enter applies the highlighted one, Super+P moves to the next layout, Escape closes.
display-mode-extend = Extend
display-mode-extend-detail = One desktop across every display
display-mode-mirror = Mirror
display-mode-mirror-detail = The same picture on every display
display-mode-builtin-only = Only this screen
display-mode-builtin-only-detail = External displays turn off
display-mode-external-only = Only external
display-mode-external-only-detail = The built-in screen turns off
display-mode-current = Current
display-mode-unavailable-single = Connect another display to use this
display-mode-unavailable-builtin = This device has no built-in screen
display-mode-unavailable-mirror = These displays share no resolution
display-confirm-title = Keep these display settings?
display-confirm-countdown = Reverting in { $seconds } s
display-confirm-applying = Applying…
display-confirm-keep = Keep
display-confirm-revert = Revert
display-rejected = Couldn't apply this layout: { $reason }

# Keep or revert a change a display tool (kanshi, wlr-randr) made
display-confirm-tool-title = Keep the display settings { $tool } applied?
display-confirm-tool-unknown-title = Keep the display settings a tool applied?
display-confirm-always = Always keep changes from { $tool }
display-confirm-a11y-description = A display tool changed your screens. Enter keeps the change, Escape restores the previous layout.

# Wireless receivers, the lower section of the same sheet
wireless-heading = Wireless displays
wireless-share-audio = Share audio
wireless-stream = Stream
wireless-stream-auto = Auto
wireless-stream-fixed = Fixed
wireless-stream-fps = { $fps } fps
wireless-credential-label = Receiver PIN or password
wireless-credential-placeholder = PIN or receiver password
wireless-credential-hint = Leave blank unless the receiver asks. Enter connects.
wireless-scanning = Looking for displays…
wireless-scanning-detail = Keep the receiver awake and on the same network.
wireless-scan-failed = Couldn't scan
wireless-none = No displays found
wireless-none-detail = Try Refresh, or check Wi-Fi and the receiver's casting mode.
wireless-connected = Connected
wireless-connecting = Connecting…
wireless-connect = Connect
wireless-stop = Stop
wireless-wait = Wait
wireless-protocols = Miracast · Google Cast · AirPlay · FCast

# Identify displays: the card painted on each display
display-identify-builtin = Built-in display
display-identify-unnamed = Display { $number }
