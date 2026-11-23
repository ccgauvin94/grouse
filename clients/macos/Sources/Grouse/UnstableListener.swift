import Foundation
import GrouseCore

/// The core's UNSTABLE-channel listener: recipe-parameter and elicitation forms.
///
/// `GrouseUnstable` is a separate uniffi object from `Core` (its callbacks come from the
/// `_goose/unstable/*` shim), so it needs its own listener. Only the two form events are
/// implemented; every other callback comes from `UnstableListenerDefaults`. As with
/// `Listener`, the callbacks arrive on the core's runtime thread and are hopped onto the
/// main queue, where the published prompts live. The bridge is held weakly: it owns this
/// listener through `GrouseUnstable`, which it owns, so a strong capture would be a cycle.
final class UnstableListener: GrouseUnstableListener, @unchecked Sendable {
    private weak var bridge: CoreBridge?

    init(_ bridge: CoreBridge) {
        self.bridge = bridge
    }

    private func onMain(_ body: @escaping (CoreBridge) -> Void) {
        DispatchQueue.main.async { [weak bridge] in
            guard let bridge else { return }
            body(bridge)
        }
    }

    private static func ownerText(_ owner: SessionOwner) -> String {
        owner.label.isEmpty ? owner.kind : "\(owner.kind): \(owner.label)"
    }

    /// Both events now carry the owning wire and a REQUEST ID. The id is the only safe
    /// answer key: a form parked on the main wire and one from a roam peer can overlap,
    /// and a process-global "current form" would send credentials to the wrong server.
    ///
    /// Two overloads on purpose: the core evolved `owner` from a plain wire string to the
    /// `SessionOwner` record while the generated bindings were being regenerated across
    /// clients. Swift allows both, and whichever shape this build of uniffi emits is the
    /// one that satisfies the protocol requirement.
    func onRecipeParams(owner: String, requestId: String, parameters: String) {
        onMain { $0.handleRecipeParams(owner: owner, requestId: requestId, parameters: parameters) }
    }

    func onRecipeParams(owner: SessionOwner, requestId: String, parameters: String) {
        let text = Self.ownerText(owner)
        onMain { $0.handleRecipeParams(owner: text, requestId: requestId, parameters: parameters) }
    }

    func onElicitation(owner: String, requestId: String, schema: String) {
        onMain { $0.handleElicitation(owner: owner, requestId: requestId, schema: schema) }
    }

    func onElicitation(owner: SessionOwner, requestId: String, schema: String) {
        let text = Self.ownerText(owner)
        onMain { $0.handleElicitation(owner: text, requestId: requestId, schema: schema) }
    }
}
