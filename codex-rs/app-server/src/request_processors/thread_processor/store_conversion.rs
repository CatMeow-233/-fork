//! Maps thread-store records and errors into app-server API types.

use super::*;

pub(crate) fn thread_read_view_error(err: ThreadReadViewError) -> JSONRPCErrorError {
    match err {
        ThreadReadViewError::InvalidRequest(message) => invalid_request(message),
        ThreadReadViewError::Unsupported(operation) => {
            unsupported_thread_store_operation(operation)
        }
        ThreadReadViewError::Internal(message) => internal_error(message),
        ThreadReadViewError::JsonRpc(error) => error,
    }
}

pub(super) fn paginated_history_list_error(err: ThreadStoreError) -> JSONRPCErrorError {
    match err {
        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
        ThreadStoreError::Unsupported { operation } => {
            unsupported_thread_store_operation(operation)
        }
        ThreadStoreError::ThreadNotFound { thread_id } => {
            invalid_request(format!("no rollout found for thread id {thread_id}"))
        }
        err => internal_error(format!("failed to list thread history: {err}")),
    }
}

pub(super) fn stored_thread_item_to_api_item(
    stored_item: &codex_thread_store::StoredThreadItem,
) -> Result<ThreadItem, JSONRPCErrorError> {
    let payload = serde_json::from_slice(&stored_item.item_json).map_err(|err| {
        internal_error(format!(
            "failed to deserialize stored thread item {}: {err}",
            stored_item.item_id
        ))
    })?;
    Ok(ThreadItem {
        payload,
        started_at_ms: stored_item.started_at_ms,
        completed_at_ms: stored_item.completed_at_ms,
    })
}

pub(super) fn stored_thread_item_to_entry(
    stored_item: codex_thread_store::StoredThreadItem,
) -> Result<ThreadItemEntry, JSONRPCErrorError> {
    let item = stored_thread_item_to_api_item(&stored_item)?;
    Ok(ThreadItemEntry {
        turn_id: stored_item.turn_id,
        output_deferred: false,
        started_at_ms: item.started_at_ms,
        completed_at_ms: item.completed_at_ms,
        item,
    })
}

pub(super) fn stored_turn_to_api_turn(
    turn: StoredTurn,
    items_view: TurnItemsView,
) -> Result<Turn, JSONRPCErrorError> {
    let status = match turn.status {
        StoredTurnStatus::Completed => TurnStatus::Completed,
        StoredTurnStatus::Interrupted => TurnStatus::Interrupted,
        StoredTurnStatus::Failed => TurnStatus::Failed,
        StoredTurnStatus::InProgress => TurnStatus::InProgress,
    };
    let error = turn.error.map(|error| TurnError {
        misalignment: None,
        message: error.message,
        codex_error_info: error.codex_error_info,
        additional_details: error.additional_details,
    });
    let items = turn
        .items
        .iter()
        .map(stored_thread_item_to_api_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Turn {
        id: turn.turn_id,
        root_turn_id: turn.root_turn_id,
        items,
        items_view,
        status,
        error,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
        duration_ms: turn.duration_ms,
    })
}

pub(crate) fn unsupported_thread_store_operation(operation: &'static str) -> JSONRPCErrorError {
    method_not_found(format!("{operation} is not supported yet"))
}

pub(super) fn thread_store_list_error(err: ThreadStoreError) -> JSONRPCErrorError {
    match err {
        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
        ThreadStoreError::Unsupported { operation } => {
            unsupported_thread_store_operation(operation)
        }
        err => internal_error(format!("failed to list threads: {err}")),
    }
}

pub(super) fn thread_store_resume_read_error(err: ThreadStoreError) -> JSONRPCErrorError {
    match err {
        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
        ThreadStoreError::Unsupported { operation } => {
            unsupported_thread_store_operation(operation)
        }
        ThreadStoreError::ThreadNotFound { thread_id } => {
            invalid_request(format!("no rollout found for thread id {thread_id}"))
        }
        err => internal_error(format!("failed to read thread: {err}")),
    }
}

pub(super) fn thread_turns_list_history_load_error(
    thread_id: ThreadId,
    err: ThreadStoreError,
) -> ThreadReadViewError {
    match err {
        ThreadStoreError::InvalidRequest { message }
            if message.starts_with("failed to resolve rollout path `") =>
        {
            ThreadReadViewError::InvalidRequest(format!(
                "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
            ))
        }
        ThreadStoreError::InvalidRequest { message } => {
            ThreadReadViewError::InvalidRequest(message)
        }
        ThreadStoreError::Unsupported { operation } => ThreadReadViewError::Unsupported(operation),
        err => ThreadReadViewError::Internal(format!(
            "failed to load thread history for thread {thread_id}: {err}"
        )),
    }
}

pub(super) fn thread_read_history_load_error(
    thread_id: ThreadId,
    err: ThreadStoreError,
) -> ThreadReadViewError {
    match err {
        ThreadStoreError::InvalidRequest { message }
            if message.starts_with("failed to resolve rollout path `") =>
        {
            ThreadReadViewError::InvalidRequest(format!(
                "thread {thread_id} is not materialized yet; includeTurns is unavailable before first user message"
            ))
        }
        ThreadStoreError::ThreadNotFound {
            thread_id: missing_thread_id,
        } if missing_thread_id == thread_id => ThreadReadViewError::InvalidRequest(format!(
            "thread {thread_id} is not materialized yet; includeTurns is unavailable before first user message"
        )),
        ThreadStoreError::InvalidRequest { message } => {
            ThreadReadViewError::InvalidRequest(message)
        }
        ThreadStoreError::Unsupported { operation } => ThreadReadViewError::Unsupported(operation),
        err => ThreadReadViewError::Internal(format!(
            "failed to load thread history for thread {thread_id}: {err}"
        )),
    }
}

pub(super) fn conversation_summary_thread_id_read_error(
    conversation_id: ThreadId,
    err: ThreadStoreError,
) -> JSONRPCErrorError {
    let no_rollout_message = format!("no rollout found for thread id {conversation_id}");
    match err {
        ThreadStoreError::InvalidRequest { message } if message == no_rollout_message => {
            conversation_summary_not_found_error(conversation_id)
        }
        ThreadStoreError::Unsupported { operation } => {
            unsupported_thread_store_operation(operation)
        }
        ThreadStoreError::ThreadNotFound { thread_id } if thread_id == conversation_id => {
            conversation_summary_not_found_error(conversation_id)
        }
        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
        err => internal_error(format!(
            "failed to load conversation summary for {conversation_id}: {err}"
        )),
    }
}

fn conversation_summary_not_found_error(conversation_id: ThreadId) -> JSONRPCErrorError {
    invalid_request(format!(
        "no rollout found for conversation id {conversation_id}"
    ))
}

pub(super) fn conversation_summary_rollout_path_read_error(
    path: &Path,
    err: ThreadStoreError,
) -> JSONRPCErrorError {
    match err {
        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
        ThreadStoreError::Unsupported { operation } => {
            unsupported_thread_store_operation(operation)
        }
        err => internal_error(format!(
            "failed to load conversation summary from {}: {}",
            path.display(),
            err
        )),
    }
}

pub(crate) fn core_thread_write_error(operation: &str, err: CodexErr) -> JSONRPCErrorError {
    match err.details() {
        CodexErrorDetails::ThreadNotFound(thread_id) => {
            invalid_request(format!("thread not found: {thread_id}"))
        }
        CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
        CodexErrorDetails::UnsupportedOperation(message) => method_not_found(message.clone()),
        _ => internal_error(format!("failed to {operation}: {err}")),
    }
}

pub(super) fn set_thread_name_from_title(thread: &mut Thread, title: String) {
    if title.trim().is_empty() || thread.preview.trim() == title.trim() {
        return;
    }
    thread.name = Some(title);
}

pub(super) fn thread_from_stored_thread(
    thread: StoredThread,
    fallback_provider: &str,
    fallback_cwd: &AbsolutePathBuf,
) -> (Thread, Option<codex_thread_store::StoredThreadHistory>) {
    let path = thread.rollout_path;
    let git_info = thread.git_info.map(|info| ApiGitInfo {
        sha: info.commit_hash.map(|sha| sha.0),
        branch: info.branch,
        origin_url: info.repository_url.map(String::from),
    });
    let cwd = AbsolutePathBuf::relative_to_current_dir(path_utils::normalize_for_native_workdir(
        thread.cwd,
    ))
    .unwrap_or_else(|err| {
        warn!("failed to normalize thread cwd while reading stored thread: {err}");
        fallback_cwd.clone()
    });
    let source = with_thread_spawn_agent_metadata(
        thread.source,
        thread.agent_nickname.clone(),
        thread.agent_role.clone(),
    );
    let history = thread.history;
    let thread_id = thread.thread_id.to_string();
    let thread = Thread {
        id: thread_id.clone(),
        environments: None,
        extra: None,
        session_id: thread_id,
        forked_from_id: thread.forked_from_id.map(|id| id.to_string()),
        parent_thread_id: thread.parent_thread_id.map(|id| id.to_string()),
        preview: thread.preview,
        ephemeral: false,
        section: thread.section.map(|section| ThreadSection {
            id: section.id,
            name: section.name,
            appearance: section
                .appearance
                .map(|appearance| ThreadSectionAppearance {
                    icon: appearance.icon,
                    color: appearance.color,
                }),
        }),
        section_entered_at: thread
            .section_entered_at
            .map(|entered_at| entered_at.timestamp()),
        project_id: thread.project_id,
        history_mode: thread.history_mode.into(),
        model_provider: if thread.model_provider.is_empty() {
            fallback_provider.to_string()
        } else {
            thread.model_provider
        },
        model: thread.model,
        reasoning_effort: thread.reasoning_effort,
        created_at: thread.created_at.timestamp(),
        updated_at: thread.updated_at.timestamp(),
        recency_at: Some(thread.recency_at.timestamp()),
        status: ThreadStatus::NotLoaded,
        path,
        cwd,
        cli_version: thread.cli_version,
        originator: thread.originator,
        agent_nickname: source.get_nickname(),
        agent_role: source.get_agent_role(),
        source: source.into(),
        can_accept_direct_input: None,
        thread_source: thread.thread_source.map(Into::into),
        git_info,
        name: thread.name,
        daybreak_enabled: thread.daybreak_enabled,
        turns: Vec::new(),
    };
    (thread, history)
}

pub(super) fn summary_from_stored_thread(
    thread: StoredThread,
    fallback_provider: &str,
) -> ConversationSummary {
    let path = thread.rollout_path.unwrap_or_default();
    let source = with_thread_spawn_agent_metadata(
        thread.source,
        thread.agent_nickname.clone(),
        thread.agent_role.clone(),
    );
    let git_info = thread.git_info.map(|git| ConversationGitInfo {
        sha: git.commit_hash.map(|sha| sha.0),
        branch: git.branch,
        origin_url: git.repository_url.map(String::from),
    });
    ConversationSummary {
        conversation_id: thread.thread_id,
        path,
        preview: thread.preview,
        // Preserve millisecond precision from the thread store so thread/list cursors
        // round-trip the same ordering key used by pagination queries.
        timestamp: Some(
            thread
                .created_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ),
        updated_at: Some(
            thread
                .updated_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ),
        model_provider: if thread.model_provider.is_empty() {
            fallback_provider.to_string()
        } else {
            thread.model_provider
        },
        cwd: thread.cwd,
        cli_version: thread.cli_version,
        source,
        git_info,
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn summary_from_state_db_metadata(
    conversation_id: ThreadId,
    path: PathBuf,
    first_user_message: Option<String>,
    preview: Option<String>,
    timestamp: String,
    updated_at: String,
    model_provider: String,
    cwd: PathBuf,
    cli_version: String,
    source: String,
    _thread_source: Option<codex_protocol::protocol::ThreadSource>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    git_sha: Option<String>,
    git_branch: Option<String>,
    git_origin_url: Option<String>,
) -> ConversationSummary {
    let preview = preview.or(first_user_message).unwrap_or_default();
    let source = serde_json::from_str(&source)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(source.clone())))
        .unwrap_or(codex_protocol::protocol::SessionSource::Unknown);
    let source = with_thread_spawn_agent_metadata(source, agent_nickname, agent_role);
    let git_info = if git_sha.is_none() && git_branch.is_none() && git_origin_url.is_none() {
        None
    } else {
        Some(ConversationGitInfo {
            sha: git_sha,
            branch: git_branch,
            origin_url: git_origin_url,
        })
    };
    ConversationSummary {
        conversation_id,
        path,
        preview,
        timestamp: Some(timestamp),
        updated_at: Some(updated_at),
        model_provider,
        cwd,
        cli_version,
        source,
        git_info,
    }
}

#[cfg(test)]
pub(super) fn summary_from_thread_metadata(metadata: &ThreadMetadata) -> ConversationSummary {
    summary_from_state_db_metadata(
        metadata.id,
        metadata.rollout_path.clone(),
        metadata.first_user_message.clone(),
        metadata.preview.clone(),
        metadata
            .created_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        metadata
            .updated_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        metadata.model_provider.clone(),
        metadata.cwd.clone(),
        metadata.cli_version.clone(),
        metadata.source.clone(),
        metadata.thread_source.clone(),
        metadata.agent_nickname.clone(),
        metadata.agent_role.clone(),
        metadata.git_sha.clone(),
        metadata.git_branch.clone(),
        metadata.git_origin_url.clone().map(String::from),
    )
}

pub(super) fn preview_from_rollout_items(items: &[RolloutItem]) -> String {
    items
        .iter()
        .find_map(|item| match item {
            RolloutItem::ResponseItem(item) => match codex_core::parse_turn_item(&item.item) {
                Some(codex_protocol::items::TurnItem::UserMessage(user)) => Some(user.message()),
                _ => None,
            },
            _ => None,
        })
        .map(|preview| strip_user_message_prefix(preview.as_str()).to_string())
        .unwrap_or_default()
}

pub(super) fn build_thread_from_snapshot(
    thread_id: ThreadId,
    session_id: String,
    multi_agent_version: Option<codex_protocol::protocol::MultiAgentVersion>,
    config_snapshot: &ThreadConfigSnapshot,
    path: Option<PathBuf>,
) -> Thread {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    Thread {
        id: thread_id.to_string(),
        environments: Some(
            config_snapshot
                .environment_selections()
                .iter()
                .map(Into::into)
                .collect(),
        ),
        extra: None,
        session_id,
        forked_from_id: None,
        parent_thread_id: config_snapshot.parent_thread_id.map(|id| id.to_string()),
        preview: String::new(),
        ephemeral: config_snapshot.ephemeral,
        section: None,
        section_entered_at: None,
        project_id: None,
        history_mode: config_snapshot.history_mode.into(),
        model_provider: config_snapshot.model_provider_id.clone(),
        model: Some(config_snapshot.model.clone()),
        reasoning_effort: config_snapshot.reasoning_effort.clone(),
        created_at: now,
        updated_at: now,
        recency_at: Some(now),
        status: ThreadStatus::NotLoaded,
        path,
        cwd: config_snapshot.cwd().clone(),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        originator: (!config_snapshot.originator.is_empty())
            .then(|| config_snapshot.originator.clone()),
        agent_nickname: config_snapshot.session_source.get_nickname(),
        agent_role: config_snapshot.session_source.get_agent_role(),
        source: config_snapshot.session_source.clone().into(),
        can_accept_direct_input: Some(can_accept_direct_input(
            multi_agent_version,
            &config_snapshot.session_source,
        )),
        thread_source: config_snapshot.thread_source.clone().map(Into::into),
        git_info: None,
        name: None,
        daybreak_enabled: None,
        turns: Vec::new(),
    }
}

pub(super) fn build_thread_from_loaded_snapshot(
    thread_id: ThreadId,
    config_snapshot: &ThreadConfigSnapshot,
    loaded_thread: &CodexThread,
) -> Thread {
    build_thread_from_snapshot(
        thread_id,
        loaded_thread.startup_metadata().session_id.to_string(),
        loaded_thread.multi_agent_version(),
        config_snapshot,
        loaded_thread.rollout_path(),
    )
}
