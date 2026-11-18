import AssistantCore
import AssistantDrawing
import AssistantUI
import SwiftUI

/// A reply at five moments of its writing, as the main chat shows them: taken up with no word
/// yet; its first words, with strong words still open; a list whose next entry has only its
/// dash; a fence of code that is being written; and written to its end.
@MainActor
enum GrowingReply {
    private static let asked = Item(id: "growing-asked", at: Drawing.now,
                                    .message(Message(.person, "What do the numbers of this week say?")))
    private static let list = "Usage went **up by four percent** this week.\n\n- sign-ups: 412\n- churned: 3\n-"
    private static let fence = "Usage went **up by four percent** this week.\n\n- sign-ups: 412\n- churned: 3\n- open: 17\n\n"
        + "The sheet computes it so:\n\n```\nchurn = left / (had + came)"

    /// Each moment: its name in the picture's file, the reply's words then, and whether it is still being written.
    private static let moments: [(name: String, words: String, isWritten: Bool)] = [
        ("1-taken-up", "", true),
        ("2-first-words", "Usage went **up by four", true),
        ("3-a-list", list, true),
        ("4-a-fence", fence, true),
        ("5-to-its-end", fence + "\n```\n\nAsk me for the plans one by one.", false),
    ]

    /// A model that holds the conversation as it stands at a moment, and is told that an answer
    /// is being written while one is.
    private static func model(_ items: [Item], isWritten: Bool) async -> AssistantModel {
        let source = MemorySource(Screens.piece(items), now: { Drawing.now })
        if isWritten { source.setAnswering(true) }
        let model = AssistantModel(source: source)
        model.start()
        for _ in 0..<400 {
            let held = model.conversation(.main)
            if held.isLoaded, held.isAnswering == isWritten { break }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return model
    }

    /// The five moments, each at the width of the drawing's piece and at an iPhone's.
    static func screens(_ scheme: ColorScheme) async -> [Screen] {
        var screens: [Screen] = []
        for moment in moments {
            let reply = Item(id: "growing", at: Drawing.now, .message(Message(.assistant, moment.words)))
            let held = await model([asked, reply], isWritten: moment.isWritten)
            for (suffix, width) in [("", CGFloat(429)), ("-phone", CGFloat(393))] {
                screens.append(Screen(name: "growing-reply-\(moment.name)\(suffix)", size: CGSize(width: width, height: 380),
                                      view: Screens.rooted(held, scheme) {
                                          ConversationView(chat: .main, picturedSecret: "").background(Token.content)
                                      }))
            }
        }
        return screens
    }
}
