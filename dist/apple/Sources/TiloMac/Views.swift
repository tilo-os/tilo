#if os(macOS)
import AppKit
import ShellCore
import ShellWeb

/// What the window shows when there is no page to show: what is wrong, and the one thing to do.
@MainActor
final class ProblemView: NSView {
    private let act: () -> Void

    init(title: String, body: String, button: String, act: @escaping () -> Void) {
        self.act = act
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        let heading = NSTextField(wrappingLabelWithString: title)
        heading.font = .preferredFont(forTextStyle: .title2)
        heading.alignment = .center
        let words = NSTextField(wrappingLabelWithString: body)
        words.textColor = .secondaryLabelColor
        words.alignment = .center
        let press = NSButton(title: button, target: self, action: #selector(pressed))
        press.keyEquivalent = "\r"
        let stack = NSStackView(views: [heading, words, press])
        stack.orientation = .vertical
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            stack.widthAnchor.constraint(lessThanOrEqualToConstant: 460),
            stack.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 24),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("made in code") }

    @objc private func pressed() { act() }
}

/// A bar under the page while the site keeps this device out: what brings the person in, and
/// the one button that does it.
@MainActor
final class WayInBar: NSView {
    private let act: () -> Void

    init(words: String, button: String, act: @escaping () -> Void) {
        self.act = act
        super.init(frame: .zero)
        let said = NSTextField(wrappingLabelWithString: words)
        said.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let press = NSButton(title: button, target: self, action: #selector(pressed))
        press.setContentHuggingPriority(.required, for: .horizontal)
        press.setContentCompressionResistancePriority(.required, for: .horizontal)
        let row = NSStackView(views: [said, press])
        row.orientation = .horizontal
        row.spacing = 12
        row.edgeInsets = NSEdgeInsets(top: 10, left: 16, bottom: 10, right: 16)
        row.translatesAutoresizingMaskIntoConstraints = false
        let line = NSBox()
        line.boxType = .separator
        line.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        addSubview(line)
        NSLayoutConstraint.activate([
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor),
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(equalTo: trailingAnchor),
            line.topAnchor.constraint(equalTo: topAnchor),
            line.leadingAnchor.constraint(equalTo: leadingAnchor),
            line.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("made in code") }

    @objc private func pressed() { act() }
}

/// One line over the foot of the page, for the moment words wait for a place to go.
@MainActor
final class HintView: NSVisualEffectView {
    init(_ words: String) {
        super.init(frame: .zero)
        material = .hudWindow
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 10
        let label = NSTextField(labelWithString: words)
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            label.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
        ])
        setAccessibilityLabel(words)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("made in code") }

    func place(in content: NSView) {
        translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(self)
        NSLayoutConstraint.activate([
            centerXAnchor.constraint(equalTo: content.centerXAnchor),
            bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -24),
        ])
        NSAccessibility.post(element: self, notification: .announcementRequested,
                             userInfo: [.announcement: accessibilityLabel() ?? ""])
        // It is a hint, not a state: gone after a while even if nothing was pressed.
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(12))
            self?.removeFromSuperview()
        }
    }
}

/// The site the app opens, and the way to the system's own settings for it.
@MainActor
final class SettingsWindow: NSObject {
    private let window: NSWindow
    private let site = NSTextField(string: "")
    private let help = NSTextField(wrappingLabelWithString: Words.siteHelp.said)
    private let desktop = NSButton(checkboxWithTitle: Words.showDesktop.said, target: nil, action: nil)
    private let changed: () -> Void

    init(changed: @escaping () -> Void) {
        self.changed = changed
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 440, height: 240),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
        super.init()
        window.title = Words.settings.said.replacingOccurrences(of: "…", with: "")
        window.isReleasedWhenClosed = false
        let label = NSTextField(labelWithString: Words.siteLabel.said)
        site.placeholderString = ShellSettings.productionSite
        site.setAccessibilityLabel(Words.siteLabel.said)
        help.textColor = .secondaryLabelColor
        help.font = .preferredFont(forTextStyle: .footnote)
        let use = NSButton(title: Words.useSite.said, target: self, action: #selector(useSite))
        use.keyEquivalent = "\r"
        let notifications = NSTextField(labelWithString: Words.notificationsLabel.said)
        let open = NSButton(title: Words.openNotificationSettings.said, target: self,
                            action: #selector(openNotificationSettings))
        let stack = NSStackView(views: [label, site, help, desktop, use, NSBox.separator(), notifications, open])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        stack.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = NSView()
        window.contentView?.addSubview(stack)
        if let content = window.contentView {
            NSLayoutConstraint.activate([
                stack.topAnchor.constraint(equalTo: content.topAnchor),
                stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
                stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                site.widthAnchor.constraint(equalToConstant: 400),
                help.widthAnchor.constraint(equalToConstant: 400),
            ])
        }
    }

    func show() {
        if case .success(let settings) = ShellSettings.load() {
            site.stringValue = settings.origin
            desktop.state = settings.desktop ? .on : .off
        }
        help.stringValue = Words.siteHelp.said
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    @objc private func useSite() {
        do {
            let chosen = try ShellSettings(site: site.stringValue)
            UserDefaults.standard.set(chosen.origin, forKey: ShellSettings.siteKey)
            UserDefaults.standard.set(desktop.state == .on, forKey: ShellSettings.desktopKey)
            window.close()
            changed()
        } catch {
            // Said where the address was typed, and the address stays as typed.
            help.stringValue = (error == .notSecure ? Words.addressNotSecure : Words.addressNotAnAddress).said
            NSAccessibility.post(element: help, notification: .announcementRequested,
                                 userInfo: [.announcement: help.stringValue])
        }
    }

    @objc private func openNotificationSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") else { return }
        NSWorkspace.shared.open(url)
    }
}

private extension NSBox {
    static func separator() -> NSBox {
        let line = NSBox()
        line.boxType = .separator
        return line
    }
}

/// A small panel that shows what is heard while the person dictates, with Insert and Cancel.
/// The system's own indicator shows that the microphone is on.
@MainActor
final class DictationPanel: NSObject, NSWindowDelegate {
    private let panel: NSPanel
    private let heard = NSTextField(wrappingLabelWithString: Words.listening.said)
    private let insert: NSButton
    private let dictation = Dictation()
    private let done: (String?) -> Void
    private var finished = false

    /// - Parameter done: called once, with the words to insert or `nil` when there are none.
    init(done: @escaping (String?) -> Void) {
        self.done = done
        panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 380, height: 150),
                        styleMask: [.titled, .closable, .utilityWindow], backing: .buffered, defer: false)
        insert = NSButton(title: Words.insertWords.said, target: nil, action: nil)
        super.init()
        panel.title = Words.dictate.said
        panel.isFloatingPanel = true
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        heard.textColor = .secondaryLabelColor
        heard.maximumNumberOfLines = 5
        insert.target = self
        insert.action = #selector(insertWords)
        insert.keyEquivalent = "\r"
        insert.isEnabled = false
        let cancel = NSButton(title: Words.cancel.said, target: self, action: #selector(cancelled))
        cancel.keyEquivalent = "\u{1b}"
        let buttons = NSStackView(views: [cancel, insert])
        let stack = NSStackView(views: [heard, buttons])
        stack.orientation = .vertical
        stack.alignment = .trailing
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 18, bottom: 18, right: 18)
        stack.translatesAutoresizingMaskIntoConstraints = false
        panel.contentView = NSView()
        panel.contentView?.addSubview(stack)
        if let content = panel.contentView {
            NSLayoutConstraint.activate([
                stack.topAnchor.constraint(equalTo: content.topAnchor),
                stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
                stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                heard.widthAnchor.constraint(equalToConstant: 344),
            ])
        }
    }

    func start() {
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        NSApp.activate()
        dictation.heard = { [weak self] words in
            self?.heard.stringValue = words
            self?.heard.textColor = .labelColor
            self?.insert.isEnabled = true
        }
        // Listening ended by itself. What was heard stays to be inserted; with nothing heard
        // there is nothing to wait for.
        dictation.ended = { [weak self] in
            guard let self, self.dictation.words.isEmpty else { return }
            self.finish(with: nil)
        }
        Task {
            do throws(Dictation.Problem) {
                try await dictation.start()
            } catch {
                heard.stringValue = (error == .notAllowed ? Words.dictationNotAllowed : Words.dictationUnavailable).said
            }
        }
    }

    @objc private func insertWords() { finish(with: dictation.words) }
    @objc private func cancelled() { finish(with: nil) }
    func windowWillClose(_ notification: Notification) { finish(with: nil) }

    private func finish(with words: String?) {
        guard !finished else { return }
        finished = true
        dictation.stop()
        panel.delegate = nil
        panel.close()
        done(words.flatMap { $0.isEmpty ? nil : $0 })
    }
}
#endif
