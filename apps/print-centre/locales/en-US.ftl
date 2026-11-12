## History

print-centre-print-defaults-two-sided = { $new ->
    [true] Printing on both sides from now on
   *[false] Printing on one side from now on
}
print-centre-print-defaults-scan-resolution = Scanning at { $new } dpi from now on
print-centre-print-defaults-scan-color = { $new ->
    [grayscale] Scanning in grayscale from now on
    [line-art] Scanning in line art from now on
   *[color] Scanning in color from now on
}
print-centre-print-defaults-scan-source = { $new ->
    [feeder] Scanning from the feeder from now on
    [feeder-duplex] Scanning both sides from the feeder from now on
   *[flatbed] Scanning from the flatbed from now on
}
print-centre-print-defaults-ocr-language = { $new ->
    [deu] Recognizing German text from now on
    [fra] Recognizing French text from now on
    [spa] Recognizing Spanish text from now on
   *[eng] Recognizing English text from now on
}

## What the window says while it works

print-centre-loading = Loading Print Centre
print-centre-finding-printers = Finding nearby printers
print-centre-choose-printer = Choose a printer
print-centre-preparing-preview = Preparing document preview
print-centre-all-pages = All pages

## Window and accessibility copy
print-centre-a11y-pages = Pages to print

## Facts, property labels and section titles
print-centre-text-language = Text language
print-centre-orientation = Orientation
print-centre-page-of = Page { $page } of { $pages }
print-centre-availability = Availability
print-centre-state = State
print-centre-section-connection-check = Connection check
print-centre-scanners = Scanners available
print-centre-reporting-levels = Reporting ink or toner levels
print-centre-printers-responding = Printers responding
print-centre-section-tools = Tools
print-centre-section-printers = Your printers

## What the window shows
print-centre-try-again = Try again
print-centre-loading-detail = Looking for printers, scanners and unfinished documents.
print-centre-unavailable-printers = Printers couldn't be checked
print-centre-unavailable-printers-detail = Print Centre couldn't get the printers right now. Your saved devices and documents have not been removed.
print-centre-unavailable-documents = Print jobs couldn't be checked
print-centre-unavailable-documents-detail = Print Centre couldn't get the print jobs right now. Your saved devices and documents have not been removed.
print-centre-unavailable-scanners = Scanners couldn't be checked
print-centre-unavailable-scanners-detail = Print Centre couldn't get the scanners right now. Your saved devices and documents have not been removed.
print-centre-unavailable-some = Some device information couldn't be checked
print-centre-unavailable-some-detail = Print Centre couldn't get every device detail right now. Your saved devices and documents have not been removed.
print-centre-job-waiting = Waiting to print
print-centre-job-attention = Needs attention
print-centre-job-printing = Printing now
print-centre-sent-to = Sent to { $printer }
print-centre-page-progress = Page { $page } of { $total }
print-centre-pages-progress = { $done } of { $total } pages
print-centre-open-queue = Open queue
print-centre-cancel = Cancel
print-centre-confirm-cancel = Cancel this document
print-centre-lowest-supply = Lowest supply
print-centre-supplies-unavailable = Supply levels unavailable
print-centre-preferred = Preferred printer
print-centre-available = Available to your apps
print-centre-no-printers = No printers added yet. Find one on your network to print from any app.
print-centre-find-printer = Find a printer
print-centre-scan-document = Scan a document
print-centre-scanner-ready = { $scanner } is ready
print-centre-history-review = Review what printed and print it again
print-centre-history-empty = Nothing printed yet
print-centre-print-history = Print history
print-centre-device-help = Device help
print-centre-device-help-detail = Check connections, paper, supplies and scanners
print-centre-status-printed = Printed
print-centre-status-canceled = Canceled
print-centre-status-not-printed = Did not print
print-centre-status-failed = Could not print
print-centre-status-waiting = Waiting
print-centre-status-in-progress = In progress
print-centre-printers-responding-count = { $responding } of { $total }
print-centre-reporting-levels-count = { $count ->
    [one] { $count } printer
   *[other] { $count } printers
}
print-centre-print-again = Print again
print-centre-nothing-completed = No completed documents yet.
print-centre-documents = Documents
print-centre-open-controls = Open printer controls
print-centre-no-scanner = No scanner was found. Check its cable or network connection, then check again.
print-centre-scanner-ready-detail = Your scanner is available for a new document.
print-centre-start-scanning = Start scanning
print-centre-scanner = Scanner
print-centre-nothing-waiting = Nothing waiting.
print-centre-pause-printer = Pause printer
print-centre-resume-printer = Resume printer
print-centre-make-preferred = Make preferred
print-centre-view-queue = View print queue
print-centre-check-printer = Check printer
print-centre-no-supply-report = This printer does not report supply levels.
print-centre-details = Details
print-centre-ink-toner = Ink and toner
print-centre-printer-controls = Printer controls
print-centre-searching = Looking for printers on your network.
print-centre-no-printers-found = No printers found. Make sure the printer is awake and connected to the same network, then search again.
print-centre-search-again = Search again
print-centre-nearby = Nearby
print-centre-add = Add
print-centre-printers-network = Printers on your network
print-centre-search-failed = Could not search right now. Check your network connection and try again.
print-centre-preview-unavailable = Preview unavailable
print-centre-original-unchanged = The original document is unchanged.
print-centre-previous = Previous
print-centre-next = Next
print-centre-print = Print
print-centre-portrait = Portrait
print-centre-landscape = Landscape
print-centre-page-of-document = Page { $page } of { $pages }
print-centre-recognized-text = Recognized text
print-centre-rotate-right = Rotate right
print-centre-apply-crop = Apply crop
print-centre-updating-text = Updating recognized text…
print-centre-update-text = Update recognized text
print-centre-copy-text = Copy recognized text
print-centre-page-tools = Page tools
print-centre-previous-page = Previous page
print-centre-next-page = Next page
print-centre-move-earlier = Move before previous page
print-centre-move-later = Move after next page
print-centre-remove-page = Remove this page
print-centre-crop = Crop
print-centre-pages-in-document = Pages in this document
print-centre-pages-in-document-count = { $count ->
    [one] { $count } page
   *[other] { $count } pages
}
print-centre-new-document = New scanned document
print-centre-export-page = Export this page
print-centre-scan-another = Scan another page
print-centre-save-document = Save document
print-centre-no-scanner-available = No scanner is available. Check its cable or network connection, then refresh Print Centre.
print-centre-quality = Quality
print-centre-appearance = Appearance
print-centre-paper-source = Paper source
print-centre-page-settings = Page settings
print-centre-pages-captured = { $count ->
    [one] { $count } page captured. New scans are added to this document.
   *[other] { $count } pages captured. New scans are added to this document.
}
print-centre-scanning-page = Scanning page…
print-centre-scan-page = Scan page

print-centre-job-on = { $status } on { $printer }
print-centre-queue-ready = Ready
print-centre-queue-not-accepting = Not accepting new documents
print-centre-queue-printing = Printing
print-centre-queue-paused = Paused
print-centre-queue-unreachable = Cannot reach this printer
print-centre-reason-out-of-paper = Add paper to continue.
print-centre-reason-jam = Clear the paper jam, then resume the printer.
print-centre-reason-cover-open = Close the printer cover.
print-centre-reason-supply-low = A supply is running low; printing can usually continue for now.
print-centre-reason-supply-empty = Replace the empty supply to continue.
print-centre-reason-offline = Check that the printer is awake and connected.
print-centre-reason-unknown = The printer reported a condition that may need attention.
print-centre-device-answered = The printer answered the latest status check.
print-centre-device-no-supplies = This printer does not report supply levels.
print-centre-device-unreachable = Live details are unavailable. The printer may be asleep or disconnected.
print-job-document = Document { $number }
print-centre-cancel-pending-message = It will be removed from the printer. This cannot be undone.
