use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::user_input::UserInput;
use codex_rollout::RolloutItem;
use codex_state::ThreadMetadataBuilder;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::ThreadStore;
use crate::local::LocalThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_archived_session_file;
use crate::local::test_support::write_session_file;
use crate::local::test_support::write_session_file_with_fork;
use crate::local::test_support::write_session_file_with_history_mode;

#[tokio::test]
async fn read_thread_returns_active_rollout_summary() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(205);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let active_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(active_path));
    assert_eq!(thread.archived_at, None);
    assert_eq!(thread.preview, "Hello from user");
    assert_eq!(
        thread.history.expect("history should load").thread_id,
        thread_id
    );
}

#[tokio::test]
async fn read_thread_returns_rollout_path_summary() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(211);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let active_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let relative_path = active_path
        .strip_prefix(home.path())
        .expect("path should be under codex home")
        .to_path_buf();

    let thread = store
        .read_thread_by_rollout_path(
            relative_path,
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read thread by rollout path");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(
        thread.rollout_path,
        Some(std::fs::canonicalize(active_path).expect("canonical path"))
    );
    assert_eq!(thread.preview, "Hello from user");
}

#[tokio::test]
async fn read_thread_by_rollout_path_preserves_legacy_name_and_sqlite_git_info() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(223);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let active_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        active_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.model_provider = Some(config.default_model_provider_id.clone());
    builder.git_branch = Some("sqlite-branch".to_string());
    let recency_at = chrono::DateTime::parse_from_rfc3339("2026-01-03T12:00:00Z")
        .expect("timestamp should parse")
        .with_timezone(&Utc);
    builder.recency_at = Some(recency_at);
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.title = "Stale SQLite name".to_string();
    metadata.section = Some(codex_state::ThreadSection {
        id: codex_state::PINNED_THREAD_SECTION_ID.to_string(),
        name: codex_state::PINNED_THREAD_SECTION_NAME.to_string(),
        appearance: None,
    });
    metadata.section_position = Some(2_000_000);
    metadata.section_entered_at = Some(recency_at);
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");
    codex_rollout::append_thread_name(home.path(), thread_id, "Latest index name")
        .await
        .expect("append thread name");

    let thread = store
        .read_thread_by_rollout_path(
            active_path,
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read thread by rollout path");

    let git_info = thread.git_info.expect("git info should be present");
    assert_eq!(thread.name.as_deref(), Some("Latest index name"));
    assert_eq!(thread.recency_at, recency_at);
    assert_eq!(
        thread.section.as_ref().map(|section| section.id.as_str()),
        Some(codex_state::PINNED_THREAD_SECTION_ID)
    );
    assert_eq!(thread.section_position, Some(2_000_000));
    assert_eq!(thread.section_entered_at, Some(recency_at));
    assert_eq!(git_info.branch.as_deref(), Some("sqlite-branch"));
    assert_eq!(
        git_info.commit_hash.as_ref().map(|sha| sha.0.as_str()),
        Some("abcdef")
    );
    assert_eq!(
        git_info.repository_url.as_deref(),
        Some("https://example.com/repo.git")
    );
}

#[tokio::test]
async fn read_thread_returns_archived_rollout_when_requested() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(207);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let archived_path = write_archived_session_file(home.path(), "2025-01-03T12-00-00", uuid)
        .expect("archived session file");

    let active_only_err = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect_err("active-only read should fail for archived rollout");
    let ThreadStoreError::InvalidRequest { message } = active_only_err else {
        panic!("expected invalid request error");
    };
    assert_eq!(
        message,
        format!("no rollout found for thread id {thread_id}")
    );

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: false,
        })
        .await
        .expect("read archived thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(archived_path));
    assert!(thread.archived_at.is_some());
    assert_eq!(thread.preview, "Archived user message");
    assert!(thread.history.is_none());
}

#[tokio::test]
async fn read_thread_prefers_active_rollout_over_archived() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(208);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let active_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    write_archived_session_file(home.path(), "2025-01-03T12-00-00", uuid)
        .expect("archived session file");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.rollout_path, Some(active_path));
    assert_eq!(thread.archived_at, None);
    assert_eq!(thread.preview, "Hello from user");
}

#[tokio::test]
async fn read_thread_returns_forked_from_id() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(209);
    let parent_uuid = Uuid::from_u128(210);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let parent_thread_id =
        ThreadId::from_string(&parent_uuid.to_string()).expect("valid parent thread id");
    write_session_file_with_fork(
        home.path(),
        home.path().join("sessions/2025/01/03"),
        "2025-01-03T12-00-00",
        uuid,
        "Forked user message",
        Some("test-provider"),
        Some(parent_uuid),
        ThreadHistoryMode::Legacy,
    )
    .expect("forked session file");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.forked_from_id, Some(parent_thread_id));
}

#[tokio::test]
async fn read_thread_applies_sqlite_thread_name() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(212);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder =
        ThreadMetadataBuilder::new(thread_id, rollout_path, Utc::now(), SessionSource::Cli);
    builder.model_provider = Some(config.default_model_provider_id.clone());
    builder.cwd = home.path().to_path_buf();
    builder.cli_version = Some("test_version".to_string());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.title = "Saved title".to_string();
    metadata.first_user_message = Some("Hello from user".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.name, Some("Saved title".to_string()));
}

#[tokio::test]
async fn read_paginated_thread_prefers_sqlite_display_metadata() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(228);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T12-00-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("session file");
    codex_rollout::append_rollout_item_to_path(
        &rollout_path,
        &RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[UserInput::Text {
                text: "Rollout user message".to_string(),
                text_elements: Vec::new(),
            }])),
            started_at_ms: Some(0),
            completed_at_ms: 0,
        })),
    )
    .await
    .expect("append rollout user message");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        rollout_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.history_mode = ThreadHistoryMode::Paginated;
    builder.model_provider = Some("Canonical SQLite provider".to_string());
    builder.cwd = home.path().to_path_buf();
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.name = Some("Canonical SQLite name".to_string());
    metadata.preview = Some("Canonical SQLite preview".to_string());
    metadata.first_user_message = Some("Canonical SQLite first user message".to_string());
    metadata.title = "Canonical SQLite derived title".to_string();
    metadata.model = Some("Canonical SQLite model".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");
    codex_rollout::append_thread_name(home.path(), thread_id, "Stale index name")
        .await
        .expect("append legacy thread name");

    let thread_by_id = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");
    let thread_by_path = store
        .read_thread_by_rollout_path(
            rollout_path,
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read thread by rollout path");

    for thread in [thread_by_id, thread_by_path] {
        assert_eq!(
            (
                thread.name.as_deref(),
                thread.preview.as_str(),
                thread.first_user_message.as_deref(),
                thread.model_provider.as_str(),
                thread.model.as_deref(),
                thread.originator.as_deref(),
            ),
            (
                Some("Canonical SQLite name"),
                "Canonical SQLite preview",
                Some("Canonical SQLite first user message"),
                "Canonical SQLite provider",
                Some("Canonical SQLite model"),
                Some("test_originator"),
            )
        );
    }
}

#[tokio::test]
async fn read_thread_returns_permission_profile_from_sqlite_metadata() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(225);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder =
        ThreadMetadataBuilder::new(thread_id, rollout_path, Utc::now(), SessionSource::Cli);
    builder.model_provider = Some(config.default_model_provider_id.clone());
    builder.cwd = home.path().to_path_buf();
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    let permission_profile: PermissionProfile = PermissionProfile::Disabled;
    metadata.sandbox_policy =
        serde_json::to_string(&permission_profile).expect("serialize profile");
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.preview, "Hello from user");
    assert_eq!(thread.permission_profile, PermissionProfile::Disabled);
}

#[tokio::test]
async fn read_thread_accepts_legacy_sandbox_policy_metadata() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(226);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder =
        ThreadMetadataBuilder::new(thread_id, rollout_path, Utc::now(), SessionSource::Cli);
    builder.model_provider = Some(config.default_model_provider_id.clone());
    builder.cwd = home.path().to_path_buf();
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.sandbox_policy = "danger-full-access".to_string();
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.permission_profile, PermissionProfile::Disabled);
}

#[tokio::test]
async fn read_thread_preserves_sqlite_cwd_when_rollout_metadata_is_stale() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let uuid = Uuid::from_u128(224);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let day_dir = home.path().join("sessions/2025/01/03");
    std::fs::create_dir_all(&day_dir).expect("sessions dir");
    let rollout_path = day_dir.join(format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"));
    let mut file = std::fs::File::create(&rollout_path).expect("session file");
    let rollout_cwd = PathBuf::from("/");
    let meta = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "session_id": uuid,
            "id": uuid,
            "timestamp": "2025-01-03T12:00:00Z",
            "cwd": rollout_cwd,
            "originator": "test_originator",
            "cli_version": "test_version",
            "source": "cli",
            "model_provider": "rollout-provider"
        },
    });
    writeln!(file, "{meta}").expect("write session meta");
    let user_event = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": "Hello from rollout",
            "kind": "plain",
        },
    });
    writeln!(file, "{user_event}").expect("write user event");

    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        rollout_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.model_provider = Some(config.default_model_provider_id.clone());
    let sqlite_cwd = home.path().join("sqlite-workspace");
    builder.cwd = sqlite_cwd.clone();
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.title = "Saved title".to_string();
    metadata.first_user_message = Some("Hello from sqlite".to_string());
    metadata.sandbox_policy = "workspace-write".to_string();
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path.clone()));
    assert_eq!(thread.preview, "Hello from rollout");
    assert_eq!(thread.name, Some("Saved title".to_string()));
    assert_eq!(thread.model_provider, "rollout-provider");
    assert_eq!(thread.cwd, sqlite_cwd);
    let legacy_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        network_access: false,
        exclude_tmpdir_env_var: false,
        exclude_slash_tmp: false,
    };
    assert_eq!(
        thread.permission_profile,
        PermissionProfile::from_legacy_sandbox_policy_for_cwd(&legacy_policy, sqlite_cwd.as_path())
    );

    let thread_by_rollout_path = store
        .read_thread_by_rollout_path(
            rollout_path.clone(),
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read thread by rollout path");

    assert_eq!(thread_by_rollout_path.cwd, sqlite_cwd);
    assert_eq!(
        thread_by_rollout_path.permission_profile,
        thread.permission_profile
    );

    metadata.cwd = PathBuf::new();
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("update state db with empty cwd");

    for thread in [
        store
            .read_thread(ReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: false,
            })
            .await
            .expect("read thread with empty sqlite cwd"),
        store
            .read_thread_by_rollout_path(
                rollout_path.clone(),
                /*include_archived*/ false,
                /*include_history*/ false,
            )
            .await
            .expect("read rollout path with empty sqlite cwd"),
    ] {
        assert_eq!(thread.cwd, rollout_cwd);
    }

    metadata.cwd = sqlite_cwd;
    metadata.rollout_path =
        write_session_file(home.path(), "2025-01-04T12-00-00", uuid).expect("newer rollout");
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("update state db with newer rollout");

    let thread_from_requested_rollout = store
        .read_thread_by_rollout_path(
            rollout_path,
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read explicitly requested rollout");

    assert_eq!(thread_from_requested_rollout.cwd, rollout_cwd);
}

#[tokio::test]
async fn read_thread_uses_legacy_thread_name_when_sqlite_title_is_missing() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(213);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    codex_rollout::append_thread_name(home.path(), thread_id, "Legacy title")
        .await
        .expect("append legacy thread name");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.name, Some("Legacy title".to_string()));
}

#[tokio::test]
async fn read_thread_uses_sqlite_metadata_for_rollout_without_user_preview() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(217);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let day_dir = home.path().join("sessions/2025/01/03");
    std::fs::create_dir_all(&day_dir).expect("sessions dir");
    let rollout_path = day_dir.join(format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"));
    let mut file = std::fs::File::create(&rollout_path).expect("session file");
    let mut meta = serde_json::json!({
        "timestamp": "2025-01-03T12-00-00",
        "type": "session_meta",
        "payload": {
            "session_id": uuid,
            "id": uuid,
            "timestamp": "2025-01-03T12-00-00",
            "cwd": home.path(),
            "originator": "test_originator",
            "cli_version": "test_version",
            "source": "cli",
            "model_provider": "rollout-provider"
        },
    });
    writeln!(file, "{meta}").expect("write session meta");

    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        rollout_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.model_provider = Some("sqlite-provider".to_string());
    builder.cwd = home.path().join("workspace");
    builder.cli_version = Some("sqlite-cli".to_string());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.title = "Command-only thread".to_string();
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path));
    assert_eq!(thread.preview, "");
    assert_eq!(thread.name.as_deref(), Some("Command-only thread"));
    assert_eq!(thread.model_provider, "sqlite-provider");
    assert_eq!(thread.cwd, home.path().join("workspace"));
    assert_eq!(thread.cli_version, "sqlite-cli");
    assert_eq!(thread.originator.as_deref(), Some("test_originator"));
    let history = thread.history.expect("history should load");
    assert_eq!(history.thread_id, thread_id);
    assert_eq!(history.items.len(), 1);

    for (sqlite_originator, rollout_originator, expected_originator) in [
        (
            Some("saved_client"),
            "test_originator",
            Some("saved_client"),
        ),
        (None, "", None),
    ] {
        meta["payload"]["originator"] = serde_json::json!(rollout_originator);
        std::fs::write(&metadata.rollout_path, format!("{meta}\n")).expect("write session meta");
        metadata.originator = sqlite_originator.map(str::to_owned);
        let thread = stored_thread_from_sqlite_metadata(&store, metadata.clone())
            .await
            .expect("read SQLite and session metadata");
        assert_eq!(thread.originator.as_deref(), expected_originator);
    }
}

#[tokio::test]
async fn read_thread_falls_back_to_rollout_search_when_sqlite_path_is_stale() {
    let home = TempDir::new().expect("temp dir");
    let external = TempDir::new().expect("external temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(220);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let stale_path = external.path().join("missing-rollout.jsonl");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        stale_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.model_provider = Some("stale-sqlite-provider".to_string());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.first_user_message = Some("stale sqlite preview".to_string());
    metadata.model = Some("persisted-model".to_string());
    metadata.reasoning_effort = Some(ReasoningEffort::High);
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path));
    assert_eq!(thread.preview, "Hello from user");
    assert_eq!(thread.model_provider, config.default_model_provider_id);
    assert_eq!(
        (thread.model.as_deref(), thread.reasoning_effort),
        (Some("persisted-model"), Some(ReasoningEffort::High))
    );
    let history = thread.history.expect("history should load");
    assert_eq!(history.thread_id, thread_id);
    assert_eq!(history.items.len(), 2);
}

#[tokio::test]
async fn read_thread_falls_back_when_sqlite_path_points_to_another_thread() {
    let home = TempDir::new().expect("temp dir");
    let external = TempDir::new().expect("external temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(221);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T12-00-00", uuid).expect("session file");
    let other_uuid = Uuid::from_u128(222);
    let stale_path = write_session_file(external.path(), "2025-01-04T12-00-00", other_uuid)
        .expect("other session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder =
        ThreadMetadataBuilder::new(thread_id, stale_path, Utc::now(), SessionSource::Cli);
    builder.model_provider = Some("wrong-sqlite-provider".to_string());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.first_user_message = Some("wrong sqlite preview".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path));
    assert_eq!(thread.preview, "Hello from user");
    assert_eq!(thread.model_provider, config.default_model_provider_id);
    let history = thread.history.expect("history should load");
    assert_eq!(history.thread_id, thread_id);
    assert_eq!(history.items.len(), 2);
}

#[tokio::test]
async fn read_thread_uses_session_meta_for_rollout_without_user_preview_or_sqlite_metadata() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(218);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let day_dir = home.path().join("sessions/2025/01/03");
    std::fs::create_dir_all(&day_dir).expect("sessions dir");
    let rollout_path = day_dir.join(format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"));
    let mut file = std::fs::File::create(&rollout_path).expect("session file");
    let meta = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "session_id": uuid,
            "id": uuid,
            "timestamp": "2025-01-03T12:00:00Z",
            "cwd": home.path(),
            "originator": "test_originator",
            "cli_version": "test_version",
            "source": "cli",
            "model_provider": "rollout-provider"
        },
    });
    writeln!(file, "{meta}").expect("write session meta");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path));
    assert_eq!(thread.preview, "");
    assert_eq!(thread.name, None);
    assert_eq!(thread.model_provider, "rollout-provider");
    assert_eq!(
        thread.created_at,
        parse_rfc3339_non_optional("2025-01-03T12:00:00Z").unwrap()
    );
    assert!(thread.updated_at >= thread.created_at);
    assert_eq!(thread.archived_at, None);
    assert_eq!(thread.cwd, home.path());
    assert_eq!(thread.cli_version, "test_version");
    assert_eq!(thread.source, SessionSource::Cli);
    let history = thread.history.expect("history should load");
    assert_eq!(history.thread_id, thread_id);
    assert_eq!(history.items.len(), 1);
}

#[tokio::test]
async fn read_thread_falls_back_to_sqlite_summary() {
    let home = TempDir::new().expect("temp dir");
    let external = TempDir::new().expect("external temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(214);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path = external
        .path()
        .join(format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"));
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        rollout_path.clone(),
        Utc::now(),
        SessionSource::Exec,
    );
    builder.model_provider = Some("sqlite-provider".to_string());
    builder.cwd = external.path().join("workspace");
    builder.cli_version = Some("sqlite-cli".to_string());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.preview = Some("optimize the benchmark".to_string());
    metadata.first_user_message = Some("next normal prompt".to_string());
    metadata.title = "next normal prompt".to_string();
    metadata.model = Some("sqlite-model".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(rollout_path));
    assert_eq!(thread.preview, "optimize the benchmark");
    assert_eq!(
        thread.first_user_message.as_deref(),
        Some("next normal prompt")
    );
    assert_eq!(thread.name, None);
    assert_eq!(thread.model_provider, "sqlite-provider");
    assert_eq!(thread.model.as_deref(), Some("sqlite-model"));
    assert_eq!(thread.cwd, external.path().join("workspace"));
    assert_eq!(thread.cli_version, "sqlite-cli");
    assert_eq!(thread.source, SessionSource::Exec);
    assert_eq!(thread.archived_at, None);
    assert!(thread.history.is_none());
}

#[tokio::test]
async fn read_thread_sqlite_fallback_respects_include_archived() {
    let home = TempDir::new().expect("temp dir");
    let external = TempDir::new().expect("external temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(216);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path = external
        .path()
        .join(format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"));
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let mut builder =
        ThreadMetadataBuilder::new(thread_id, rollout_path, Utc::now(), SessionSource::Cli);
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    builder.archived_at = Some(Utc::now());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.first_user_message = Some("Archived SQLite preview".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let active_only_err = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect_err("active-only read should fail for archived metadata");
    let ThreadStoreError::InvalidRequest { message } = active_only_err else {
        panic!("expected invalid request error");
    };
    assert_eq!(
        message,
        format!("no rollout found for thread id {thread_id}")
    );

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: false,
        })
        .await
        .expect("read archived thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.preview, "Archived SQLite preview");
    assert!(thread.archived_at.is_some());
}

#[tokio::test]
async fn read_thread_sqlite_fallback_loads_archived_history() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(219);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let archived_path = write_archived_session_file(home.path(), "2025-01-03T12-00-00", uuid)
        .expect("archived session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        archived_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.archived_at = Some(Utc::now());
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.first_user_message = Some("Archived SQLite preview".to_string());
    runtime
        .upsert_thread(&metadata)
        .await
        .expect("state db upsert should succeed");

    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: true,
        })
        .await
        .expect("read archived thread");

    assert_eq!(thread.thread_id, thread_id);
    assert_eq!(thread.rollout_path, Some(archived_path));
    assert_eq!(thread.preview, "Archived SQLite preview");
    assert!(thread.archived_at.is_some());
    let history = thread.history.expect("history should load");
    assert_eq!(history.thread_id, thread_id);
    assert_eq!(history.items.len(), 2);
}

#[tokio::test]
async fn read_thread_fails_without_rollout() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(206);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");

    let err = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect_err("read should fail without rollout");

    let ThreadStoreError::InvalidRequest { message } = err else {
        panic!("expected invalid request error");
    };
    assert_eq!(
        message,
        format!("no rollout found for thread id {thread_id}")
    );
}
