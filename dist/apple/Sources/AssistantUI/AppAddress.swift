import AssistantCore
import Foundation

/// What the app does with an address of its own that the system hands it: a pairing link from
/// Tilo's page (`HandedLink`), or where a live activity or a notification leads (`LiveLink`).
public enum AppAddress {
    @MainActor
    public static func open(_ address: URL, in model: AssistantModel) {
        if let words = HandedLink.words(in: address) {
            model.takeHanded(words)
        } else {
            LiveLink.open(address, in: model)
        }
    }
}
