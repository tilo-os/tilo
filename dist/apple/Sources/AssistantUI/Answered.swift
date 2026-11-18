import AssistantCore
import SwiftUI

/// The line that says which message is answered: a thin bar and one line of that message's
/// words. Above the field it carries a cross that drops it; above a reply it carries none.
struct AnsweredLine: View {
    let answered: Answered
    /// Drop what is answered: the cross above the field. Nil where nothing can be dropped.
    var drop: (() -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            RoundedRectangle(cornerRadius: 1).fill(Token.line).frame(width: 2).accessibilityHidden(true)
            Text(words).font(.small12).foregroundStyle(Token.ink2).lineLimit(1).truncationMode(.tail)
                .accessibilityLabel("\(AssistantWords.replyingTo.said): \(words)")
            if let drop {
                Spacer(minLength: 0)
                Button {
                    drop()
                } label: {
                    Glyph(.x, side: 11).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                .accessibilityLabel(AssistantWords.dropReply.said)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    /// One line of the answered words; where this device does not hold them, that there are some.
    private var words: String {
        answered.line.isEmpty ? AssistantWords.earlierMessage.said : answered.line
    }
}

#if !os(macOS)
/// A swipe to the right on a piece answers it. The piece follows the finger a little way and,
/// let go past the mark, stands above the field. Only a drag that goes sideways counts: a
/// scroll of the conversation moves nothing here.
struct SwipeToReply: ViewModifier {
    let reply: () -> Void
    @GestureState(resetTransaction: Transaction(animation: .easeOut(duration: Motion.base))) private var pulled: CGFloat = 0

    /// How far a piece is pulled before letting go answers it, and how far it follows at most.
    static let mark: CGFloat = 56
    static let most: CGFloat = 72

    /// How far a drag pulls the piece: to the right, and more across than down.
    static func pull(_ drag: DragGesture.Value) -> CGFloat {
        let across = drag.translation.width, down = abs(drag.translation.height)
        return across > 0 && across > 2 * down ? min(across, most) : 0
    }

    func body(content: Content) -> some View {
        content
            .offset(x: pulled)
            .background(alignment: .leading) {
                // What letting go will do shows where the piece stood.
                Glyph(.ret).foregroundStyle(Token.ink3).opacity(Double(min(pulled / Self.mark, 1))).accessibilityHidden(true)
            }
            .simultaneousGesture(
                DragGesture(minimumDistance: 20)
                    .updating($pulled) { drag, pulled, _ in pulled = Self.pull(drag) }
                    .onEnded { drag in
                        if Self.pull(drag) >= Self.mark { reply() }
                    })
    }
}
#endif
