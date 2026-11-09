use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_utils_absolute_path::test_support::PathExt;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::GitInfoPatch;
use crate::ListThreadsParams;
use crate::MoveThreadToSectionParams;
use crate::ResumeThreadParams;
use crate::SortDirection;
use crate::ThreadMetadataPatch;
use crate::ThreadPersistenceMetadata;
use crate::ThreadSortKey;
use crate::ThreadStore;
use crate::local::LocalThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_archived_session_file;
use crate::local::test_support::write_session_file;
use crate::local::test_support::write_session_file_with_history_mode;

#[tokio::test]
async fn update_thread_metadata_sets_name_on_active_rollout_and_indexes_name() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(301);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T14-00-00", uuid).expect("session file");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("A sharper name".to_string())),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set thread name")
        .expect("local store returns updated thread");

    assert_eq!(thread.name.as_deref(), Some("A sharper name"));
    let latest_name = codex_rollout::find_thread_name_by_id(home.path(), &thread_id)
        .await
        .expect("find thread name");
    assert_eq!(latest_name.as_deref(), Some("A sharper name"));
}

#[tokio::test]
async fn section_moves_persist_in_sqlite_without_changing_the_rollout() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(320);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let rollout_path =
        write_session_file(home.path(), "2025-01-03T14-20-00", uuid).expect("session file");
    let original_rollout = std::fs::read_to_string(&rollout_path).expect("read rollout");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));

    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        rollout_path.as_path(),
        "test-provider",
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    store
        .move_thread_to_section(MoveThreadToSectionParams {
            thread_id,
            section: Some(codex_state::PINNED_THREAD_SECTION_ID.to_string()),
            before_thread_id: None,
        })
        .await
        .expect("pin thread");

    let pinned = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read pinned thread");

    assert_eq!(
        pinned.section,
        Some(codex_state::ThreadSection {
            id: codex_state::PINNED_THREAD_SECTION_ID.to_string(),
            name: codex_state::PINNED_THREAD_SECTION_NAME.to_string(),
            appearance: None,
        })
    );
    let pinned_metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("read pinned metadata")
        .expect("pinned metadata");
    assert_eq!(
        pinned_metadata.section,
        Some(codex_state::ThreadSection {
            id: codex_state::PINNED_THREAD_SECTION_ID.to_string(),
            name: codex_state::PINNED_THREAD_SECTION_NAME.to_string(),
            appearance: None,
        })
    );
    assert_eq!(pinned_metadata.preview.as_deref(), Some("Hello from user"));
    assert_eq!(pinned_metadata.source, "cli");
    let pinned_page = store
        .list_threads(ListThreadsParams {
            page_size: 10,
            cursor: None,
            sort_key: ThreadSortKey::RecencyAt,
            sort_direction: SortDirection::Desc,
            allowed_sources: Vec::new(),
            model_providers: None,
            cwd_filters: None,
            section: Some(Some(codex_state::PINNED_THREAD_SECTION_ID.to_string())),
            project_id: None,
            archived: false,
            search_term: None,
            relation_filter: None,
            use_state_db_only: true,
        })
        .await
        .expect("list pinned thread");
    assert_eq!(
        pinned_page
            .items
            .iter()
            .map(|thread| thread.thread_id)
            .collect::<Vec<_>>(),
        vec![thread_id]
    );
    let read_by_path = store
        .read_thread_by_rollout_path(
            rollout_path.clone(),
            /*include_archived*/ false,
            /*include_history*/ false,
        )
        .await
        .expect("read pinned thread by rollout path");
    assert_eq!(
        read_by_path.section,
        Some(codex_state::ThreadSection {
            id: codex_state::PINNED_THREAD_SECTION_ID.to_string(),
            name: codex_state::PINNED_THREAD_SECTION_NAME.to_string(),
            appearance: None,
        })
    );
    assert_eq!(
        std::fs::read_to_string(&rollout_path).expect("read rollout"),
        original_rollout
    );

    store
        .move_thread_to_section(MoveThreadToSectionParams {
            thread_id,
            section: None,
            before_thread_id: None,
        })
        .await
        .expect("clear thread section");

    let unpinned = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read unpinned thread");

    assert_eq!(unpinned.section, None);
    assert_eq!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("read cleared metadata")
            .expect("cleared metadata")
            .section,
        None
    );
    assert_eq!(
        std::fs::read_to_string(&rollout_path).expect("read rollout"),
        original_rollout
    );
}

#[tokio::test]
async fn paginated_name_updates_use_sqlite_without_rollout_writes() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(318);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T14-15-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("session file");
    let original_rollout = std::fs::read_to_string(&path).expect("read rollout");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Canonical paginated name".to_string())),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set paginated thread name")
        .expect("local store returns updated thread");

    assert_eq!(thread.name.as_deref(), Some("Canonical paginated name"));
    let metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("read metadata")
        .expect("thread metadata");
    assert_eq!(metadata.name.as_deref(), Some("Canonical paginated name"));
    assert!(metadata.title.is_empty());
    assert_eq!(
        codex_rollout::find_thread_name_by_id(home.path(), &thread_id)
            .await
            .expect("find thread name")
            .as_deref(),
        Some("Canonical paginated name")
    );

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                title: Some("Derived first message".to_string()),
                preview: Some("Derived first message".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("apply derived paginated metadata")
        .expect("local store returns updated thread");
    assert_eq!(thread.name.as_deref(), Some("Canonical paginated name"));

    let session_index_path = home.path().join("session_index.jsonl");
    std::fs::remove_file(&session_index_path).expect("remove session index");
    std::fs::create_dir(&session_index_path).expect("block session index writes");
    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Updated SQLite name".to_string())),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set paginated thread name with unavailable index")
        .expect("local store returns updated thread");
    assert_eq!(thread.name.as_deref(), Some("Updated SQLite name"));

    let err = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None)
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Unpersistable name".to_string())),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect_err("paginated name update without sqlite should fail");

    assert!(matches!(err, ThreadStoreError::Internal { .. }));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read rollout"),
        original_rollout
    );
}

#[tokio::test]
async fn update_thread_metadata_sets_memory_mode_on_active_rollout() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(302);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file(home.path(), "2025-01-03T14-30-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Disabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set thread memory mode")
        .expect("local store returns updated thread");

    assert_eq!(thread.thread_id, thread_id);
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"]["id"], thread_id.to_string());
    assert_eq!(appended["payload"]["memory_mode"], "disabled");
    let memory_mode = runtime
        .get_thread_memory_mode(thread_id)
        .await
        .expect("thread memory mode should be readable");
    assert_eq!(memory_mode.as_deref(), Some("disabled"));
}

#[tokio::test]
async fn metadata_patch_completes_before_concurrent_shutdown()
-> Result<(), Box<dyn std::error::Error>> {
    let home = TempDir::new()?;
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::new_v4();
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let path = write_session_file(home.path(), "2025-01-03T12-00-00", uuid)?;
    store
        .resume_thread(ResumeThreadParams {
            history_revision: None,
            thread_id,
            rollout_path: Some(path.clone()),
            history: None,
            include_archived: true,
            metadata: test_thread_metadata(),
        })
        .await?;
    let writer_lock = store.live_recorders.lock().await[&thread_id]
        .writer_lock
        .clone();
    let (mut expected, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    let mut metadata = read_session_meta_line(&path).await?;
    metadata.meta.memory_mode = Some("disabled".into());
    expected.push(RolloutItem::SessionMeta(Box::new(metadata)));

    let mut shutdown = Box::pin(store.shutdown_thread(thread_id));
    update_rollout_metadata(&store, thread_id, &path, &writer_lock, |meta| {
        meta.meta.memory_mode = Some("disabled".into());
        // Poll shutdown in the read/append gap; the writer mutex must keep it pending.
        assert!(
            tokio::task::unconstrained(shutdown.as_mut())
                .now_or_never()
                .is_none()
        );
    })
    .await?;
    shutdown.await?;

    let (actual, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    assert_eq!(json!(actual), json!(expected));
    Ok(())
}

#[tokio::test]
async fn update_thread_metadata_updates_paginated_git_info_in_sqlite_only() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(303);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T14-35-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("session file");
    let original_rollout = std::fs::read_to_string(&path).expect("read rollout");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let mut stale_metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("read metadata")
        .expect("thread metadata");
    stale_metadata.history_mode = ThreadHistoryMode::Legacy;
    runtime
        .upsert_thread(&stale_metadata)
        .await
        .expect("seed stale history mode");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    sha: Some(None),
                    branch: Some(Some("feature".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("paginated metadata update")
        .expect("local store returns updated thread");

    let git_info = thread.git_info.expect("git info");
    assert_eq!(git_info.commit_hash, None);
    assert_eq!(git_info.branch.as_deref(), Some("feature"));
    assert_eq!(
        git_info.repository_url.as_deref(),
        Some("https://example.com/repo.git")
    );
    assert_eq!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("read metadata")
            .expect("thread metadata")
            .history_mode,
        ThreadHistoryMode::Paginated
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read rollout"),
        original_rollout
    );
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let thread = store
        .read_thread_by_rollout_path(
            path, /*include_archived*/ false, /*include_history*/ false,
        )
        .await
        .expect("read paginated thread by rollout path");
    let git_info = thread.git_info.expect("git info");
    assert_eq!(git_info.commit_hash, None);
    assert_eq!(git_info.branch.as_deref(), Some("feature"));
    assert_eq!(
        git_info.repository_url.as_deref(),
        Some("https://example.com/repo.git")
    );
}

#[tokio::test]
async fn update_thread_metadata_preserves_memory_mode_when_updating_git_info() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(312);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file(home.path(), "2025-01-03T18-30-00", uuid).expect("session file");
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Disabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set memory mode");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    branch: Some(Some("feature".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set git metadata")
        .expect("local store returns updated thread");

    assert_eq!(
        thread.git_info.expect("git info").branch.as_deref(),
        Some("feature")
    );
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"]["memory_mode"], "disabled");
    assert_eq!(appended["payload"]["git"]["branch"], "feature");

    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let memory_mode = runtime
        .get_thread_memory_mode(thread_id)
        .await
        .expect("thread memory mode should be readable");
    assert_eq!(memory_mode.as_deref(), Some("disabled"));
}

#[tokio::test]
async fn update_thread_metadata_uses_live_rollout_path_for_external_resume() {
    let home = TempDir::new().expect("temp dir");
    let external_home = TempDir::new().expect("external temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let uuid = Uuid::from_u128(307);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file(external_home.path(), "2025-01-03T14-45-00", uuid)
        .expect("external session file");

    store
        .resume_thread(ResumeThreadParams {
            history_revision: None,
            thread_id,
            rollout_path: Some(path.clone()),
            history: None,
            include_archived: true,
            metadata: test_thread_metadata(),
        })
        .await
        .expect("resume external live thread");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Disabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set memory mode on external live thread")
        .expect("local store returns updated thread");

    assert_eq!(thread.thread_id, thread_id);
    assert!(thread.rollout_path.is_some());
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"]["memory_mode"], "disabled");
}

#[tokio::test]
async fn update_thread_metadata_sets_git_info() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime));
    let uuid = Uuid::from_u128(309);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T17-00-00", uuid).expect("session file");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    sha: Some(Some("abc123".to_string())),
                    branch: Some(Some("main".to_string())),
                    origin_url: Some(Some(
                        SanitizedGitUrl::try_from("https://github.com/openai/codex")
                            .expect("valid git remote URL"),
                    )),
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set git metadata")
        .expect("local store returns updated thread");

    let git_info = thread.git_info.expect("git info should be present");
    assert_eq!(
        git_info.commit_hash.as_ref().map(|sha| sha.0.as_str()),
        Some("abc123")
    );
    assert_eq!(git_info.branch.as_deref(), Some("main"));
    assert_eq!(
        git_info.repository_url.as_deref(),
        Some("https://github.com/openai/codex")
    );
}

#[tokio::test]
async fn update_thread_metadata_updates_permission_profile_and_reasoning_effort() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let uuid = Uuid::from_u128(317);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T20-30-00", uuid).expect("session file");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                permission_profile: Some(PermissionProfile::Disabled),
                reasoning_effort: Some(Some(ReasoningEffort::Ultra)),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set permission profile")
        .expect("local store returns updated thread");

    assert_eq!(thread.permission_profile, PermissionProfile::Disabled);
    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                reasoning_effort: Some(None),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("clear reasoning effort")
        .expect("local store returns updated thread");

    assert_eq!(thread.reasoning_effort, None);
    let metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("sqlite metadata read")
        .expect("sqlite metadata");
    let permission_profile: PermissionProfile = PermissionProfile::Disabled;
    assert_eq!(
        metadata.sandbox_policy,
        serde_json::to_string(&permission_profile).expect("serialize profile")
    );
    assert_eq!(metadata.reasoning_effort, None);
}

#[tokio::test]
async fn update_thread_metadata_partially_updates_git_info() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime));
    let uuid = Uuid::from_u128(310);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T17-30-00", uuid).expect("session file");

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    sha: Some(Some("abc123".to_string())),
                    branch: Some(Some("main".to_string())),
                    origin_url: Some(Some(
                        SanitizedGitUrl::try_from("https://github.com/openai/codex")
                            .expect("valid git remote URL"),
                    )),
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("seed git metadata");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    branch: Some(Some("feature".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("partially update git metadata")
        .expect("local store returns updated thread");

    let git_info = thread.git_info.expect("git info should be present");
    assert_eq!(
        git_info.commit_hash.as_ref().map(|sha| sha.0.as_str()),
        Some("abc123")
    );
    assert_eq!(git_info.branch.as_deref(), Some("feature"));
    assert_eq!(
        git_info.repository_url.as_deref(),
        Some("https://github.com/openai/codex")
    );
}

#[tokio::test]
async fn update_thread_metadata_clears_git_info_fields() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    let uuid = Uuid::from_u128(311);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file(home.path(), "2025-01-03T18-00-00", uuid).expect("session file");

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    sha: Some(Some("abc123".to_string())),
                    branch: Some(Some("main".to_string())),
                    origin_url: Some(Some(
                        SanitizedGitUrl::try_from("https://github.com/openai/codex")
                            .expect("valid git remote URL"),
                    )),
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("seed git metadata");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    sha: Some(None),
                    branch: Some(None),
                    origin_url: Some(None),
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("clear git metadata")
        .expect("local store returns updated thread");

    assert!(thread.git_info.is_none());
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"]["git"], json!({}));

    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread after reconcile");
    assert!(thread.git_info.is_none());

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Disabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set memory mode after git clear");
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"].get("git"), None);
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread after memory mode update with no git");
    assert!(thread.git_info.is_none());

    assert_eq!(
        runtime
            .delete_thread(thread_id)
            .await
            .expect("delete sqlite thread row"),
        1
    );
    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                git_info: Some(GitInfoPatch {
                    branch: Some(Some("feature".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("partially update after clear with missing sqlite row")
        .expect("local store returns updated thread");
    let git_info = thread.git_info.expect("branch should be present");
    assert_eq!(git_info.commit_hash, None);
    assert_eq!(git_info.branch.as_deref(), Some("feature"));
    assert_eq!(git_info.repository_url, None);

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Disabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set memory mode after git clear and partial update");
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"].get("git"), None);
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    let thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread after memory mode update");
    let git_info = thread.git_info.expect("branch should remain present");
    assert_eq!(git_info.commit_hash, None);
    assert_eq!(git_info.branch.as_deref(), Some("feature"));
    assert_eq!(git_info.repository_url, None);
}

#[tokio::test]
async fn update_thread_metadata_rejects_mismatched_session_meta_id() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let filename_uuid = Uuid::from_u128(303);
    let metadata_uuid = Uuid::from_u128(304);
    let thread_id = ThreadId::from_string(&filename_uuid.to_string()).expect("valid thread id");
    let path = write_session_file(home.path(), "2025-01-03T15-00-00", filename_uuid)
        .expect("session file");
    let content = std::fs::read_to_string(&path).expect("read rollout");
    std::fs::write(
        &path,
        content.replace(&filename_uuid.to_string(), &metadata_uuid.to_string()),
    )
    .expect("rewrite rollout");

    let err = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Enabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect_err("mismatch should fail");

    assert!(matches!(err, ThreadStoreError::Internal { .. }));
    assert!(err.to_string().contains("metadata id mismatch"));
}

#[tokio::test]
async fn update_thread_metadata_applies_combined_explicit_patch() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let uuid = Uuid::from_u128(305);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let path = write_session_file(home.path(), "2025-01-03T15-30-00", uuid).expect("session file");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Combined metadata".to_string())),
                memory_mode: Some(ThreadMemoryMode::Disabled),
                git_info: Some(GitInfoPatch {
                    branch: Some(Some("combined".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("combined patch should apply")
        .expect("local store returns updated thread");

    assert_eq!(thread.name.as_deref(), Some("Combined metadata"));
    assert_eq!(
        thread.git_info.expect("git info").branch.as_deref(),
        Some("combined")
    );
    let appended = last_rollout_item(path.as_path());
    assert_eq!(appended["type"], "session_meta");
    assert_eq!(appended["payload"]["memory_mode"], "disabled");
    assert_eq!(appended["payload"]["git"]["branch"], "combined");
    let latest_name = codex_rollout::find_thread_name_by_id(home.path(), &thread_id)
        .await
        .expect("find thread name");
    assert_eq!(latest_name.as_deref(), Some("Combined metadata"));
    let memory_mode = runtime
        .get_thread_memory_mode(thread_id)
        .await
        .expect("thread memory mode should be readable");
    assert_eq!(memory_mode.as_deref(), Some("disabled"));
}

#[test]
fn sqlite_failures_are_best_effort_for_legacy_rollout_compat_updates() {
    assert!(!sqlite_write_failure_should_block(&ThreadMetadataPatch {
        name: Some(Some("User chosen name".to_string())),
        ..Default::default()
    }));
    assert!(!sqlite_write_failure_should_block(&ThreadMetadataPatch {
        memory_mode: Some(ThreadMemoryMode::Disabled),
        ..Default::default()
    }));
}

#[test]
fn sqlite_failures_are_best_effort_for_observed_metadata_updates() {
    assert!(!sqlite_write_failure_should_block(&ThreadMetadataPatch {
        updated_at: Some(Utc::now()),
        ..Default::default()
    }));
    assert!(!sqlite_write_failure_should_block(&ThreadMetadataPatch {
        preview: Some("Observed preview".to_string()),
        git_info: Some(GitInfoPatch {
            branch: Some(Some("main".to_string())),
            ..Default::default()
        }),
        memory_mode: Some(ThreadMemoryMode::Enabled),
        ..Default::default()
    }));
}

#[test]
fn sqlite_failures_still_block_for_explicit_git_only_updates() {
    assert!(sqlite_write_failure_should_block(&ThreadMetadataPatch {
        git_info: Some(GitInfoPatch {
            branch: Some(Some("main".to_string())),
            ..Default::default()
        }),
        ..Default::default()
    }));
}

#[tokio::test]
async fn metadata_patch_applies_title_over_existing_name() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime));
    let uuid = Uuid::from_u128(306);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T15-45-00", uuid).expect("session file");

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("User chosen name".to_string())),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set explicit name");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                title: Some("Derived first message".to_string()),
                preview: Some("Derived first message".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("apply observed metadata")
        .expect("local store returns updated thread");

    assert_eq!(thread.name.as_deref(), Some("Derived first message"));
}

#[tokio::test]
async fn metadata_patch_applies_latest_preview_and_first_user_message() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let uuid = Uuid::from_u128(313);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T19-00-00", uuid).expect("session file");

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                preview: Some("Original preview".to_string()),
                first_user_message: Some("Original first message".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("set observed metadata");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                preview: Some("Later preview".to_string()),
                first_user_message: Some("Later first message".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("apply later observed metadata")
        .expect("local store returns updated thread");

    assert_eq!(thread.preview, "Hello from user");
    assert_eq!(
        thread.first_user_message.as_deref(),
        Some("Hello from user")
    );
    let metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("sqlite metadata read")
        .expect("sqlite metadata");
    assert_eq!(metadata.preview.as_deref(), Some("Later preview"));
    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some("Later first message")
    );
}

#[tokio::test]
async fn observed_metadata_rejects_unknown_thread_without_rollout() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let uuid = Uuid::from_u128(314);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");

    let err = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                preview: Some("phantom".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect_err("metadata-only update should not create a missing thread");

    assert!(matches!(
        err,
        ThreadStoreError::InvalidRequest { message }
            if message == format!("thread not found: {thread_id}")
    ));
    let metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("sqlite metadata read");
    assert!(metadata.is_none());
}

#[tokio::test]
async fn observed_metadata_rebuilds_history_mode_from_canonical_session_meta() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(317);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T19-15-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("session file");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                preview: Some("Paginated preview".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("update paginated thread without sqlite row")
        .expect("local store returns updated thread");

    assert_eq!(thread.history_mode, ThreadHistoryMode::Paginated);
    assert_eq!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata")
            .history_mode,
        ThreadHistoryMode::Paginated
    );
}

#[tokio::test]
async fn update_thread_metadata_recreates_missing_archived_sqlite_row_as_archived() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(315);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_archived_session_file(home.path(), "2025-01-03T19-30-00", uuid)
        .expect("archived session file");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                preview: Some("Archived missing sqlite row".to_string()),
                ..Default::default()
            },
            include_archived: true,
        })
        .await
        .expect("update archived thread without sqlite row")
        .expect("local store returns updated thread");

    assert!(thread.archived_at.is_some());
    assert!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("get metadata")
            .expect("metadata")
            .archived_at
            .is_some()
    );
}

#[tokio::test]
async fn observed_metadata_normalizes_cwd_for_list_filters() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let uuid = Uuid::from_u128(316);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    write_session_file(home.path(), "2025-01-03T20-00-00", uuid).expect("session file");
    let workspace = home.path().join("workspace");
    let child = workspace.join("child");
    std::fs::create_dir_all(child.as_path()).expect("create workspace");
    let unnormalized_cwd = child.join("..");
    let normalized_cwd = codex_utils_path::normalize_for_path_comparison(workspace.as_path())
        .expect("normalize cwd");

    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                cwd: Some(unnormalized_cwd),
                preview: Some("cwd preview".to_string()),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("update observed cwd");

    let metadata = runtime
        .get_thread(thread_id)
        .await
        .expect("get metadata")
        .expect("metadata");
    assert_eq!(metadata.cwd, normalized_cwd);
    let page = store
        .list_threads(ListThreadsParams {
            page_size: 10,
            cursor: None,
            sort_key: ThreadSortKey::UpdatedAt,
            sort_direction: SortDirection::Desc,
            allowed_sources: Vec::new(),
            model_providers: Some(Vec::new()),
            cwd_filters: Some(vec![workspace]),
            section: None,
            project_id: None,
            archived: false,
            search_term: None,
            relation_filter: None,
            use_state_db_only: true,
        })
        .await
        .expect("list threads by cwd");
    assert_eq!(
        page.items
            .iter()
            .map(|thread| thread.thread_id)
            .collect::<Vec<_>>(),
        vec![thread_id]
    );
}

#[tokio::test]
async fn update_thread_metadata_keeps_archived_thread_archived_in_sqlite() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(307);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let archived_path = write_archived_session_file(home.path(), "2025-01-03T16-00-00", uuid)
        .expect("archived session file");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    runtime
        .mark_backfill_complete(/*last_watermark*/ None)
        .await
        .expect("backfill should be complete");
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        archived_path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ Some(true),
        /*new_thread_memory_mode*/ None,
    )
    .await;
    assert!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("get metadata")
            .expect("metadata")
            .archived_at
            .is_some()
    );

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Archived title".to_string())),
                ..Default::default()
            },
            include_archived: true,
        })
        .await
        .expect("set archived thread name")
        .expect("local store returns updated thread");

    assert!(thread.archived_at.is_some());
    assert!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("get metadata")
            .expect("metadata")
            .archived_at
            .is_some()
    );
}

#[tokio::test]
async fn update_thread_metadata_keeps_live_archived_thread_archived_in_sqlite() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let uuid = Uuid::from_u128(308);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
    let archived_path = write_archived_session_file(home.path(), "2025-01-03T16-30-00", uuid)
        .expect("archived session file");
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("state db should initialize");
    let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
    runtime
        .mark_backfill_complete(/*last_watermark*/ None)
        .await
        .expect("backfill should be complete");
    codex_rollout::state_db::reconcile_rollout(
        Some(runtime.as_ref()),
        archived_path.as_path(),
        config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ Some(true),
        /*new_thread_memory_mode*/ None,
    )
    .await;
    store
        .resume_thread(ResumeThreadParams {
            history_revision: None,
            thread_id,
            rollout_path: Some(archived_path.clone()),
            history: None,
            include_archived: true,
            metadata: test_thread_metadata(),
        })
        .await
        .expect("resume archived live thread");

    let thread = store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                name: Some(Some("Live archived title".to_string())),
                ..Default::default()
            },
            include_archived: true,
        })
        .await
        .expect("set archived thread name")
        .expect("local store returns updated thread");

    assert!(thread.archived_at.is_some());
    assert!(
        runtime
            .get_thread(thread_id)
            .await
            .expect("get metadata")
            .expect("metadata")
            .archived_at
            .is_some()
    );
}

fn test_thread_metadata() -> ThreadPersistenceMetadata {
    ThreadPersistenceMetadata {
        cwd: Some(std::env::current_dir().expect("cwd")),
        model_provider: "test-provider".to_string(),
        memory_mode: ThreadMemoryMode::Enabled,
    }
}

fn last_rollout_item(path: &std::path::Path) -> Value {
    let last_line = std::fs::read_to_string(path)
        .expect("read rollout")
        .lines()
        .last()
        .expect("last line")
        .to_string();
    serde_json::from_str(&last_line).expect("json line")
}
