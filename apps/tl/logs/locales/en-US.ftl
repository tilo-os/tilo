# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-logs = Logs

d-so-tilo-logs-keywords = logs, journal, journalctl, systemd, errors, warnings, diagnostics, protokoll, fehler

d-ydaofznc = Logs
    .doc = The shared journal filter: what the workspace shows of this device's journal.
    .noun = { $count ->
        [one] log
       *[other] logs
    }

d-6e5izmwp = Severity
    .doc = The least severe record shown.

d-lq27tlwx = Unit
    .doc = One unit, or empty for every unit.

d-vzywk5iy = Boot
    .doc = One boot's id, or empty for every boot.

d-yuygrdvc = Search
    .doc = Words to find in a message, a unit or a program.

d-rumvop3c = Following
    .doc = Whether this device's journal is followed live.

d-ydaofznc-filter-following = Following

d-hitfexjm = Export entry
    .doc = One record as the export writes it.
    .noun = { $count ->
        [one] export entry
       *[other] export entries
    }

d-37kvzek7 = Realtime us

d-v3wlyikw = Unit

d-ur4aajno = Identifier

d-wsg4pkbu = Priority

d-cd6taqqw = Message

d-xkxlpyrt = Pid

d-5wwthj2f = Hostname

d-odiofhx6 = Boot id

d-3ra7d3km = Transport

d-djjluyru = Extra

d-hitfexjm-filter-realtime_us = Realtime us

d-hitfexjm-filter-priority = Priority

d-hitfexjm-filter-pid = Pid

d-siu4bjad = Export field
    .noun = { $count ->
        [one] export field
       *[other] export fields
    }

d-mj25pae6 = Name

d-pnqueidz = Value

d-o7nl26fl = Severity
    .doc = The least severe record the window shows.

d-cjghwldx = Only emergencies
    .doc = Only emergencies.

d-rtsnau5j = Alerts and worse
    .doc = Alerts and worse.

d-yqzshsup = Critical and worse
    .doc = Critical and worse.

d-g4l2hdej = Errors and worse
    .doc = Errors and worse.

d-lnknco5l = Warnings and worse
    .doc = Warnings and worse.

d-77mgxiik = Notices and worse
    .doc = Notices and worse.

d-e3vdpsxj = Information and worse
    .doc = Information and worse.

d-hjvfczr3 = Every record, including debug
    .doc = Every record, including debug.

d-h7ux6zuj = Made export
    .doc = The records an export writes, one JSON object a line.

d-fslvwdeu = Set severity
    .doc = Shows records of at least `value`'s severity.

d-fslvwdeu-value = Value

d-qfveahvf = Set unit
    .doc = Shows one unit's records, or every unit's when `value` is empty.

d-qfveahvf-value = Value

d-e24v4l2c = Set boot
    .doc = Shows one boot's records, or every boot's when `value` is empty.

d-e24v4l2c-value = Value

d-5rvl6tfm = Set search
    .doc = Narrows the records to those holding `value`.

d-5rvl6tfm-value = Value

d-u6of4uob = Clear filters
    .doc = Clears every journal filter.

d-ac3g3mvu = Refresh journal
    .doc = Reads the most recent records of this device's journal.

d-equcewsy = Follow
    .doc = Follows new records as they arrive, or stops following them.

d-equcewsy-on = On

d-liurooe5 = Poll tail
    .doc = Reads the records written since the last one read, while following.

d-v3wcvt43 = Copy entry
    .doc = Copies one record, with its time, unit, program and message.

d-v3wcvt43-entry = Entry

d-jnxcvadn = Copy message
    .doc = Copies one record's message.

d-jnxcvadn-entry = Entry

d-xwaqirn3 = Severity words
    .w-emergency = Emergency
    .w-alert = Alert
    .w-critical = Critical
    .w-error = Error
    .w-warning = Warning
    .w-notice = Notice
    .w-info = Info
    .w-debug = Debug

d-yij3luub = Window
    .w-logs = Logs
    .w-filter = Filter
    .w-emerg = emerg
    .w-alert = alert
    .w-crit = crit
    .w-err = err
    .w-warning = warning
    .w-notice = notice
    .w-info = info
    .w-debug = debug
    .w-severity = Severity
    .w-unit = Unit
    .w-boot = Boot
    .w-search-the-journal = Search the journal
    .w-clear-filters = Clear filters
    .w-following-the-journal = Following the journal
    .w-reading-the-journal = Reading the journal…
    .w-follow-live = Follow live
    .w-refresh-journal = Refresh journal
    .w-export-filtered-logs = Export filtered logs
    .w-no-records-match = No records match
    .w-the-filter-hides-every-record-this = The filter hides every record this journal holds.
    .w-records-of = { $len } records of { $len2 }
    .w-show-this-record = Show this record
    .w-this-record-is-already-shown = This record is already shown
    .w-no-record-selected = No record selected
    .w-choose-a-record-to-see-its = Choose a record to see its fields.

d-p3un3cpt = Unavailable
    .w-the-journal-is-not-available = The journal is not available
    .w-this-computer-s-system-journal-could = This computer's system journal could not be read.
    .w-try-again = Try again

d-cxvsq2r6 = Empty journal
    .w-no-records = No records
    .w-this-device-s-journal-holds-no = This device's journal holds no records yet.
    .w-read-the-journal = Read the journal

d-wfe6eitp = Entry detail
    .w-none = None
    .w-record = Record
    .w-time = Time
    .w-severity = Severity
    .w-unit = Unit
    .w-program = Program
    .w-process-id = Process ID
    .w-host = Host
    .w-boot-id = Boot ID
    .w-transport = Transport
    .w-fields = Fields
    .w-copy-message = Copy message
    .w-copy-entry = Copy entry
