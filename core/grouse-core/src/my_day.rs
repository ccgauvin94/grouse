// SPDX-License-Identifier: AGPL-3.0-or-later

//! My Day board: task lists, thread map, thread conversations and their notes.
//! The facade, records and shared state stay in `lib.rs`; this module
//! contributes inherent `impl Core` methods only. Behaviour is unchanged.

use super::*;

impl Core {
    /// One deterministic tool call. Any failure drops the cached hidden session:
    /// a refusal is what an evicted or restarted session looks like, and
    /// recreating it is cheaper than diagnosing which.
    pub(crate) async fn tasks_call<F: Fn(&str) -> Value>(&self, build: F) -> Result<String, String> {
        let (conn, wire) = self.tasks_wire().await?;
        let reply =
            crate::session::request(conn.as_ref(), "_goose/unstable/tools/call", build(&wire))
                .await;
        match reply {
            Ok(reply) => {
                let text = crate::tasks::content_text(&reply);
                if crate::tasks::is_error(&reply) {
                    *self.inner.tasks_session.lock() = None;
                    self.hidden_failed("tasks");
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            Err(e) => {
                *self.inner.tasks_session.lock() = None;
                self.hidden_failed("tasks");
                Err(e.to_string())
            }
        }
    }

    /// All three lists in one `on_tasks`. Sent together because the tab paints them
    /// together; separate callbacks would let it show a half-updated set.
    ///
    /// The same document carries the thread map, and a refresh is also the moment to
    /// mint the conversations a new thread has not got yet — see `threads_sync`.
    pub(crate) async fn tasks_refresh_now(&self) {
        // One refresh at a time: this path creates thread conversations, and a
        // concurrent second pass would create them again (see the field's comment).
        let _one_at_a_time = self.inner.tasks_refresh_lock.lock().await;
        let open = self
            .tasks_call(|wire| crate::tasks::open_params(wire, 60))
            .await;
        let done = self
            .tasks_call(|wire| crate::tasks::done_params(wire, 20))
            .await;
        let tasks = self
            .tasks_call(|wire| crate::tasks::task_params(wire, 20))
            .await;
        let labels = crate::tasks::labels(
            open.as_deref().ok(),
            tasks.as_deref().ok(),
        );
        let notes = crate::tasks::thread_notes(
            open.as_deref().ok(),
            tasks.as_deref().ok(),
        );
        if !notes.is_empty() {
            *self.inner.last_notes.lock() = notes.clone();
        }
        let (map, mut error) = self.threads_read().await;
        // Paint first: the lists are the tab, and a first run has several conversations
        // to create. The re-emit below carries them.
        let envelope = self.emit_tasks(&open, &done, &tasks, &map, error.clone());
        self.save_my_day(&envelope);
        if self.threads_sync(&labels, &notes).await {
            let (map, err) = self.threads_read().await;
            error = err.or(error);
            let envelope = self.emit_tasks(&open, &done, &tasks, &map, error);
            self.save_my_day(&envelope);
        }
    }

    /// Remember a complete board so the next cold open paints instead of spinning.
    ///
    /// Gated on completeness rather than on "an envelope arrived": a refresh whose
    /// `open` call failed still produces a document (with `errors.open`), and
    /// caching that would replay an empty board as if the server had said so.
    pub(crate) fn save_my_day(&self, envelope: &str) {
        self.inner
            .cache
            .save_tasks(envelope, crate::tasks::envelope_is_complete(envelope));
    }

    pub(crate) fn emit_tasks(
        &self,
        open: &Result<String, String>,
        done: &Result<String, String>,
        tasks: &Result<String, String>,
        threads: &std::collections::BTreeMap<String, String>,
        thread_error: Option<String>,
    ) -> String {
        let envelope = crate::tasks::envelope_threads(
            open.clone(),
            done.clone(),
            tasks.clone(),
            threads,
            thread_error,
        );
        self.inner.listener.on_tasks(envelope.clone());
        envelope
    }

    /// Read the label -> conversation map, replacing the cache when the server answers.
    ///
    /// A failed read keeps the previous map: the map is why a tap opens the right chat,
    /// and a transient refusal must not make every thread look like it has none (which
    /// is what mints duplicates).
    pub(crate) async fn threads_read(
        &self,
    ) -> (std::collections::BTreeMap<String, String>, Option<String>) {
        match self
            .tasks_call(crate::tasks::thread_map_params)
            .await
        {
            Ok(text) => {
                let map = crate::tasks::parse_threads(&text);
                *self.inner.thread_map.lock() = map.clone();
                (map, None)
            }
            Err(e) => {
                let cached = self.inner.thread_map.lock().clone();
                (cached, Some(e))
            }
        }
    }

    /// Mint the conversation for every label that has none. Returns whether anything
    /// was created, so the caller re-emits only when the answer moved.
    ///
    /// BOUNDED per refresh (`MAX_NEW_THREADS`): a board with twenty new labels must not
    /// turn one refresh into sixty round trips, and each creation is three (session/new,
    /// rename, link). The rest converge on the following refreshes.
    pub(crate) async fn threads_sync(
        &self,
        labels: &[String],
        notes: &std::collections::BTreeMap<String, Vec<crate::tasks::ContextNote>>,
    ) -> bool {
        const MAX_NEW_THREADS: usize = 4;
        let mut created = false;
        let mut tried = 0usize;
        for label in labels {
            if tried >= MAX_NEW_THREADS {
                break;
            }
            if self.inner.thread_map.lock().contains_key(label) {
                continue;
            }
            // Someone else may have created it between our read and now (a second
            // client, or an earlier refresh): ask again before minting, because a
            // duplicate conversation is worse than a slightly stale one.
            let (fresh, _) = self.threads_read().await;
            if fresh.contains_key(label) {
                continue;
            }
            tried += 1;
            // Silent by design: this is background work with a live fallback (the tap
            // still opens a chat), and the tab shows `errors.threads` when the MAP itself
            // could not be read. A failure here is retried on the next refresh.
            if self
                .create_thread(label, notes.get(label).map(Vec::as_slice))
                .await
                .is_ok()
            {
                created = true;
            }
        }
        created
    }

    /// Create one thread conversation: `session/new` filed under the assistant project,
    /// renamed "Thread: <Label>", and recorded in the server-side map.
    ///
    /// The name matters: it is what every thread chat has always been called, so the
    /// sessions created before this map existed keep their identity.
    pub(crate) async fn create_thread(
        &self,
        label: &str,
        notes: Option<&[crate::tasks::ContextNote]>,
    ) -> Result<String, String> {
        // Asked again here, not just by the caller: this runs from a tap as well as
        // from a refresh, and both can be in flight at once.
        if let Some(existing) = self.inner.thread_map.lock().get(label).cloned() {
            return Ok(existing);
        }
        let conn = self.inner.conn.lock().clone().ok_or("not connected")?;
        if !conn.is_ready() {
            return Err("not connected".into());
        }
        let project = self.assistant_project();
        let id = conn
            .new_session_detached(Some(&project))
            .await
            .map_err(|e| e.to_string())?;
        if id.is_empty() {
            return Err("session/new returned no id".into());
        }
        let title = crate::tasks::thread_title(label);
        let _ = conn
            .rpc_async(
                "_goose/unstable/session/rename",
                json!({ "sessionId": id, "title": title }),
            )
            .await;
        let wire = id.clone();
        let name = title.clone();
        let label_owned = label.to_string();
        let link = self
            .tasks_call(move |hidden| {
                crate::tasks::link_params(hidden, &label_owned, &wire, &name)
            })
            .await;
        // The session exists and is named; without the link the next refresh would mint
        // another one, so the failure has to come out rather than leave a duplicate seed
        // behind.
        link?;
        self.inner
            .thread_map
            .lock()
            .insert(label.to_string(), id.clone());
        // The context: a thread chat that starts blank makes the reader (and the model)
        // work out which area it is. Best effort — a relay failure must not fail the
        // creation, the conversation already exists and is linked.
        if let Some(notes) = notes {
            self.write_thread_notes(label, notes).await;
        }
        Ok(id)
    }

    /// Put the area's rows into the session's SYSTEM PROMPT, where they cannot be lost.
    ///
    /// The notes (below) are a record written into history: once, best-effort, deduped by a
    /// text search, and summarised away when a thread gets long. This is the channel that
    /// makes a thread HAVE its area — goose re-sends `system_prompt_extras` on every
    /// inference, keyed, so a re-set replaces rather than accumulates. One round trip on
    /// open, and the prompt cache is invalidated when the text changes, so it is done on
    /// OPEN and not per turn.
    ///
    /// Silent on failure, like the notes: a thread still opens and still has its record.
    pub(crate) async fn set_thread_context(&self, label: &str, session_id: &str) {
        let Some(notes) = self.inner.last_notes.lock().get(label).cloned() else {
            return;
        };
        let text = crate::tasks::thread_context_text(&notes);
        if text.is_empty() {
            return;
        }
        let Some(conn) = self.inner.conn.lock().clone() else {
            return;
        };
        let params = crate::tasks::thread_prompt_params(session_id, &text);
        if let Err(e) = conn
            .rpc_async("_goose/unstable/session/system-prompt/set", params)
            .await
        {
            eprintln!("[thread-context] {label}: {e}");
        }
    }

    /// Offer a thread's context notes to its conversation.
    ///
    /// Best effort and idempotent by the relay's own rule (it writes a note only when the
    /// conversation does not already mention that note's key), so this is a no-op on a
    /// thread that is already current and a repair on one that is not. Each note is one
    /// deterministic `thread_note` round trip — no model turn — so a thread that gains an
    /// item costs exactly one call the next time it is opened.
    pub(crate) async fn write_thread_notes(&self, label: &str, notes: &[crate::tasks::ContextNote]) {
        for note in notes {
            let label_owned = label.to_string();
            let text = note.text.clone();
            let key = note.key.clone();
            if let Err(e) = self
                .tasks_call(move |wire| {
                    crate::tasks::note_params(wire, &label_owned, &text, &key)
                })
                .await
            {
                // Silent until 2026-11-04, and that silence is why a batch of threads opened
                // blank with nobody able to say why: the notes are best effort, so the
                // failure has to be visible SOMEWHERE.
                eprintln!("[thread-note] {label} {}: {e}", note.key);
            }
        }
    }

    /// Offer a thread's context to a conversation that may already exist.
    ///
    /// Best effort and idempotent by the relay's own rule (the `key`), so it is a no-op on
    /// a thread that is current and a repair on one that is not — including one minted
    /// before the notes carried detail, which is why it runs on every open and not just at
    /// mint. Runs off the caller's thread: `open_thread` is a tap handler and must not
    /// wait on tool calls.
    pub(crate) fn seed_thread_once(&self, label: &str) {
        let Some(notes) = self.inner.last_notes.lock().get(label).cloned() else {
            return;
        };
        let this = self.clone();
        let label = label.to_string();
        crate::roam::runtime().spawn(async move {
            this.write_thread_notes(&label, &notes).await;
        });
    }

    /// A mutation, then a refresh: the UI reflects server state, never a local
    /// patch of it (the rule the unstable shim's list replies already follow).
    pub(crate) async fn task_action<F: Fn(&str) -> Value>(&self, build: F) {
        match self.tasks_call(build).await {
            Ok(_) => self.tasks_refresh_now().await,
            Err(e) => self.alert(format!("My Day: the change was not saved ({e})")),
        }
    }
}
