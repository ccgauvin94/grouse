// SPDX-License-Identifier: AGPL-3.0-or-later

//! Peer-side permission and server-request responders. The registry keys
//! and rejection behavior live in `crate::spine::responders`.

use super::*;

impl RoamPeer {
    /// Answer the pending `session/request_permission` from this peer
    /// (CONTRACT §5; the spine routes `respond_permission` here when the peer
    /// owns the active session).
    pub fn respond_permission(
        &self,
        tool_call_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpError> {
        let responder = self
            .inner
            .lock()
            .pending_permission
            .remove(tool_call_id)
            .ok_or_else(|| AcpError::internal_error().data("no pending roam permission request"))?;
        responder
            .respond(crate::session::permission_response(outcome))
            .map_err(|error| {
                AcpError::internal_error().data(format!("roam permission respond: {error}"))
            })
    }

    /// A `session/request_permission` arrived: stash the responder and prompt
    /// the UI. The host emits only while this peer's session is displayed.
    pub(crate) fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        let request = crate::session::permission_request(&req);
        self.inner
            .lock()
            .pending_permission
            .insert(request.tool_call_id.clone(), responder);
        let key = self.active_session_id().unwrap_or_default();
        self.host.permission(&key, request);
    }

    /// Server→client requests the UI can answer (recipe params, elicitation).
    /// The same parked-responder seam as the spine: the core exposes
    /// `respond_recipe_params`/`respond_elicitation` regardless of which wire
    /// asked, so declaring the capability on a peer now means it works there.
    pub(crate) fn on_server_request(&self, method: &str, params: &Value, responder: Responder<Value>) {
        match method {
            "_goose/unstable/session/recipe/request-params" => {
                crate::spine::park_server_request(
                    PendingRequest::RecipeParams,
                    method,
                    params,
                    responder,
                );
            }
            "elicitation/create" => {
                crate::spine::park_server_request(
                    PendingRequest::Elicitation,
                    method,
                    params,
                    responder,
                );
            }
            _ => {
                let _ = responder.respond_with_error(
                    AcpError::method_not_found()
                        .data(format!("not supported by this client: {method}")),
                );
            }
        }
    }

}
