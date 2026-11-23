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

/// The rule of the swipe that answers a piece on an iPhone: how far a drag pulls the piece, and
/// whether letting go there answers it. Plain numbers of a drag, so the rule is the same
/// wherever it is asked.
enum SwipePull {
    /// How far a piece is pulled before letting go answers it, and how far it follows at most.
    static let mark: CGFloat = 56
    static let most: CGFloat = 72

    /// How far a drag pulls the piece: to the right, and more across than down.
    static func pull(across: CGFloat, down: CGFloat) -> CGFloat {
        across > 0 && across > 2 * abs(down) ? min(across, most) : 0
    }

    /// Whether a drag let go there answers the piece: it was pulled to the mark.
    static func answers(across: CGFloat, down: CGFloat) -> Bool {
        pull(across: across, down: down) >= mark
    }
}

#if !os(macOS)
/// A swipe to the right on a piece answers it. The piece follows the finger a little way and,
/// let go past the mark, stands above the field. Only a drag that goes sideways counts: a
/// scroll of the conversation moves nothing here. The rule is `SwipePull`.
struct SwipeToReply: ViewModifier {
    let reply: () -> Void
    @GestureState(resetTransaction: Transaction(animation: .easeOut(duration: Motion.base))) private var pulled: CGFloat = 0

    func body(content: Content) -> some View {
        content
            .offset(x: pulled)
            .background(alignment: .leading) {
                // What letting go will do shows where the piece stood.
                Glyph(.ret).foregroundStyle(Token.ink3).opacity(Double(min(pulled / SwipePull.mark, 1))).accessibilityHidden(true)
            }
            .simultaneousGesture(
                DragGesture(minimumDistance: 20)
                    .updating($pulled) { drag, pulled, _ in
                        pulled = SwipePull.pull(across: drag.translation.width, down: drag.translation.height)
                    }
                    .onEnded { drag in
                        if SwipePull.answers(across: drag.translation.width, down: drag.translation.height) { reply() }
                    })
    }
}
#endif
