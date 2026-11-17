import AssistantCore
import AssistantDrawing
import AssistantUI
import SwiftUI

/// A message that carries a view, as the main chat shows it: its words, and under them the
/// place of the view at the height the view asked for.
///
/// No engine draws in this program, so what stands in the place is this file's own stand-in
/// for what Tilo's page draws there: a view over the areas Tasks keeps. The picture shows the
/// place and its height, never Tilo's own drawing.
@MainActor
enum ViewPiece {
    /// The height the stand-in asks for, as a view's mount says its own.
    static let asked: CGFloat = 132

    static var items: [Item] {
        [
            Item(id: "areas-asked", at: Drawing.now, .message(Message(.person, "Which areas does Tasks keep right now?"))),
            Item(id: "areas", at: Drawing.now, .message(Message(
                .assistant, "These are the areas as they stand. The view follows Tasks while you read it.",
                live: [LiveThing(id: "areas", title: "Tasks", reference: "view:areas:main", sizesItself: true)]))),
        ]
    }

    static func screen(_ scheme: ColorScheme) async -> Screen {
        let one = await Screens.model(Screens.piece(items))
        return Screen(name: "piece-view", size: CGSize(width: 429, height: 330), view: Screens.rooted(one, scheme) {
            ConversationView(chat: .main, picturedSecret: "")
                .environment(\.liveThingView, { _ in AnyView(StandIn()) })
                .background(Token.content)
        })
    }

    private struct StandIn: View {
        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(["Launch", "Growth", "Platform"], id: \.self) { area in
                    Text(area).font(.tilo(14, .medium)).foregroundStyle(Token.ink)
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .frame(height: ViewPiece.asked, alignment: .topLeading)
            .background(Token.raised)
        }
    }
}
