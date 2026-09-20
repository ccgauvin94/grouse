// SPDX-License-Identifier: AGPL-3.0-or-later
import QtQuick
import QtQuick.Controls as Controls
import QtWebEngine

/*
 * Inline host for one MCP App: a WebEngineView pointed at grouse's own
 * loopback bridge URL (Manager::appViewUrl), which serves the server-supplied
 * template WITH the host shim already injected — ui/initialize resolves,
 * ui/message posts back into the chat. The same URL feeds the external browser
 * when WebEngine is unavailable; this file only loads when it is.
 *
 * The document is untrusted (came over the wire): off-the-record profile, no
 * local storage, no downloads, dialogs refused.
 */
Item {
    id: root
    // The chat row's app key; the URL is the loopback bridge document for it
    // (registered on first request — and only requested when this view is
    // active, so hidden/collapsed bubbles don't start the bridge or pile up
    // tokens). Empty until set; view stays blank until then.
    property string appKey: ""
    property string url: appKey.length > 0 ? Mgr.appViewUrl(appKey) : ""
    implicitHeight: 420

    WebEngineProfile {
        id: appProfile
        offTheRecord: true
        // downloadRequested is a profile signal; cancel every one (the doc is
        // untrusted and has no business writing to disk).
        onDownloadRequested: (download) => download.cancel()
    }

    WebEngineView {
        id: view
        anchors.fill: parent
        profile: appProfile
        url: root.url.length > 0 ? root.url : "about:blank"
        settings.javascriptCanOpenWindows: false
        settings.localStorageEnabled: false
        // Server-supplied, untrusted document: block navigation to any other
        // origin (a crafted app must not browse the user out of the sandbox).
        // The bridge is ALWAYS the loopback origin — check that, not the full
        // URL (every registered app shares the one ephemeral port). In-page
        // fetch/XHR to it is not a navigation and stays allowed regardless.
        onNavigationRequested: (request) => {
            if (request.url.toString().indexOf("http://127.0.0.1:") !== 0)
                request.action = WebEngineNavigationRequest.IgnoreRequest;
        }
        onPermissionRequested: (permission) => permission.deny()
        onFeaturePermissionRequested: (securityOrigin, feature) => denyFeature(securityOrigin, feature)

        Controls.BusyIndicator {
            anchors.centerIn: parent
            running: view.loading
            visible: view.loading
        }
    }
}
