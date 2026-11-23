// Default (no-op) implementations for every CoreListener callback.
//
// WHY THIS FILE EXISTS — a finding from generating the Swift bindings (2026-11-18):
// uniffi emits `public protocol CoreListener` with one requirement per event and NO defaults, so
// every conforming type in the app would have to stub every event, and each new one
// the core gains would break the build in every conformance. The Android and desktop
// clients both hit this (Kotlin needs every member; the C ABI has a whole callback table
// of nulls), and it is worse in Swift because a UI type would carry the noise.
//
// A protocol extension solves it once: a conformance implements what it cares about and
// inherits the rest. Dispatch is still dynamic — if a type implements a method, Swift's
// witness table uses the type's own implementation, not this one.
//
// This is hand-written, so it is NOT regenerated: keep it in step with the generated
// protocol. The compiler enforces that direction (a new requirement with no default is
// still an error at the conformance), so drift here shows up as a build failure in the
// app, not as a silent drop.
//
// (An app target would move this file next to its own listener; it is here so the spike
// and the future app can share the same omission-free starting point.)

import Foundation

public extension CoreListener {
    func onStatus(status: ConnectionStatus) {}
    func onSessions(sessions: [SessionSummary]) {}
    func onItem(op: TranscriptOp) {}
    func onUsage(used: Int64, size: Int64, cost: Double, currency: String) {}
    func onRunEnded(sessionId: String, stopReason: String) {}
    func onThreadTimeline(label: String, rows: [TimelineRow]) {}
    func onSensitivity(data: String) {}
    func onSpeak(text: String) {}
    func onConfig(options: [ConfigOption]) {}
    func onPermissionRequest(request: PermissionRequest) {}
    func onSessionTouched(sessionId: String, title: String, updatedAt: String) {}
    func onProjects(projects: [ProjectSummary]) {}
    func onRoamPeerStatus(label: String, status: String) {}
    func onRoamSessions(label: String, sessions: [SessionSummary]) {}
    func onPeerNewSession(label: String, sessionId: String) {}
    func onActiveRun(sessionId: String, runId: String) {}
    func onCommands(commands: [String]) {}
    func onSessionOpenFailed(sessionId: String, message: String) {}
    func onSessionOpened(sessionId: String) {}
    func onTasks(tasks: String) {}
    func onThreadReady(label: String, sessionId: String) {}
    func onReplayProgress(sessionId: String, loaded: Int64, total: Int64, done: Bool) {}
    func onFile(event: FileEvent) {}
    func onAdvisorReport(sessionId: String, text: String) {}
}
