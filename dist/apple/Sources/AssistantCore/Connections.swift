import Foundation

/// Where a person's browser comes back to after they signed in to a service: an address in a
/// scheme of this app's own, which no browser takes for the web. The system's sign-in sheet
/// catches it and hands it to this app alone.
public enum SignInReturn {
    public static let scheme = "so.tilo.assistant"
    public static let address = "so.tilo.assistant:/signed-in"
}

extension ConnectionOffer {
    /// What stands beside the service's name on its card: how many commands it offers, where
    /// it says so before anyone signed in; else where its person signs in.
    public var note: String {
        if let commands {
            return (needsSignIn ? AssistantWords.commandsAfter : .commandsCount).said(["count": String(commands)])
        }
        if needsSignIn, let signInAt, !signInAt.isEmpty { return AssistantWords.signInAt.said(["site": signInAt]) }
        return ""
    }

    /// What its yes says: the person signs in, or the service is only added.
    public var yes: String {
        (needsSignIn ? AssistantWords.authenticate : .add).said
    }
}
