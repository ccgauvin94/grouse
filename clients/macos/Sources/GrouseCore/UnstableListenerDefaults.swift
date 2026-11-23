// No-op defaults for the UNSTABLE core listener (`GrouseUnstableListener`).
//
// The app only renders two of these events — recipe parameters and elicitation forms,
// both now keyed by (owner, requestId) — via `UnstableListener`. uniffi's generated Swift
// protocol has one REQUIRED method per event and no defaults, so without this extension a
// conformance would have to stub all 25 callbacks and every new core event would break the
// build on a Mac (where CI cannot compile it). This is the same trick as
// `ListenerDefaults.swift`; `check-bindings.py` checks BOTH protocols against these defaults.
//
// Hand-written, not regenerated. Keep it in step with the generated protocol; the checker
// compares full signatures, not just names, so a changed payload shows up as a failure.

import Foundation

public extension GrouseUnstableListener {
    func onExport(data: String) {}
    // Both owner shapes are supplied: the core moved from a plain wire string to the
    // `SessionOwner` record mid-change, and the generated Swift of a given build may
    // carry either. An overload that the protocol does not need is just dead code.
    func onRecipeParams(owner: String, requestId: String, parameters: String) {}
    func onRecipeParams(owner: SessionOwner, requestId: String, parameters: String) {}
    func onElicitation(owner: String, requestId: String, schema: String) {}
    func onElicitation(owner: SessionOwner, requestId: String, schema: String) {}
    func onCompactionStatus(message: String) {}
    func onUsage(used: Int64, size: Int64, cost: Double) {}
    func onMessageUsage(outputTokens: UInt64, elapsedMs: UInt64,
                        timeToFirstTokenMs: UInt64, cost: Double) {}
    func onAppResource(key: String, html: String) {}
    func onRecipes(recipes: String) {}
    func onSchedules(schedules: String) {}
    func onProjects(projects: String) {}
    func onSkills(skills: String) {}
    func onTools(sessionId: String, tools: String) {}
    func onExtensions(extensions: String) {}
    func onSessionExtensions(sessionId: String, extensions: String) {}
    func onExtensionCatalog(sessionId: String, extensions: String) {}
    func onConfigValue(key: String, value: String) {}
    func onSupportedModels(provider: String, models: String) {}
    func onProviders(providers: String) {}
    func onSessionProbe(sessionId: String, updatedAt: String, messageCount: Int64) {}
    func onContextReport(sessionId: String, report: String) {}
    func onToolResult(text: String, isError: Bool) {}
    func onDictationConfig(providers: String) {}
    func onTranscript(text: String) {}
    func onError(method: String, message: String) {}
}
