// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reverse-loading and backfill pages for `Core` (docs/TRANSCRIPT_MODEL.md §Windows).
//! The facade, records and shared state stay in `lib.rs`; this module
//! contributes inherent `impl Core` methods only. Behaviour is unchanged.

use super::*;

impl Core {
    // -- reverse loading: paging the wire -------------------------------------

    /// The server reported `skipped` leading messages behind the load it just
    /// answered, having asked for `tail` trailing ones
    /// (docs/TRANSCRIPT_MODEL.md, "Windows"). Records the backlog, keeps
    /// `has_older` honest at the store's origin, and starts the background fill.
    pub(crate) fn note_backlog(&self, key: &str, skipped: usize, tail: usize) {
        let store = self.store_for(key);
        let more_changed = store.set_server_more(skipped > 0);
        let fill = {
            let mut state = self.inner.state.lock();
            let entry = state.backfill.entry(key.to_string()).or_default();
            entry.skipped = skipped;
            entry.tail = tail;
            entry.failures = 0;
            // A new load supersedes any fill the previous one started.
            entry.fill += 1;
            entry.fill
        };
        // While a replay is held, the paint it owes carries the cursor; a
        // SUPPRESSED open emits no paint at all, and the one it already sent
        // said "nothing older" because the backlog was not known yet.
        if more_changed && !store.emissions_held() {
            store.publish_window();
        }
        if skipped > 0 {
            self.spawn_fill(key.to_string(), fill);
        }
    }

    /// Walk the rest of the conversation in while the user reads the recent
    /// part: one page per [`FILL_DELAY_MS`], only while this chat is the one on
    /// screen and nothing else is using the session.
    ///
    /// Slow and cancellable on purpose. It is a background reader, not a sync:
    /// `entry.fill` is bumped by every new load, and leaving the chat ends it.
    pub(crate) fn spawn_fill(&self, key: String, gen: u64) {
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(FILL_DELAY_MS)).await;
                let live = {
                    let state = core.inner.state.lock();
                    state
                        .backfill
                        .get(&key)
                        .is_some_and(|b| b.fill == gen && b.skipped > 0)
                };
                if !live {
                    return;
                }
                if core.inner.hub.displayed_key().as_deref() != Some(key.as_str()) {
                    return;
                }
                // A running turn owns the session: its rows and a page's rows
                // would interleave on one wire, so wait for the next tick.
                if !core.can_page(&key) {
                    continue;
                }
                core.page(&key, PAGE_MESSAGES, PageReason::Fill).await;
            }
        });
    }

    /// May a page run for `key` right now?
    ///
    /// Every condition here is a corruption this could otherwise cause:
    ///
    /// - a turn in flight — the page's rows and the turn's would share one
    ///   notification stream, and the page's store is a scratch one;
    /// - a replay (open, reload, resync) — it owns the transcript;
    /// - a page already in flight — one at a time;
    /// - too many failures — a wire that cannot serve a page must not turn
    ///   scroll-back into a retry storm.
    pub(crate) fn can_page(&self, key: &str) -> bool {
        let state = self.inner.state.lock();
        !state.in_flight.contains(key)
            && !state.replaying.contains(key)
            && !state
                .backfill
                .get(key)
                .is_some_and(|b| b.page.is_some() || b.failures >= PAGE_FAILURES)
    }

    /// Fetch ONE page of history older than what the store holds, and splice it
    /// in front of it.
    ///
    /// Why a page and not a full replay: `session/load` replays from the START,
    /// so the only way to get OLDER history from this server is to ask for a
    /// LONGER trailing window — it has no backwards cursor. Each page therefore
    /// asks for more trailing messages than we hold rows (rows are an upper
    /// bound on messages), and the overlap is dropped by item id. That
    /// guarantees progress even when the conversation grew between pages.
    ///
    /// Why a scratch store: the rows arrive on the same notification stream the
    /// live chat uses, and turning them into items must not touch the transcript
    /// on screen. They land in a store whose listener is a no-op; only after the
    /// whole page has been ingested is it spliced in — and only if the real store
    /// is still the one this page was opened against (its generation says so).
    pub(crate) async fn page(&self, key: &str, want: usize, reason: PageReason) -> bool {
        let store = self.store_for(key);
        let (token, asked, before) = {
            let mut state = self.inner.state.lock();
            let entry = state.backfill.entry(key.to_string()).or_default();
            if entry.page.is_some() {
                return false;
            }
            entry.page_seq += 1;
            let token = entry.page_seq;
            entry.page = Some(token);
            let held = store.len();
            // Growth is GEOMETRIC, with a floor that guarantees progress:
            //
            //  - doubling keeps a big conversation to a handful of pages (a 4,221
            //    message chat is 7, and the total re-sent is ~2x the transcript
            //    rather than 17x, which is what linear growth costs);
            //  - rows are an upper bound on messages, so `held + page` can never
            //    be less than what we already have — which is what makes a page
            //    progress even after the conversation grew between pages;
            //  - the floor covers a SUPPRESSED load, whose tail was 1 (nothing to
            //    replay), so the first page is a real page rather than a doubling
            //    crawl from one message.
            let asked = (entry.tail * 2)
                .max(held + want.max(PAGE_MESSAGES))
                .max(PAGE_MESSAGES);
            entry.tail = asked;
            (token, asked, entry.skipped)
        };
        // Serialize against opens and replays: they rebuild this transcript.
        let load = store.begin_load().await;
        let generation = store.generation();
        let Some(owner) = self.owner_for(key) else {
            drop(load);
            self.end_page(key, token);
            self.settle(key, reason);
            return false;
        };
        let cwd = owner
            .session_cwd(key)
            .unwrap_or_else(|| self.resolve_cwd(key));
        let scratch = Arc::new(TranscriptStore::new(Box::new(crate::session::NullListener)));
        scratch.set_session(key);
        self.inner.hub.set_page_target(key, Some(scratch.clone()));
        let request = owner.request(
            "session/load",
            crate::session::session_load_params(key, &cwd, Some(asked)),
        );
        let reply = tokio::time::timeout(PAGE_TIMEOUT, request).await;
        // Un-point the scratch store FIRST: from here on this key's rows are the
        // live chat's again, whatever happens below.
        self.inner.hub.set_page_target(key, None);

        let done;
        match reply {
            Ok(Ok(reply)) => {
                let skipped = crate::session::replay_skipped(&reply);
                let items = scratch.rich_transcript();
                let spliced = if store.generation() == generation {
                    match store.prepend_rich(items.clone()) {
                        Some(n) => n,
                        None => {
                            // No stamp headroom left (a page may not renumber rows
                            // the client already holds). Rebuild from the whole
                            // item list instead — one repaint, in order.
                            let mut all = items;
                            all.extend(store.rich_transcript());
                            store.replace_rich(all, false);
                            0
                        }
                    }
                } else {
                    // The transcript was replaced while the page was in flight: it
                    // belongs to a conversation that is no longer on screen.
                    0
                };
                let progressed = spliced > 0 || skipped < before;
                let mut state = self.inner.state.lock();
                let entry = state.backfill.entry(key.to_string()).or_default();
                entry.skipped = if progressed { skipped } else { 0 };
                entry.failures = 0;
                done = entry.skipped == 0;
                drop(state);
                store.set_server_more(!done);
                if done {
                    // The cache is the whole conversation now: the next open must
                    // not page for what is already here.
                    self.save_cache_for(key);
                }
            }
            Ok(Err(_)) | Err(_) => {
                // A page that fails is not a transcript that is wrong: nothing
                // was spliced. Close the cursor after PAGE_FAILURES so a wire
                // that cannot serve pages does not turn scroll-back into a retry
                // storm; the next open re-probes.
                let mut state = self.inner.state.lock();
                let entry = state.backfill.entry(key.to_string()).or_default();
                entry.failures += 1;
                if entry.failures >= PAGE_FAILURES {
                    entry.skipped = 0;
                }
                done = entry.failures >= PAGE_FAILURES;
                drop(state);
                if done {
                    store.set_server_more(false);
                }
            }
        }
        drop(load);
        self.end_page(key, token);
        // A prompt parked behind this page (the chat is "busy" while paging) can
        // go now.
        self.flush_pending();
        // A scroll that arrived while this page was in flight takes precedence:
        // it is a client waiting on a cursor, and the page that just landed has
        // the rows it wants.
        let waiting = {
            let mut state = self.inner.state.lock();
            state
                .backfill
                .get_mut(key)
                .and_then(|entry| entry.wait.take())
        };
        self.settle(key, waiting.map_or(reason, PageReason::Scroll));
        done
    }

    /// Release this session's page claim.
    pub(crate) fn end_page(&self, key: &str, token: u64) {
        let mut state = self.inner.state.lock();
        if let Some(entry) = state.backfill.get_mut(key) {
            if entry.page == Some(token) {
                entry.page = None;
            }
        }
    }

    /// Answer whatever the client was waiting for when the page ended: a
    /// scroll-back gets its batch (possibly empty, which is how it learns the
    /// origin is real), and a background fill gets nothing (nobody is waiting).
    pub(crate) fn settle(&self, key: &str, reason: PageReason) {
        let PageReason::Scroll(count) = reason else {
            return;
        };
        let store = self.store_for(key);
        if store.can_load_older() {
            store.load_older(count);
        } else {
            // Nothing splices in front of the window: emit the cursor so the
            // client's in-flight `load_older` settles instead of spinning.
            store.publish_window();
        }
    }
}
