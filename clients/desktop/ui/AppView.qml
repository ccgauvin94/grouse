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
    // Not a plain binding: appViewUrl() only returns a URL once the template is
    // cached, and for a freshly pinned app that can lag the pane's creation.
    // refreshUrl() is retried until it resolves (retryUrl).
    property string url: ""
    function refreshUrl() { url = appKey.length > 0 ? Mgr.appViewUrl(appKey) : "" }
    onAppKeyChanged: refreshUrl()
    Component.onCompleted: refreshUrl()
    Timer {
        id: retryUrl
        interval: 500
        repeat: true
        running: root.appKey.length > 0 && root.url.length === 0
        onTriggered: root.refreshUrl()
    }
    // The app's own content height, measured in-page (see measure()). 0 until the
    // first successful read, so the frame starts at a sane fallback rather than
    // collapsing to nothing while the document loads.
    property int measuredHeight: 0
    property int stableTicks: 0
    implicitHeight: measuredHeight > 0 ? measuredHeight : 360

    /// Read the guest's natural height. The MCP-App template reports it through
    /// `ui/notifications/size-changed` (the shim stashes it on <html> as
    /// data-grouse-height); fall back to the document's scrollHeight for a
    /// template that never reports one. Polled briefly until it settles so a
    /// chart/canvas that paints late still gets measured.
    function measure() {
        if (view.loading || root.url.length === 0)
            return;
        view.runJavaScript(
            "(function(){var d=document.documentElement,b=document.body;" +
            "var h=parseInt(d&&d.getAttribute('data-grouse-height'),10);" +
            "if(!h||isNaN(h))h=Math.max(d?d.scrollHeight:0,b?b.scrollHeight:0);" +
            "return h||0;})()",
            function(res) {
                if (typeof res !== "number" || res <= 0)
                    return;
                var h = Math.min(Math.max(res, 160), 4000);
                if (h === root.measuredHeight) {
                    root.stableTicks++;
                } else {
                    root.stableTicks = 0;
                    root.measuredHeight = h;
                }
                if (root.stableTicks >= 3)
                    poll.stop();
            });
    }

    Timer {
        id: poll
        interval: 600
        repeat: true
        onTriggered: root.measure()
    }

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
        // Re-measure from scratch on every load (a re-registered template can
        // change height), then stop once the value has been stable for a bit.
        onLoadingChanged: {
            if (loadRequest.status === WebEngineView.LoadSucceededStatus) {
                root.stableTicks = 0;
                poll.restart();
            }
        }
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
        Controls.Label {
            anchors.centerIn: parent
            visible: !view.loading && root.url.length === 0
            text: qsTr("Preparing app…")
            opacity: 0.7
        }
    }
}
