import AssistantCore
import AuthenticationServices
import SwiftUI

// The pieces of a conversation that are more than words: a secret it needs, a service that
// wants a sign-in, an event it set up, a brief.

/// A card's body: `.box`, with its head `.hd`.
struct CardBox<Content: View>: View {
    let name: String
    let note: String
    @ViewBuilder var content: Content
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        // At the largest text sizes the note stands under the name, not squeezed beside it.
        let large = textSize.isAccessibilitySize
        let head = large ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2))
            : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 12))
        VStack(alignment: .leading, spacing: 10) {
            head {
                // A name is read whole: a secret that belongs to two servers names both, and
                // a person reads both before they type it. It wraps and is never cut short.
                Text(name).font(.tilo(14, .semibold)).foregroundStyle(Token.ink)
                    .fixedSize(horizontal: false, vertical: true)
                if !large { Spacer(minLength: 0) }
                Text(note).font(.small125).foregroundStyle(Token.ink2).accessibilityLabel(note.spoken)
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
    @State private var typed = ""
    @State private var saving = false

    var body: some View {
        Bubble { MarkdownText(text: request.text) }
        CardBox(name: request.name, note: refused ? "" : AssistantWords.keptInVault.said) {
            switch request.state {
            case .waiting:
                ProtectedField(typed: $typed, label: AssistantWords.secretField.said(["name": request.name]), submit: save)
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
            case .refused(let why):
                Settled(words: why.isEmpty ? AssistantWords.secretDeclined.said : why, good: false)
            }
        }
        .onAppear { if typed.isEmpty { typed = pictured } }
        .onDisappear { typed = "" }
    }

    /// Whether where the secret belongs did not take it: nothing of it is kept then, and the
    /// card does not say beside its name that the vault keeps it.
    private var refused: Bool {
        if case .refused = request.state { return true }
        return false
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

/// The system's protected field behind its lock: the one field of the app that a secret is typed
/// into, for the card that asks for a secret and for the one that links a provider. What is
/// typed lives in the card's own state until its button hands it on, and it is out of sight
/// while the app is not in front.
struct ProtectedField: View {
    @Binding var typed: String
    /// What the field is for, as it is read out.
    let label: String
    let submit: () -> Void
    @Environment(\.coversSecrets) private var covered

    var body: some View {
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
                .onSubmit(submit)
                .accessibilityLabel(label)
                .accessibilityHidden(covered)
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
    }
}

/// What stands between the person and their assistant's first answer, said in the main chat
/// where that answer would stand: the assistant being set up, why it could not be, the provider
/// to link, the model to choose.
struct SetupStep: View {
    @Environment(AssistantModel.self) private var model
    /// The models that can be chosen since a provider was linked here: the choice stays in the
    /// conversation until the person made it, or went on writing.
    @State private var offered: [ModelChoice] = []
    @State private var choosingFrom: [AssistantAppChoice]?

    var body: some View {
        Group {
            switch model.setup {
            case .findingAssistant:
                // The look every start makes. The window says over the message field that it
                // waits for Tilo, and nothing stands here.
                EmptyView()
            case .makingWorkspace:
                // Their server has been making their own workspace for a while: said here, so
                // that nobody takes the wait for a window that stopped.
                Fine(words: AssistantWords.makingWorkspace.said).padding(.leading, 14)
            case .makingAssistant:
                // The first time: an assistant is being made for a person who keeps none.
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(AssistantWords.makingAssistant.said).font(.body14).foregroundStyle(Token.ink2)
                }
                .padding(.leading, 14)
                .accessibilityElement(children: .combine)
            case .noAssistant(let why):
                Bubble { MarkdownText(text: why) }
                Button(AssistantWords.tryAgain.said) { model.makeAssistant() }.buttonStyle(BoxButtonStyle(isMain: true))
            case .chatPermission(let permission):
                if permission.canRequest, model.canChooseAssistantApps, let choosingFrom,
                   choosingFrom == model.supervision.assistantAppChoices {
                    AssistantAppPicker(choices: choosingFrom) { self.choosingFrom = nil }
                        .onDisappear { self.choosingFrom = nil }
                } else {
                    chatAccess(permission)
                }
            case .needsProvider(let providers):
                ProviderCard(providers: providers) { offered = $0 }
            case .needsModel(let models):
                if let step = model.keyStep { back(to: step) } else { ModelCard(models: models) }
            case .unknown, .ready:
                if let step = model.keyStep {
                    back(to: step)
                } else if !offered.isEmpty {
                    ModelCard(models: offered, answers: model.supervision.model)
                }
            }
        }
        .onChange(of: model.conversation(.main).items.count) { offered = [] }
    }

    @ViewBuilder private func chatAccess(_ permission: ChatPermission) -> some View {
        Bubble {
            VStack(alignment: .leading, spacing: 8) {
                switch permission {
                case .needed: EmptyView()
                case .declined: Text(AssistantWords.chatPermissionDeclined.said)
                case .expired: Text(AssistantWords.chatPermissionExpired.said)
                case .revoked: Text(AssistantWords.chatPermissionRevoked.said)
                case .pending(let code):
                    Text(AssistantWords.chatPermissionPending.said)
                    if let code, !code.isEmpty {
                        Text(AssistantWords.chatPermissionCode.said(["code": code])).monospacedDigit()
                    }
                case .joining: Text(AssistantWords.chatPermissionJoining.said)
                case .unavailable(let reason):
                    Text(AssistantWords.chatPermissionUnavailable.said)
                    if !reason.isEmpty { Text(verbatim: reason) }
                }
                if permission.canRequest { Text(AssistantWords.chatPermissionScope.said) }
            }
        }
        if permission.canRequest {
            Button(AssistantWords.requestChatPermission.said) { Task { await model.requestChatPermission() } }
                .buttonStyle(BoxButtonStyle(isMain: true))
                .accessibilityIdentifier("request-chat-permission")
                .disabled(model.chatPermissionBusy)
            Button(AssistantWords.requestAssistantWorkPermission.said) { Task { await model.requestAssistantWorkPermission() } }
                .buttonStyle(BoxButtonStyle())
                .accessibilityIdentifier("request-assistant-work-permission")
                .disabled(model.chatPermissionBusy)
            if model.canChooseAssistantApps {
                Button(AssistantWords.chooseAssistantApps.said) { choosingFrom = model.supervision.assistantAppChoices }
                    .buttonStyle(BoxButtonStyle())
                    .accessibilityIdentifier("choose-assistant-apps")
                    .disabled(model.chatPermissionBusy)
            }
        } else {
            let retry = if case .unavailable = permission { true } else { false }
            Button((retry ? AssistantWords.tryAgain : .checkChatPermission).said) { Task { await model.checkChatPermission() } }
                .buttonStyle(BoxButtonStyle())
                .accessibilityIdentifier("check-chat-permission")
                .disabled(model.chatPermissionBusy)
        }
    }

    /// The provider step as a person comes back to it from the assistant view, with the
    /// provider they asked for picked. A step asked for anew begins anew.
    private func back(to step: AssistantModel.KeyStep) -> some View {
        ProviderCard(providers: step.providers, picked: step.picked, again: true) { offered = $0 }.id(step.picked)
    }
}

private struct AssistantAppPicker: View {
    let choices: [AssistantAppChoice]
    let close: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var selected = Set(AssistantAccess.chat.apps)

    var body: some View {
        let requested = choices.filter { selected.contains($0.id) }
        let names = requested.map(\.name).formatted(.list(type: .and).locale(wording.locale))
        Bubble {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(choices) { choice in
                    let required = AssistantAccess.chat.apps.contains(choice.id)
                    let label = required ? AssistantWords.assistantAppRequired.said(["name": choice.name]) : choice.name
                    Toggle(label, isOn: Binding(get: { selected.contains(choice.id) }, set: { isOn in
                        if isOn { selected.insert(choice.id) } else { selected.remove(choice.id) }
                    }))
                    #if os(macOS)
                    .toggleStyle(.checkbox)
                    #endif
                    .tint(Token.on)
                    .disabled(required)
                    .accessibilityIdentifier(choice.id)
                }
                Text(AssistantWords.assistantAccessSelection.said(["apps": names]))
                    .fixedSize(horizontal: false, vertical: true)
                Flow(spacing: 8, lineSpacing: 8) {
                    Button(AssistantWords.requestSelectedAssistantApps.said) {
                        Task {
                            guard choices == model.supervision.assistantAppChoices else { return }
                            if await model.requestAssistantAppPermission(apps: requested.map(\.id)) { close() }
                        }
                    }
                    .buttonStyle(BoxButtonStyle(isMain: true))
                    .accessibilityIdentifier("request-selected-assistant-apps")
                    Button(AssistantWords.cancel.said, action: close).buttonStyle(BoxButtonStyle())
                }
            }
            .disabled(model.chatPermissionBusy)
        }
    }
}

/// The provider step: no provider is linked, so the main chat says so and offers the step. The
/// person picks a provider and types a key of their own into the protected field, the same one
/// a secret is typed into. What is typed lives in this view alone until the button hands it to
/// their vault; the app has no other field for a key. A person who linked a provider comes back
/// to this same step from the assistant view, for another key or another provider.
struct ProviderCard: View {
    let providers: [ProviderChoice]
    /// Whether the person came back to the step: a provider is linked, and the step can be put away.
    let again: Bool
    /// Called with the models that can be chosen once the provider is linked.
    let linked: ([ModelChoice]) -> Void
    @Environment(AssistantModel.self) private var model
    @State private var picked: ProviderChoice.ID?
    @State private var typed = ""
    @State private var linking = false

    init(providers: [ProviderChoice], picked: ProviderChoice.ID? = nil, again: Bool = false, linked: @escaping ([ModelChoice]) -> Void) {
        self.providers = providers
        self.again = again
        self.linked = linked
        _picked = State(initialValue: picked)
    }

    /// What the step says over its box. At the first start: that no provider is linked. To a
    /// person who came back: that the key typed takes the place of the one kept, or that a
    /// provider is to be chosen.
    static func says(of provider: ProviderChoice?, again: Bool) -> String {
        guard again, let provider else { return AssistantWords.needsProvider.said }
        return provider.isLinked ? AssistantWords.anotherKeyStep.said(["name": provider.name]) : AssistantWords.anotherProviderStep.said
    }

    /// What the step's button says: the provider is linked, or a linked one is given this key.
    static func button(for provider: ProviderChoice) -> String {
        (provider.isLinked ? AssistantWords.useKey : AssistantWords.linkProvider).said(["name": provider.name])
    }

    var body: some View {
        let provider = providers.first(where: { $0.id == picked }) ?? providers.first
        Bubble { MarkdownText(text: ProviderCard.says(of: provider, again: again)) }
        if let provider {
            CardBox(name: provider.name, note: AssistantWords.keptInVault.said) {
                if providers.count > 1 {
                    Flow(spacing: 8, lineSpacing: 8) {
                        ForEach(providers) { choice in
                            Button(choice.name) {
                                // A key typed for one provider is not handed to another.
                                if choice.id != provider.id { typed = "" }
                                picked = choice.id
                            }
                            .buttonStyle(BoxButtonStyle())
                            .opacity(choice.id == provider.id ? 1 : 0.6)
                            .accessibilityAddTraits(choice.id == provider.id ? .isSelected : [])
                        }
                    }
                }
                ProtectedField(typed: $typed, label: AssistantWords.providerKey.said(["name": provider.name])) { link(provider) }
                Flow(spacing: 8, lineSpacing: 8) {
                    Button(ProviderCard.button(for: provider)) { link(provider) }
                        .buttonStyle(BoxButtonStyle(isMain: true))
                        .disabled(typed.isEmpty || linking)
                    if again {
                        // Nothing that was typed outlives the step.
                        Button(AssistantWords.notNow.said) {
                            typed = ""
                            model.putAwayKeyStep()
                        }
                        .buttonStyle(BoxButtonStyle())
                    }
                }
                Fine(words: AssistantWords.secretFine.said)
            }
            .onDisappear { typed = "" }
        }
    }

    private func link(_ provider: ProviderChoice) {
        guard !typed.isEmpty, !linking else { return }
        // The field is emptied before the key leaves: it is held in one place at a time.
        let key = Secret(typed)
        typed = ""
        linking = true
        Task {
            if await model.link(provider.id, key: key) { linked(model.models) }
            linking = false
        }
    }
}

/// The models a linked provider offers, for the person to choose the one the main chat answers
/// on. They are the ones the person's server listed; nothing here names a model.
struct ModelCard: View {
    let models: [ModelChoice]
    /// The model the main chat answers on already, by its name, where the person came back to
    /// the provider step: their choice stands, and they can change it here.
    var answers: String?
    @Environment(AssistantModel.self) private var model
    @State private var chosen: ModelChoice?

    /// The providers the offered models are of, each once, in the order they are offered.
    static func providers(of models: [ModelChoice]) -> [String] {
        var named: [String] = []
        for choice in models where !named.contains(choice.provider) { named.append(choice.provider) }
        return named
    }

    /// What the box says beside its name: the chosen model's provider, or the one provider
    /// every offered model is of. Where models of several are offered and none is chosen yet,
    /// it names no provider: the menu tells them apart.
    static func note(chosen: ModelChoice?, among models: [ModelChoice]) -> String {
        if let chosen { return chosen.provider }
        let named = providers(of: models)
        return named.count == 1 ? named[0] : ""
    }

    var body: some View {
        let providers = ModelCard.providers(of: models)
        let named = chosen?.name ?? answers
        Bubble { MarkdownText(text: (answers == nil ? AssistantWords.needsModel : AssistantWords.keyTaken).said) }
        CardBox(name: AssistantWords.model.said, note: ModelCard.note(chosen: chosen, among: models)) {
            if let named { Settled(words: AssistantWords.answersOn.said(["model": named])) }
            Menu {
                if providers.count > 1 {
                    // Models of several providers stand under their provider's name.
                    ForEach(providers, id: \.self) { provider in
                        Section(provider) {
                            ForEach(models.filter { $0.provider == provider }) { pick($0) }
                        }
                    }
                } else {
                    ForEach(models) { pick($0) }
                }
            } label: {
                Text((named == nil ? AssistantWords.chooseModel : AssistantWords.change).said)
                    .font(.tilo(14, .medium)).foregroundStyle(Token.asst)
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
        }
    }

    private func pick(_ choice: ModelChoice) -> some View {
        Button(choice.name) {
            chosen = choice
            model.choose(choice.id)
        }
    }
}

/// A service the person named by its address: its name as it gives it, what it offers, and
/// the person's yes and no. Where it wants a sign-in, the yes goes on to the service's own page.
struct ConnectionCard: View {
    let offer: ConnectionOffer
    @Environment(AssistantModel.self) private var model
    @Environment(\.webAuthenticationSession) private var signInSheet

    var body: some View {
        Bubble { MarkdownText(text: offer.text) }
        CardBox(name: offer.name, note: offer.note) {
            switch offer.state {
            case .waiting:
                Flow(spacing: 8, lineSpacing: 8) {
                    Button(offer.yes) { Task { await authenticate() } }
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

    /// The person's yes. Where the service wants a sign-in, its own page opens in the system's
    /// sign-in sheet: nothing of the sign-in is typed into this app, and the address the sheet
    /// comes back with goes to the person's Tilo and nowhere else. A sheet the person closes
    /// leaves the card as it stands.
    private func authenticate() async {
        guard let page = await model.authenticate(offer.id) else { return }
        guard let returned = try? await signInSheet.authenticate(using: page, callback: .customScheme(SignInReturn.scheme),
                                                                 additionalHeaderFields: [:])
        else { return }
        await model.signedIn(offer.id, at: returned)
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
        // A card that stands under the assistant's own answer says nothing above its box.
        if !card.text.isEmpty { Bubble { MarkdownText(text: card.text) } }
        CardBox(name: card.automation.title,
                note: card.settled == .undone ? AssistantWords.eventRemoved.said : wording.state(card.automation)) {
            let tags = wording.cardTags(card.automation)
            if !tags.isEmpty {
                Flow(spacing: 8, lineSpacing: 6) {
                    ForEach(tags, id: \.self) { Tag(words: $0) }
                }
            }
            // What the event keeps, where its person asked to see it, and what became of a press.
            if let shown = card.shown { MarkdownText(text: shown) }
            if let settled = card.settled {
                Fine(words: (settled == .undone ? AssistantWords.eventUndone : AssistantWords.asksEachTime).said)
            }
            if let chosen = card.answer, let choice = card.choices.first(where: { $0.id == chosen }) {
                Fine(words: AssistantWords.youChose.said(["answer": choice.title]))
            } else if !card.choices.isEmpty {
                Flow(spacing: 8, lineSpacing: 8) {
                    ForEach(card.choices) { choice in
                        Button(choice.title) { model.answer(card.question, with: choice.id) }
                            .buttonStyle(BoxButtonStyle(isMain: choice.isMain))
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityLabel(AssistantWords.waitsForAnswer.said)
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
