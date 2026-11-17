import Foundation
import ShellCore
import UniformTypeIdentifiers

#if os(iOS)
import UIKit
typealias ShareBase = UIViewController
#else
import AppKit
typealias ShareBase = NSViewController
#endif

/// The share sheet's way into the thread. It takes the words or the link another app shares,
/// leaves them where the app picks them up, and says so. The app puts them into the message
/// field the next time it is in front, for the person to send.
@objc(ShareViewController)
final class ShareViewController: ShareBase {
    #if os(iOS)
    private let said = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        said.font = .preferredFont(forTextStyle: .headline)
        said.adjustsFontForContentSizeCategory = true
        said.textAlignment = .center
        said.numberOfLines = 0
        said.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(said)
        NSLayoutConstraint.activate([
            said.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            said.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor),
            said.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        Task { await take() }
    }

    private func show(_ words: String) {
        said.text = words
        UIAccessibility.post(notification: .announcement, argument: words)
    }
    #else
    private let said = NSTextField(wrappingLabelWithString: "")

    override func loadView() {
        let box = NSView(frame: NSRect(x: 0, y: 0, width: 320, height: 110))
        said.alignment = .center
        said.font = .preferredFont(forTextStyle: .headline)
        said.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(said)
        NSLayoutConstraint.activate([
            said.centerYAnchor.constraint(equalTo: box.centerYAnchor),
            said.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 20),
            said.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -20),
        ])
        view = box
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        Task { await take() }
    }

    private func show(_ words: String) {
        said.stringValue = words
    }
    #endif

    private func take() async {
        let item = await read()
        let outcome: Words
        if item.words == nil {
            outcome = .shareNothing
        } else if let place = SharedPlace.handOver(), (try? place.leave(item)) != nil {
            outcome = .shareAdded
        } else {
            outcome = .shareFailed
        }
        show(outcome.said)
        // Long enough to read, then out of the way.
        try? await Task.sleep(for: .seconds(outcome == .shareAdded ? 0.9 : 2.4))
        extensionContext?.completeRequest(returningItems: nil)
    }

    private func read() async -> SharedItem {
        var text: String?
        var link: String?
        for case let item as NSExtensionItem in extensionContext?.inputItems ?? [] {
            if text == nil, let words = item.attributedContentText?.string, !words.isEmpty { text = words }
            for provider in item.attachments ?? [] {
                if link == nil, provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                    link = await words(in: provider, as: .url)
                } else if text == nil, provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                    text = await words(in: provider, as: .plainText)
                }
            }
        }
        return SharedItem(text: text, link: link)
    }

    /// What `provider` holds as `type`, as text. A file's address is not something to say.
    private func words(in provider: NSItemProvider, as type: UTType) async -> String? {
        await withCheckedContinuation { answer in
            provider.loadItem(forTypeIdentifier: type.identifier, options: nil) { value, _ in
                switch value {
                case let url as URL: answer.resume(returning: url.isFileURL ? nil : url.absoluteString)
                case let words as String: answer.resume(returning: words)
                case let data as Data: answer.resume(returning: String(data: data, encoding: .utf8))
                default: answer.resume(returning: nil)
                }
            }
        }
    }
}
