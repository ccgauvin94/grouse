// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hidden side channels: My Day/file host sessions, file ingest, advisor and label summarizers.
//! The facade, records and shared state stay in `lib.rs`; this module
//! contributes inherent `impl Core` methods only. Behaviour is unchanged.

use super::*;

impl Core {
    // -- My Day plumbing (`tasks.rs` holds the pure half) --------------------

    /// The hidden session the task calls run on, created on first use.
    ///
    /// Hidden so it never enters a session list, and `auto` because goose
    /// refuses app tool calls in every other mode. Nothing is ever prompted on
    /// it — it exists to host deterministic tool calls, so its context stays
    /// empty — which is why trading its mode away costs nothing.
    /// The hidden session a file fetch runs on for `owner`'s endpoint.
    ///
    /// Built exactly like the My Day task session (`_meta.hidden` + mode auto),
    /// because goose refuses a client-initiated `tools/call` on any session whose
    /// mode is not auto — with a message that reads like a permissions problem
    /// ("app tool calls require auto mode"). A download is not part of the
    /// conversation, so it must not borrow the conversation's mode.
    pub(crate) async fn file_wire(&self, owner: &Arc<dyn crate::session::ChatEndpoint>) -> Result<String, String> {
        let key = owner.label().unwrap_or_default();
        if let Some(wire) = self.inner.file_sessions.lock().get(&key).cloned() {
            return Ok(wire);
        }
        let purpose = format!("file:{key}");
        if !self.hidden_ready(&purpose) {
            return Err("the file host session failed moments ago; retrying shortly".to_string());
        }
        let cwd = self
            .inner
            .state
            .lock()
            .last_config
            .clone()
            .map(|c| c.cwd)
            .unwrap_or_default();
        let reply = match crate::session::request(
            owner.as_ref(),
            "session/new",
            crate::tasks::new_session_params(&cwd),
        )
        .await
        {
            Ok(reply) => reply,
            Err(error) => {
                self.hidden_failed(&purpose);
                return Err(error.to_string());
            }
        };
        let Some(wire) = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            self.hidden_failed(&purpose);
            return Err("session/new returned no session id".to_string());
        };
        if let Err(error) = crate::session::request(
            owner.as_ref(),
            "session/set_mode",
            crate::tasks::set_mode_params(&wire, "auto"),
        )
        .await
        {
            self.hidden_failed(&purpose);
            return Err(error.to_string());
        }
        self.inner
            .file_sessions
            .lock()
            .insert(key, wire.clone());
        self.hidden_created(&purpose);
        Ok(wire)
    }

    /// May a hidden session be created for `purpose` right now? See
    /// `CoreState::hidden_backoff`: after a failure, wait before minting another
    /// agent, and say so in the turn log so a burst is visible rather than
    /// inferred from a process count.
    pub(crate) fn hidden_ready(&self, purpose: &str) -> bool {
        let mut state = self.inner.state.lock();
        match state.hidden_backoff.get(purpose).copied() {
            Some(at) if at.elapsed() < HIDDEN_RETRY_AFTER => false,
            Some(_) => {
                state.hidden_backoff.remove(purpose);
                true
            }
            None => true,
        }
    }

    /// Record that a hidden session for `purpose` failed, so the next attempt
    /// waits instead of spawning another agent immediately.
    pub(crate) fn hidden_failed(&self, purpose: &str) {
        self.inner.state.lock().hidden_backoff.insert(
            purpose.to_string(),
            std::time::Instant::now(),
        );
        self.note(format!("hidden-session {purpose} failed; backing off"));
    }

    /// One line per creation, so "why are there N agents" is answerable from the
    /// turn log instead of a process count.
    pub(crate) fn hidden_created(&self, purpose: &str) {
        self.note(format!("hidden-session {purpose} created"));
    }

    pub(crate) async fn tasks_wire(&self) -> Result<(Arc<crate::spine::Conn>, String), String> {
        let conn = self
            .inner
            .conn
            .lock()
            .clone()
            .ok_or_else(|| "not connected".to_string())?;
        if let Some(wire) = self.inner.tasks_session.lock().clone() {
            return Ok((conn, wire));
        }
        if !self.hidden_ready("tasks") {
            return Err("the task host session failed moments ago; retrying shortly".to_string());
        }
        let cwd = self
            .inner
            .state
            .lock()
            .last_config
            .clone()
            .map(|c| c.cwd)
            .unwrap_or_default();
        let reply = match crate::session::request(
            conn.as_ref(),
            "session/new",
            crate::tasks::new_session_params(&cwd),
        )
        .await
        {
            Ok(reply) => reply,
            Err(error) => {
                self.hidden_failed("tasks");
                return Err(error.to_string());
            }
        };
        let Some(wire) = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            self.hidden_failed("tasks");
            return Err("session/new returned no session id".to_string());
        };
        // Not fatal: the tool call that follows reports the real failure, and
        // this alert is the only place that failure's CAUSE is named.
        if let Err(e) = crate::session::request(
            conn.as_ref(),
            "session/set_mode",
            crate::tasks::set_mode_params(&wire, "auto"),
        )
        .await
        {
            self.alert(format!("My Day: task session could not be set to auto ({e})"));
        }
        *self.inner.tasks_session.lock() = Some(wire.clone());
        self.hidden_created("tasks");
        Ok((conn, wire))
    }

    // -- File ingest plumbing (`attachments.rs` holds the pure half) --------

    /// One deterministic attachments tool call on `owner`'s hidden `auto`
    /// session. Uploads and downloads share this seam: a roam chat's bytes live
    /// on its peer, so the call must never fall back to the main wire.
    pub(crate) async fn attachment_call<F: Fn(&str) -> Value>(
        &self,
        owner: &Arc<dyn crate::session::ChatEndpoint>,
        build: F,
    ) -> Result<Value, String> {
        let drop_wire = || {
            self.inner
                .file_sessions
                .lock()
                .remove(&owner.label().unwrap_or_default());
        };
        let wire = match self.file_wire(owner).await {
            Ok(wire) => wire,
            Err(e) => {
                drop_wire();
                return Err(e);
            }
        };
        let reply =
            crate::session::request(owner.as_ref(), "_goose/unstable/tools/call", build(&wire))
                .await;
        match reply {
            Ok(reply) => {
                if crate::tasks::is_error(&reply) {
                    drop_wire();
                    Err(crate::tasks::content_text(&reply))
                } else {
                    Ok(reply)
                }
            }
            Err(e) => {
                drop_wire();
                Err(e.to_string())
            }
        }
    }

    /// Pull one file: windows until `total`, then land it atomically.
    ///
    /// The owner is the chat's OWN endpoint, not the main wire: a file shared in
    /// a roam chat lives on that peer's filesystem, and `attachments__attachment_read`
    /// has to run there.
    pub(crate) async fn download_one(
        &self,
        display: &str,
        path: &str,
        name: &str,
        sha256: &str,
    ) -> Result<(PathBuf, u64), String> {
        let local = self.local_file_path(display, path, name, sha256);
        if !path_within_files(self.inner.cache.dir(), &local) {
            return Err("refusing a file path outside the cache".to_string());
        }
        // A finished copy is the whole answer: the name is content-addressed, so
        // an existing file IS this file -- but only if its bytes still hash to
        // the digest that named it.
        if let Ok(meta) = std::fs::symlink_metadata(&local) {
            if meta.file_type().is_symlink() {
                return Err("refusing a cached symlink".to_string());
            }
            if meta.len() > 0 {
                verify_cached_digest(&local, sha256)?;
                return Ok((local, meta.len()));
            }
        }
        let owner = self
            .owner_for(display)
            .ok_or_else(|| "no connection for that chat".to_string())?;
        // Deterministic calls run on the endpoint's hidden session, not the chat's
        // (see `file_wire`): the chat may be in approve mode, which refuses them.
        let wire = match self.file_wire(&owner).await {
            Ok(wire) => wire,
            Err(e) => {
                self.inner
                    .file_sessions
                    .lock()
                    .remove(&owner.label().unwrap_or_default());
                return Err(e);
            }
        };
        let mut data: Vec<u8> = Vec::new();
        loop {
            let params = crate::attachments::read_params(
                &wire,
                path,
                data.len() as u64,
                crate::attachments::DOWNLOAD_CHUNK,
            );
            // The shared 30 s control-call policy: a peer that stalls after one
            // chunk must fail (and emit FileEvent::Failed) instead of parking
            // this task forever.
            let reply =
                crate::session::request(owner.as_ref(), "_goose/unstable/tools/call", params)
                    .await
                    .map_err(|e| e.to_string())?;
            if crate::tasks::is_error(&reply) {
                // The hidden session is the usual casualty of a server restart or
                // an evicted session: drop it so the next tap rebuilds it.
                self.inner
                    .file_sessions
                    .lock()
                    .remove(&owner.label().unwrap_or_default());
                return Err(crate::tasks::content_text(&reply));
            }
            let payload = tool_json_payload(&reply)
                .ok_or_else(|| "the server returned no chunk".to_string())?;
            let chunk = crate::attachments::parse_read(&payload)?;
            // Appending by offset: a window out of place would silently reorder
            // the file, so refuse instead of assembling garbage.
            if chunk.offset != data.len() as u64 {
                return Err(format!(
                    "chunk arrived at offset {} while {} bytes were held",
                    chunk.offset,
                    data.len()
                ));
            }
            data.extend_from_slice(&chunk.data);
            if data.len() as u64 > crate::attachments::MAX_BYTES {
                return Err(format!(
                    "file exceeds the {} byte limit",
                    crate::attachments::MAX_BYTES
                ));
            }
            self.inner
                .listener
                .on_file(crate::attachments::FileEvent::Progress {
                    path: path.to_string(),
                    name: name.to_string(),
                    received: data.len() as u64,
                    total: chunk.total,
                    // Same DISPLAY id as Saved/Failed for this one fetch.
                    session_id: display.to_string(),
                    request_id: String::new(),
                });
            if chunk.data.is_empty() || data.len() as u64 >= chunk.total {
                break;
            }
        }
        // Verify the digest and land atomically. A mismatch returns Err here,
        // which `fetch_file` turns into FileEvent::Failed with no file on disk.
        let local = land_download(self.inner.cache.dir(), display, path, name, sha256, &data)?;
        let bytes = data.len() as u64;
        Ok((local, bytes))
    }

    /// Where a fetched file lives: content-addressed by a CANONICAL digest
    /// when the item knows one, else by the hashed display+path key.
    pub(crate) fn local_file_path(&self, display: &str, path: &str, name: &str, sha256: &str) -> PathBuf {
        file_cache_path(self.inner.cache.dir(), display, path, name, sha256)
    }

    /// Upload one file: begin, chunk, commit. Returns where the server put it.
    pub(crate) async fn upload_one(
        &self,
        owner: &Arc<dyn crate::session::ChatEndpoint>,
        file: Arc<dyn FileSource>,
    ) -> Result<crate::attachments::UploadedFile, String> {
        use base64::Engine as _;

        // The name is carried through VERBATIM: it is the user's, and it is what
        // the transcript captions and the link shows. The server keeps a
        // sanitised name for the path only (attachments-mcp rule 5).
        let name = file.name();
        let mime = file.mime();
        let size = file.size();
        if size > crate::attachments::MAX_BYTES {
            return Err(format!(
                "{} bytes exceeds the {} byte limit",
                size,
                crate::attachments::MAX_BYTES
            ));
        }

        let begun = self
            .attachment_call(owner, |wire| {
                crate::attachments::begin_params(wire, &name, &mime, size)
            })
            .await?;
        let upload_id = tool_json_field(&begun, "uploadId")
            .ok_or_else(|| "the server returned no uploadId".to_string())?;

        for (offset, len) in
            crate::attachments::chunk_plan(size, crate::attachments::CHUNK_BYTES)
        {
            let bytes = file.read(offset, len as u32);
            // A short read is the app failing to produce what it promised.
            // Refuse rather than upload a truncated file that commits clean.
            if bytes.len() != len {
                return Err(format!(
                    "read {} of {} bytes at offset {offset}",
                    bytes.len(),
                    len
                ));
            }
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            self.attachment_call(owner, |wire| {
                crate::attachments::chunk_params(wire, &upload_id, offset, &data)
            })
            .await?;
        }

        let committed = self
            .attachment_call(owner, |wire| {
                crate::attachments::commit_params(wire, &upload_id)
            })
            .await?;
        let blob = tool_json_payload(&committed)
            .ok_or_else(|| "the server returned no commit result".to_string())?;
        let path = blob
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "the commit result had no path".to_string())?
            .to_string();
        Ok(crate::attachments::UploadedFile {
            name: blob
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string(),
            mime: blob
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or(&mime)
                .to_string(),
            bytes: blob.get("bytes").and_then(Value::as_u64).unwrap_or(size),
            path,
            sha256: blob
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Review the turn that just ended, if this is a dev chat and the advisor is on.
    ///
    /// GLOBAL switch, DEV chats only: the whole point is that a coding session gets a
    /// second opinion without per-chat bookkeeping. Cheap to decline — two state reads —
    /// because this runs after every turn of every chat.
    pub(crate) fn spawn_advisor_review(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
    ) {
        let (enabled, is_dev, run_ids) = {
            let state = self.inner.state.lock();
            (
                state.advisor,
                state
                    .session_modes
                    .get(&key)
                    .is_some_and(|mode| mode == crate::session::DEV_MODE),
                // A CLONE: the summariser consumes this list a moment later.
                state.turn_runs.get(&key).cloned().unwrap_or_default(),
            )
        };
        if !enabled || !is_dev || run_ids.is_empty() {
            return;
        }
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            core.review_turn(owner, key, run_ids).await;
        });
    }

    /// The review itself: one ordinary model call on the hidden session, then
    /// truncate it back to empty so it cannot accumulate a conversation.
    ///
    /// Failure is SILENT by design. A review is a courtesy; a broken one must never
    /// surface as an error in a chat that did nothing wrong, and the cool-off in
    /// `hidden_failed` keeps it from costing a session per turn.
    pub(crate) async fn review_turn(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
        run_ids: Vec<String>,
    ) {
        let slot = owner.label().unwrap_or_default();
        // One review per endpoint at a time: the reply is collected by position, so
        // two concurrent prompts on the same hidden session would cross.
        {
            let mut state = self.inner.state.lock();
            if !state.summarizer_busy.insert(slot.clone()) {
                return;
            }
        }
        self.review_turn_serial(owner, key, run_ids, slot.clone()).await;
        self.inner.state.lock().summarizer_busy.remove(&slot);
    }

    pub(crate) async fn review_turn_serial(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
        run_ids: Vec<String>,
        slot: String,
    ) {
        let store = self.store_for(&key);
        let calls = store.run_calls(&run_ids);
        if calls.is_empty() {
            self.inner.state.lock().summarizer_busy.remove(&slot);
            return;
        }
        // What the user asked, because "unfinished work" is measured against it.
        let ask = store
            .transcript()
            .into_iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content)
            .unwrap_or_default();
        let purpose = format!("advisor:{slot}");
        let Some((hidden, wire)) = self.hidden_session(&owner, &key, &slot, &purpose).await
        else {
            return;
        };
        self.inner
            .state
            .lock()
            .summarizer_ids
            .insert(hidden.clone());
        let params = crate::session::prompt_params(
            &Prompt {
                blocks: vec![PromptBlock::Text {
                    text: crate::advisor::prompt(&ask, &calls),
                }],
            },
            &wire,
        );
        let sent = crate::session::request(owner.as_ref(), "session/prompt", params)
            .await
            .is_ok();
        let text = crate::session::DispatchHost::take_summarizer_reply(self, &hidden);
        let truncated = crate::session::request(
            owner.as_ref(),
            "_goose/unstable/session/conversation/truncate",
            json!({ "sessionId": &wire, "truncateFrom": 0 }),
        )
        .await
        .is_ok();
        {
            let mut state = self.inner.state.lock();
            state.summarizer_ids.remove(&hidden);
            if !sent || !truncated {
                state.summarizer_sessions.remove(&slot);
                state.summarizer_replies.remove(&hidden);
            }
        }
        if !sent || !truncated {
            self.hidden_failed(&purpose);
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() || crate::advisor::nothing_material(&text) {
            return;
        }
        self.inner
            .listener
            .on_advisor_report(key, text);
    }

    /// Ask the model for a one-line label per tool run of the turn that just
    /// ended. The provisional label (the run's own thinking, condensed by the
    /// store) stands wherever the model does not answer, so this is pure
    /// improvement — and it runs after the turn, so it never delays a reply.
    pub(crate) fn spawn_run_summaries(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
    ) {
        let (enabled, run_ids, already) = {
            let mut state = self.inner.state.lock();
            let ids = state.turn_runs.remove(&key).unwrap_or_default();
            // Steps the live passes already labelled while the turn ran are done;
            // the rest of the turn (including the step that was still open) is
            // this pass's work. The bookkeeping is per turn.
            let already = std::mem::take(&mut state.labeled_runs);
            (state.run_summaries, ids, already)
        };
        if !enabled {
            return;
        }
        let store = self.store_for(&key);
        let last_run = run_ids.last().cloned();
        let runs: Vec<(String, String, Vec<String>)> = run_ids
            .into_iter()
            // A step labelled mid-turn was labelled from the calls it had THEN;
            // the last one may have gained calls since, so it is rewritten once
            // more now that the turn is closed. The rest are done.
            .filter(|id| Some(id) == last_run.as_ref() || !already.contains(id))
            // One step per thinking burst means a long loop is many labels; a
            // turn with an unusually long loop is capped so the request stays a
            // short one. Steps past the cap keep their provisional label (the
            // thinking's own first line) rather than waiting for a rewrite.
            .take(MAX_SUMMARIZED_STEPS)
            .filter_map(|id| {
                store
                    .run_framing(&id)
                    .map(|(thought, tools)| (id, thought, tools))
            })
            .collect();
        if runs.is_empty() {
            return;
        }
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            core.rewrite_run_labels(owner, key, runs).await;
        });
    }

    /// A step just closed: label it NOW, while the turn is still running.
    ///
    /// Debounced by a generation counter, so a burst of steps (a fast tool loop)
    /// becomes one request per quiet gap instead of one per step, and only the
    /// steps that are actually finished are sent — the one running keeps its
    /// provisional label until the turn ends.
    pub(crate) fn schedule_step_labels(&self, key: &str) {
        let gen = {
            let mut state = self.inner.state.lock();
            if !state.run_summaries {
                return;
            }
            state.label_gen += 1;
            state.label_gen
        };
        let core = self.clone();
        let key = key.to_string();
        crate::roam::runtime().spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            core.label_closed_steps(gen, key).await;
        });
    }

    pub(crate) async fn label_closed_steps(&self, gen: u64, key: String) {
        let (enabled, ids, labeled) = {
            let state = self.inner.state.lock();
            if state.label_gen != gen {
                return; // a newer step superseded this pass
            }
            (
                state.run_summaries,
                state.turn_runs.get(&key).cloned().unwrap_or_default(),
                state.labeled_runs.clone(),
            )
        };
        if !enabled {
            return;
        }
        let Some(owner) = self.owner_for(&key) else {
            return;
        };
        let store = self.store_for(&key);
        let runs: Vec<(String, String, Vec<String>)> = unlabeled_steps(&ids, &labeled)
            .into_iter()
            .filter_map(|id| {
                store
                    .run_framing(&id)
                    .map(|(thought, tools)| (id, thought, tools))
            })
            .collect();
        if runs.is_empty() {
            return;
        }
        self.rewrite_run_labels(owner, key, runs).await;
    }

    /// Pin the operator's advisor model on a freshly minted hidden session, if the
    /// server advertises one (see [`ADVISOR_MODEL_KEY`]).
    ///
    /// Best effort, and LOUD on refusal — unlike the review itself. A review that fails
    /// is nobody's problem, but an env var that names a model the provider does not serve
    /// would otherwise make the advisor silently never work again (the cool-off then
    /// stops even the retries that would have shown it).
    pub(crate) async fn pin_advisor_model(
        &self,
        owner: &Arc<dyn crate::session::ChatEndpoint>,
        wire: &str,
    ) {
        let provider = self.read_config(owner, ADVISOR_PROVIDER_KEY).await;
        let model = self.read_config(owner, ADVISOR_MODEL_KEY).await;
        for params in crate::advisor::model_pin_params(wire, provider.as_deref(), model.as_deref())
        {
            if let Err(error) =
                crate::session::request(owner.as_ref(), "session/set_config_option", params.clone())
                    .await
            {
                eprintln!(
                    "grouse-core: advisor model pin refused ({}) — {}",
                    params.get("configId").and_then(Value::as_str).unwrap_or("?"),
                    error
                );
            }
        }
    }

    /// A non-secret config value from the SERVER (`_goose/unstable/config/read`), which
    /// checks the server process environment before its config.yaml. `None` when the key
    /// is absent — which is the normal case, not an error.
    pub(crate) async fn read_config(
        &self,
        owner: &Arc<dyn crate::session::ChatEndpoint>,
        key: &str,
    ) -> Option<String> {
        let reply = crate::session::request(
            owner.as_ref(),
            "_goose/unstable/config/read",
            json!({ "key": key }),
        )
        .await
        .ok()?;
        reply
            .get("value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// The hidden session a side-channel model call runs on, minted on first use.
    ///
    /// `_meta.hidden` keeps it out of every session list, and it lives on the CHAT'S
    /// OWN connection so the call cannot cross transports. `slot` is the per-endpoint
    /// key it is filed under (one hidden session per connection), `purpose` the
    /// cool-off label. `None` means "do not try now": the path failed recently, so a
    /// session minted here would be another agent for nothing.
    ///
    /// The caller must register the returned display id in `summarizer_ids` before
    /// prompting — that is what routes its chunks away from every client store.
    pub(crate) async fn hidden_session(
        &self,
        owner: &Arc<dyn crate::session::ChatEndpoint>,
        key: &str,
        slot: &str,
        purpose: &str,
    ) -> Option<(String, String)> {
        if let Some(pair) = self
            .inner
            .state
            .lock()
            .summarizer_sessions
            .get(slot)
            .cloned()
        {
            return Some(pair);
        }
        if !self.hidden_ready(purpose) {
            return None;
        }
        let cwd = owner
            .session_cwd(key)
            .filter(|cwd| !cwd.is_empty())
            .unwrap_or_else(|| {
                if owner.label().is_none() {
                    self.resolve_cwd(key)
                } else {
                    String::new()
                }
            });
        let params = json!({
            "cwd": cwd,
            "mcpServers": [],
            "_meta": { "hidden": true },
        });
        let Ok(reply) = crate::session::request(owner.as_ref(), "session/new", params).await
        else {
            self.hidden_failed(purpose);
            return None;
        };
        let Some(wire) = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            self.hidden_failed(purpose);
            return None;
        };
        let hidden = crate::session::display_id(owner.label().as_deref(), &wire);
        self.inner
            .state
            .lock()
            .summarizer_sessions
            .insert(slot.to_string(), (hidden.clone(), wire.clone()));
        self.hidden_created(purpose);
        // Only the advisor honours the operator's model: the summarizer's labels are a
        // few words and should not change because someone picked a reviewer.
        if purpose.starts_with("advisor") {
            self.pin_advisor_model(owner, &wire).await;
        }
        Some((hidden, wire))
    }

    /// One hidden session on the CHAT'S OWN connection (main or peer). Its
    /// `session/update` chunks are intercepted centrally by `session::dispatch`
    /// via the summarizer ids, so they never touch a client store.
    ///
    /// Serialised per endpoint: a live pass and the end-of-turn pass would
    /// otherwise prompt the SAME hidden session at once and apply each other's
    /// labels (the reply is matched to the request by position).
    pub(crate) async fn rewrite_run_labels(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
        runs: Vec<(String, String, Vec<String>)>,
    ) {
        let slot = owner.label().unwrap_or_default();
        if !self.inner.state.lock().summarizer_busy.insert(slot.clone()) {
            return; // the pass in flight will cover these at the turn's end
        }
        self.rewrite_run_labels_serial(owner, key, runs, slot.clone())
            .await;
        self.inner.state.lock().summarizer_busy.remove(&slot);
    }

    pub(crate) async fn rewrite_run_labels_serial(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
        runs: Vec<(String, String, Vec<String>)>,
        slot: String,
    ) {
        let purpose = format!("labels:{slot}");
        let Some((hidden, wire)) = self.hidden_session(&owner, &key, &slot, &purpose).await
        else {
            return;
        };
        self.inner
            .state
            .lock()
            .summarizer_ids
            .insert(hidden.clone());

        let body = crate::summarize::prompt(
            &runs
                .iter()
                .map(|(_, t, tools)| (t.clone(), tools.clone()))
                .collect::<Vec<_>>(),
        );
        let params = crate::session::prompt_params(
            &Prompt {
                blocks: vec![PromptBlock::Text { text: body }],
            },
            &wire,
        );
        let sent = crate::session::request(owner.as_ref(), "session/prompt", params)
            .await
            .is_ok();
        let text = crate::session::DispatchHost::take_summarizer_reply(self, &hidden);
        // Empty the hidden session: its context must not grow turn on turn.
        let truncated = crate::session::request(
            owner.as_ref(),
            "_goose/unstable/session/conversation/truncate",
            json!({ "sessionId": &wire, "truncateFrom": 0 }),
        )
        .await
        .is_ok();
        {
            let mut state = self.inner.state.lock();
            state.summarizer_ids.remove(&hidden);
            if !sent || !truncated {
                // The hidden session is unusable (the link replaced it, or its
                // context could not be emptied): recreate it next turn — after a
                // cool-off, or a path that keeps failing mints one per pass.
                state.summarizer_sessions.remove(&slot);
                state.summarizer_replies.remove(&hidden);
            }
        }
        if !sent || !truncated {
            self.hidden_failed(&purpose);
            return;
        }
        let store = self.store_for(&key);
        let mut applied = Vec::new();
        for (n, label) in crate::summarize::parse_labels(&text) {
            if let Some((id, _, _)) = runs.get(n - 1) {
                store.set_run_summary(id, &label);
                applied.push(id.clone());
            }
        }
        // Remember what carries a model label so a later pass of the same turn
        // does not ask for it again.
        {
            let mut state = self.inner.state.lock();
            for id in applied {
                state.labeled_runs.insert(id);
            }
        }
    }

    pub(crate) fn spawn_set_config(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        session_id: String,
        config_id: String,
        value: String,
    ) {
        let core = self.clone();
        let key = session_id.clone();
        crate::roam::runtime().spawn(async move {
            let params = json!({
                "sessionId": owner.wire_id(&session_id),
                "configId": config_id,
                "value": value,
            });
            if let Ok(reply) =
                crate::session::request(owner.as_ref(), "session/set_config_option", params).await
            {
                let options = crate::session::parse_config_options(&reply);
                core.on_config_reply(options);
                core.refresh_stamp(&key, false);
            } else {
                // A config pick that never lands cannot be silent: the toggle
                // would just keep showing the old value and the user has no
                // way to tell "slow" from "refused" (roam provider switching
                // silently no-oped forever). Emit an advisory — NOT
                // ConnectionStatus::Error — so the status line explains it
                // while the connection stays up.
                core.alert(format!(
                    "{config_id} change didn't reach the session ({key})"
                ));
            }
        });
    }
}
