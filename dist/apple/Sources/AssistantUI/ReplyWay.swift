import AssistantCore
import SwiftUI

/// The way back to the provider step under a reply whose model gave no answer because of the
/// key, the key's account or a missing provider: one press, where the failure stands, drawn as
/// an answer's button is. It leads to the same step the assistant view's Model line leads to;
/// the app has no other field for a key.
struct ReplyWayButton: View {
    let offered: AssistantModel.ReplyWay
    @Environment(AssistantModel.self) private var model

    var body: some View {
        Flow(spacing: 8, lineSpacing: 8) {
            Button(offered.way.words) { model.take(offered) }.buttonStyle(PillStyle(isMain: true))
        }
        .padding(.top, 2)
    }
}
