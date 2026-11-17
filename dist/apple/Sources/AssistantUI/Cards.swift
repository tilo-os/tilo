import AssistantCore
import SwiftUI

// The pieces of a conversation that are more than words: a secret it needs, a service that
// wants a sign-in, an event it set up, a brief.

/// A card's body: `.box`, with its head `.hd`.
struct CardBox<Content: View>: View {
    let name: String
    let note: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(name).font(.tilo(14, .semibold)).foregroundStyle(Token.ink)
                Spacer(minLength: 0)
                Text(note).font(.small125).foregroundStyle(Token.ink2)
            }
            content
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.bubbleRadius))
        .frame(maxWidth: Measure.bubbleWidth, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// How a settled card says where it stands.
private struct Settled: View {
    let words: String
    var good = true

    var body: some View {
        HStack(spacing: 8) {
            if good { Glyph(.check).foregroundStyle(Token.on) }
            Text(words).font(.body14).foregroundStyle(Token.ink2)
        }
    }
}

/// A secret the assistant needs. The field is the system's protected one, and what is typed in
/// it lives in this view alone until Save hands it to the vault: it is never part of a message,
/// a draft, a notification or a log.
struct SecretCard: View {
    let request: SecretRequest
    /// Dots to stand in the field for a picture of it filled. Nothing a person types comes this way.
    var pictured = ""
    @Environment(AssistantModel.self) private var model
    @Environment(\.coversSecrets) private var covered
    @State private var typed = ""
    @State private var saving = false

    var body: some View {
        Bubble { MarkdownText(text: request.text) }
        CardBox(name: request.name, note: AssistantWords.keptInVault.said) {
            switch request.state {
            case .waiting:
                HStack(spacing: 10) {
                    Glyph(.lock).foregroundStyle(Token.ink3)
                    SecureField("", text: $typed)
                        .textFieldStyle(.plain)
                        .font(.tiloMono(14))
                        .foregroundStyle(Token.ink2)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never)
                        #endif
                        .onSubmit(save)
                        .accessibilityLabel(AssistantWords.secretField.said(["name": request.name]))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(Token.content, in: RoundedRectangle(cornerRadius: 9))
                .overlay {
                    // Out of sight while the app is not in front: not how long it is, not a dot of it.
                    if covered {
                        HStack(spacing: 10) {
                            Glyph(.lock).foregroundStyle(Token.ink3)
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 12)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Token.content, in: RoundedRectangle(cornerRadius: 9))
                        .accessibilityHidden(true)
                    }
                }
                .accessibilityHidden(covered)
                Flow(spacing: 8, lineSpacing: 8) {
                    Button(AssistantWords.save.said, action: save)
                        .buttonStyle(BoxButtonStyle(isMain: true))
                        .disabled(typed.isEmpty || saving)
                    if let manager = request.manager {
                        Button(AssistantWords.getFromManager.said(["manager": manager])) {
                            typed = ""
                            model.fetchSecret(for: request.id)
                        }
                        .buttonStyle(BoxButtonStyle())
                    }
                }
                Fine(words: AssistantWords.secretFine.said)
            case .kept:
                Settled(words: AssistantWords.secretKept.said)
            case .declined:
                Settled(words: AssistantWords.secretDeclined.said, good: false)
            }
        }
        .onAppear { if typed.isEmpty { typed = pictured } }
        .onDisappear { typed = "" }
    }

    private func save() {
        guard !typed.isEmpty, !saving else { return }
        // The field is emptied before the secret leaves: it is held in one place at a time.
        let secret = Secret(typed)
        typed = ""
        saving = true
        Task {
            _ = await model.keep(secret, for: request.id)
            saving = false
        }
    }
}

/// A service the person named by its address, which wants a sign-in.
struct ConnectionCard: View {
    let offer: ConnectionOffer
    @Environment(AssistantModel.self) private var model
    @Environment(\.openURL) private var openURL

    var body: some View {
        Bubble { MarkdownText(text: offer.text) }
        CardBox(name: offer.name, note: offer.commands.map { AssistantWords.commandsAfter.said(["count": String($0)]) } ?? "") {
            switch offer.state {
            case .waiting:
                Flow(spacing: 8, lineSpacing: 8) {
                    Button(AssistantWords.authenticate.said) {
                        Task {
                            if let page = await model.authenticate(offer.id) { openURL(page) }
                        }
                    }
                    .buttonStyle(BoxButtonStyle(isMain: true))
                    Button(AssistantWords.notNow.said) { model.decline(offer.id) }.buttonStyle(BoxButtonStyle())
                }
            case .connected:
                Settled(words: AssistantWords.connected.said)
            case .declined:
                Settled(words: AssistantWords.notConnected.said, good: false)
            }
        }
    }
}

/// A small rounded word: `.tag`.
struct Tag: View {
    let words: String

    var body: some View {
        Text(words).font(.tag115).foregroundStyle(Token.ink2)
            .padding(.horizontal, 9).padding(.vertical, 2)
            .background(Token.raised, in: Capsule())
    }
}

/// An event the assistant set up, shown for review: its filter, its note, and the answers.
struct EventCardView: View {
    let card: EventCard
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    var body: some View {
        Bubble { MarkdownText(text: card.text) }
        CardBox(name: card.automation.title, note: wording.state(card.automation)) {
            let tags = wording.cardTags(card.automation)
            if !tags.isEmpty {
                Flow(spacing: 8, lineSpacing: 6) {
                    ForEach(tags, id: \.self) { Tag(words: $0) }
                }
            }
            if let chosen = card.answer, let choice = card.choices.first(where: { $0.id == chosen }) {
                Fine(words: AssistantWords.youChose.said(["answer": choice.title]))
            } else {
                Flow(spacing: 8, lineSpacing: 8) {
                    ForEach(card.choices) { choice in
                        Button(choice.title) { model.answer(card.question, with: choice.id) }
                            .buttonStyle(BoxButtonStyle(isMain: choice.isMain))
                    }
                }
            }
        }
    }
}

/// What the person should know before something begins.
struct BriefView: View {
    let brief: Brief
    let isImportant: Bool

    var body: some View {
        Flow(spacing: 7, lineSpacing: 4) {
            EyeChip()
            Text(brief.title).foregroundStyle(Token.ink)
            if isImportant { ImportantMark() }
        }
        .font(.line135)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
        Bubble { MarkdownText(text: brief.text) }
    }
}
