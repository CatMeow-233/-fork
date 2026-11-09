//! Connection/thread state, thread initialization and goal events, voice sessions, context lookups.

use super::*;

impl AnalyticsReducer {
    pub(super) fn ingest_initialize(
        &mut self,
        connection_id: u64,
        params: InitializeParams,
        product_client_id: String,
        runtime: CodexRuntimeMetadata,
        rpc_transport: AppServerRpcTransport,
    ) {
        self.connections.insert(
            connection_id,
            ConnectionState {
                app_server_client: CodexAppServerClientMetadata {
                    product_client_id,
                    client_name: Some(params.client_info.name),
                    client_version: Some(params.client_info.version),
                    rpc_transport,
                    experimental_api_enabled: params
                        .capabilities
                        .map(|capabilities| capabilities.experimental_api),
                },
                runtime,
            },
        );
    }

    pub(super) fn ingest_subagent_thread_started(
        &mut self,
        input: SubAgentThreadStartedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let parent_thread_id = input.parent_thread_id.clone();
        let parent_connection_id = parent_thread_id
            .as_deref()
            .and_then(|parent_thread_id| self.thread_connection_id(parent_thread_id));
        let thread_state = self.threads.entry(input.thread_id.clone()).or_default();
        thread_state
            .originator
            .get_or_insert_with(|| input.product_client_id.clone());
        if thread_state.metadata.is_none()
            || matches!(input.initialization_mode, ThreadInitializationMode::Resumed)
        {
            thread_state.metadata = Some(ThreadMetadataState {
                session_id: input.session_id.clone(),
                thread_source: input.thread_source.clone(),
                initialization_mode: input.initialization_mode,
                subagent_source: Some(subagent_source_name(&input.subagent_source)),
                parent_thread_id,
            });
        }
        if thread_state.connection_id.is_none() {
            thread_state.connection_id = parent_connection_id;
        }
        // Guardian prewarm can register lineage before parent client metadata is set.
        // Keep the existing completeness requirement for the initialization event.
        if input.client_name.is_some() && input.client_version.is_some() {
            out.push(TrackEventRequest::ThreadInitialized(
                subagent_thread_started_event_request(input),
            ));
        }
    }

    pub(super) fn thread_archive_event_params(
        &self,
        thread_id: String,
        action: ThreadArchiveAction,
    ) -> ThreadArchiveEventParams {
        let thread_state = self.threads.get(&thread_id);
        let connection_state = self
            .thread_connection_id(&thread_id)
            .and_then(|connection_id| self.connections.get(&connection_id));
        let thread_metadata = thread_state.and_then(|thread_state| thread_state.metadata.as_ref());

        ThreadArchiveEventParams {
            thread_id,
            action,
            occurred_at_ms: now_unix_millis(),
            app_server_client: thread_state
                .zip(connection_state)
                .map(|(thread_state, connection_state)| {
                    thread_state.app_server_client(connection_state)
                }),
            runtime: connection_state.map(|connection_state| connection_state.runtime.clone()),
            thread_source: thread_metadata
                .and_then(|thread_metadata| thread_metadata.thread_source.as_ref())
                .filter(|thread_source| {
                    !matches!(thread_source, ThreadSource::Feature(feature) if feature != "automation")
                })
                .cloned(),
            parent_thread_id: thread_metadata
                .and_then(|thread_metadata| thread_metadata.parent_thread_id.clone()),
        }
    }

    pub(super) fn emit_thread_initialized(
        &mut self,
        connection_id: u64,
        thread: codex_app_server_protocol::Thread,
        model: String,
        initialization_mode: ThreadInitializationMode,
        thread_originator: Option<String>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let session_source: SessionSource = thread.source.into();
        let is_worktree =
            codex_git_utils::repository_identity(thread.cwd.as_path()).and_then(|_| {
                codex_git_utils::get_git_repo_root(thread.cwd.canonicalize().ok()?.as_path())
                    .map(|root| root.join(".git").is_file())
            });
        let session_id = thread.session_id;
        let thread_id = thread.id;
        let parent_thread_id = thread.parent_thread_id;
        let forked_from_thread_id = thread.forked_from_id;
        let Some(connection_state) = self.connections.get(&connection_id) else {
            return;
        };
        let thread_metadata = ThreadMetadataState::from_thread_metadata(
            session_id.clone(),
            &session_source,
            thread.thread_source.map(Into::into),
            parent_thread_id,
            initialization_mode,
        );
        let thread_state = self.threads.entry(thread_id.clone()).or_default();
        if let Some(originator) = thread_originator {
            thread_state.originator = Some(originator);
        }
        thread_state.connection_id = Some(connection_id);
        thread_state.metadata = Some(thread_metadata.clone());
        let app_server_client = thread_state.app_server_client(connection_state);
        out.push(TrackEventRequest::ThreadInitialized(
            ThreadInitializedEvent {
                event_type: "codex_thread_initialized",
                event_params: ThreadInitializedEventParams {
                    thread_id,
                    session_id,
                    app_server_client,
                    runtime: connection_state.runtime.clone(),
                    model,
                    ephemeral: thread.ephemeral,
                    is_worktree,
                    thread_source: thread_metadata.thread_source,
                    initialization_mode,
                    subagent_source: thread_metadata.subagent_source.clone(),
                    parent_thread_id: thread_metadata.parent_thread_id,
                    forked_from_thread_id,
                    created_at: u64::try_from(thread.created_at).unwrap_or_default(),
                },
            },
        ));
    }

    pub(super) fn set_active_voice_session_id(
        &mut self,
        thread_id: String,
        realtime_session_id: Option<String>,
    ) {
        let thread = self.threads.entry(thread_id).or_default();
        let realtime_session_id = realtime_session_id.filter(|id| !id.is_empty());
        let Some(session) = thread.active_voice_session.as_mut() else {
            return;
        };
        session.session_id.clone_from(&realtime_session_id);
        if let Some(turn_id) = thread.active_turn_id.clone() {
            self.turn_state(turn_id).voice_session_id = realtime_session_id;
        }
    }

    pub(super) fn start_voice_session(
        &mut self,
        thread_id: String,
        realtime_session_id: Option<String>,
        started_at: u64,
    ) {
        self.close_voice_session(&thread_id, started_at);
        let realtime_session_id = realtime_session_id.filter(|id| !id.is_empty());
        let thread = self.threads.entry(thread_id).or_default();
        thread.active_voice_session = Some(VoiceSessionWindow {
            session_id: realtime_session_id.clone(),
            started_at,
            closed_at: None,
        });
        if let Some(turn_id) = thread.active_turn_id.clone() {
            self.turn_state(turn_id).voice_session_id = realtime_session_id;
        }
    }

    pub(super) fn close_voice_session(&mut self, thread_id: &str, closed_at: u64) {
        let Some(thread) = self.threads.get_mut(thread_id) else {
            return;
        };
        if let Some(mut session) = thread.active_voice_session.take() {
            session.closed_at = Some(closed_at);
            if session.session_id.is_some() {
                if thread.recent_voice_sessions.len() >= MAX_RECENT_VOICE_SESSION_WINDOWS {
                    thread.recent_voice_sessions.pop_front();
                }
                thread.recent_voice_sessions.push_back(session);
            }
        }
    }

    pub(super) fn ingest_goal(&mut self, input: CodexGoalEvent, out: &mut Vec<TrackEventRequest>) {
        let Some((connection_state, thread_state, thread_metadata)) =
            self.thread_context_or_warn(AnalyticsDropSite::goal(&input))
        else {
            return;
        };
        out.push(TrackEventRequest::Goal(Box::new(CodexGoalEventRequest {
            event_type: "codex_goal_event",
            event_params: codex_goal_event_params(
                input,
                thread_metadata.session_id.clone(),
                thread_state.app_server_client(connection_state),
                connection_state.runtime.clone(),
                thread_metadata.thread_source.clone(),
                thread_metadata.subagent_source.clone(),
                thread_metadata.parent_thread_id.clone(),
            ),
        })));
    }

    /// Resolve the parent connection lazily when a subagent fact arrives first.
    ///
    /// Parents are spawned before their children, so ancestor links cannot cycle.
    pub(super) fn thread_connection_id(&self, thread_id: &str) -> Option<u64> {
        let mut thread = self.threads.get(thread_id)?;
        while thread.connection_id.is_none() {
            let thread_metadata = thread.metadata.as_ref()?;
            let parent_thread_id = thread_metadata.parent_thread_id.as_deref()?;
            thread = self.threads.get(parent_thread_id)?;
        }
        thread.connection_id
    }

    fn thread_connection_or_warn(
        &self,
        drop_site: AnalyticsDropSite<'_>,
    ) -> Option<&ConnectionState> {
        let Some(connection_id) = self.thread_connection_id(drop_site.thread_id) else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadConnection);
            return None;
        };
        let Some(connection_state) = self.connections.get(&connection_id) else {
            warn_missing_analytics_context(
                &drop_site,
                MissingAnalyticsContext::Connection { connection_id },
            );
            return None;
        };
        Some(connection_state)
    }

    pub(super) fn thread_context_or_warn(
        &self,
        drop_site: AnalyticsDropSite<'_>,
    ) -> Option<(
        &ConnectionState,
        &ThreadAnalyticsState,
        &ThreadMetadataState,
    )> {
        let connection_state = self.thread_connection_or_warn(drop_site)?;
        let thread_state = self.threads.get(drop_site.thread_id)?;
        let Some(thread_metadata) = thread_state.metadata.as_ref() else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadMetadata);
            return None;
        };
        Some((connection_state, thread_state, thread_metadata))
    }
}

pub(super) fn warn_missing_analytics_context(
    drop_site: &AnalyticsDropSite<'_>,
    missing: MissingAnalyticsContext,
) {
    let (missing_context, connection_id) = match missing {
        MissingAnalyticsContext::ThreadConnection => ("thread_connection", None),
        MissingAnalyticsContext::Connection { connection_id } => {
            ("connection", Some(connection_id))
        }
        MissingAnalyticsContext::ThreadMetadata => ("thread_metadata", None),
    };
    tracing::warn!(
        thread_id = %drop_site.thread_id,
        turn_id = ?drop_site.turn_id,
        review_id = ?drop_site.review_id,
        item_id = ?drop_site.item_id,
        missing_context,
        connection_id,
        "dropping {} analytics event: missing analytics context",
        drop_site.event_name
    );
}
