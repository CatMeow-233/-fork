use super::*;
use chrono::Duration;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadSourceKind;
use codex_config::CONFIG_TOML_FILE;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tempfile::tempdir;

pub(super) fn page(
    rows: Vec<Row>,
    next_cursor: Option<&str>,
    num_scanned_files: usize,
    reached_scan_cap: bool,
) -> PickerPage {
    PickerPage {
        rows,
        read_states: HashMap::new(),
        history_modes: HashMap::new(),
        next_cursor: next_cursor.map(|cursor| PageCursor::AppServer(cursor.to_string())),
        num_scanned_files,
        reached_scan_cap,
    }
}

fn page_only_loader(loader: impl Fn(PageLoadRequest) + Send + Sync + 'static) -> PickerLoader {
    Arc::new(move |request| {
        if let PickerLoadRequest::Page(request) = request {
            loader(request);
        }
    })
}

async fn deliver_page(
    state: &mut PickerState,
    request: &PageLoadRequest,
    page: std::io::Result<PickerPage>,
) {
    state
        .handle_background_event(BackgroundEvent::Page {
            request_token: request.request_token,
            search_token: request.search_token,
            mode: request.mode,
            page,
        })
        .await
        .expect("page event");
}

fn ok_page(rows: Vec<Row>, next_cursor: Option<&str>) -> std::io::Result<PickerPage> {
    let n = rows.len();
    Ok(page(rows, next_cursor, n, /*reached_scan_cap*/ false))
}

pub(super) fn make_row(path: &str, ts: &str, preview: &str) -> Row {
    let timestamp = parse_timestamp_str(ts).expect("timestamp should parse");
    Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from(path)),
        preview: preview.to_string(),
        thread_id: None,
        thread_name: None,
        created_at: Some(timestamp),
        updated_at: Some(timestamp),
        cwd: None,
        git_branch: None,
    }
}

pub(super) fn local_db_first_state() -> (PickerState, Arc<Mutex<Vec<PageLoadRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let request_sink = Arc::clone(&requests);
    let loader = page_only_loader(move |request| {
        request_sink.lock().unwrap().push(request);
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.initial_page_mode = PageLoadMode::StateDbOnly;
    (state, requests)
}

fn footer_lines_text(state: &PickerState, width: u16) -> String {
    footer_hint_lines(state, width)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn footer_snapshot(state: &PickerState, width: u16, list_height: u16) -> String {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let backend = VT100Backend::new(width, /*height*/ 4);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, 4));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_picker_footer(&mut frame, area, state, list_height);
    }
    terminal.flush().expect("flush");

    terminal
        .backend()
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_picker_list(state: &PickerState, width: u16, height: u16) -> String {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, state);
    }
    terminal.flush().expect("flush");
    terminal.backend().to_string()
}

#[test]
fn row_display_preview_prefers_thread_name() {
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("first message"),
        thread_id: None,
        thread_name: Some(String::from("My session")),
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    };

    assert_eq!(row.display_preview(), "My session");
}

#[tokio::test]
async fn linked_worktree_sessions_follow_directory_scope() {
    let root = tempfile::tempdir().expect("temporary repository");
    let primary = root.path().join("primary");
    let linked = root.path().join("linked");
    let admin = primary.join(".git/worktrees/linked");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(primary.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(admin.join("commondir"), "../..").unwrap();
    std::fs::write(
        admin.join("gitdir"),
        linked.join(".git").display().to_string(),
    )
    .unwrap();
    std::fs::write(linked.join(".git"), format!("gitdir: {}", admin.display())).unwrap();
    let primary = dunce::canonicalize(primary).unwrap();
    let linked = dunce::canonicalize(linked).unwrap();
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        page_only_loader(|_| {}),
        ProviderFilter::MatchDefault("openai".to_string()),
        /*show_all*/ false,
        Some(primary.clone()),
        SessionPickerAction::Resume,
    );
    let mut row = make_row("session.jsonl", "2025-01-01T00:00:00Z", "Linked session");
    row.cwd = Some(linked.clone());
    let single = ThreadListCwdFilter::One(primary.display().to_string());
    state.repo_filter_available = local_repo_filter_available(&primary);
    state.start_initial_load();
    state.ingest_page(page(
        vec![row.clone()],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    assert!(state.filtered_rows.is_empty());
    state.change_filter_mode(ToolbarValueDirection::Next);
    state.ingest_page(page(
        vec![row.clone()],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    assert_eq!(state.filtered_rows, vec![row.clone()]);
    state
        .pagination
        .finish_load(state.next_request_token - 1)
        .expect("initial page completed");
    state.relative_time_reference =
        Some(parse_timestamp_str("2025-01-02T00:00:00Z").expect("fixed time"));
    // The retained row was filtered using its real checkout path; use a stable
    // display path so the snapshot is independent of the temporary directory.
    state.filtered_rows[0].cwd = Some(PathBuf::from("/repo/linked"));
    state.update_viewport(/*rows*/ 12, /*width*/ 100);
    assert_snapshot!(
        "resume_picker_linked_worktree",
        render_picker_list(&state, /*width*/ 100, /*height*/ 12)
    );
    assert_eq!(
        repository_cwd_filter(&primary, /*uses_remote_filesystem*/ false),
        ThreadListCwdFilter::Many(vec![
            primary.display().to_string(),
            linked.display().to_string(),
        ])
    );
    assert_eq!(
        repository_cwd_filter(&primary, /*uses_remote_filesystem*/ true),
        single
    );
    let config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(root.path().to_path_buf())
        .build()
        .await
        .unwrap();
    for (filename_ts, timestamp, cwd) in [
        ("2025-01-02T10-00-00", "2025-01-02T10:00:00Z", &primary),
        ("2025-01-02T11-00-00", "2025-01-02T11:00:00Z", &linked),
    ] {
        crate::tests::write_session_rollout(
            root.path(),
            filename_ts,
            timestamp,
            "picker session",
            &config.model_provider_id,
            cwd,
        )
        .expect("persist CLI session");
    }
    let app_server = crate::start_embedded_app_server_for_picker(&config)
        .await
        .expect("start local app-server");
    let request_handle = app_server.request_handle();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel();
    let loader = spawn_app_server_page_loader(
        /*uses_remote_filesystem*/ false,
        app_server,
        request_handle,
        /*include_non_interactive*/ false,
        RawReasoningVisibility::Hidden,
        Some(config.clone()),
        bg_tx,
    );
    let mut live_picker = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader.loader.clone(),
        ProviderFilter::MatchDefault(config.model_provider_id.clone()),
        /*show_all*/ false,
        Some(primary.clone()),
        SessionPickerAction::Resume,
    );
    live_picker.local_filter_cwd = Some(primary.clone());
    live_picker.start_initial_load();
    while live_picker.pagination.is_loading() {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), bg_rx.recv())
            .await
            .expect("app-server page response")
            .expect("page event");
        live_picker
            .handle_background_event(event)
            .await
            .expect("ingest app-server page");
    }
    assert!(
        live_picker
            .filtered_rows
            .iter()
            .all(|row| row.cwd.as_deref() == Some(primary.as_path()))
    );
    live_picker.filter_mode = SessionFilterMode::Repo;
    live_picker.start_initial_load();
    while live_picker.pagination.is_loading() {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), bg_rx.recv())
            .await
            .expect("app-server repo page response")
            .expect("repo page event");
        live_picker
            .handle_background_event(event)
            .await
            .expect("ingest app-server repo page");
    }
    let linked_index = live_picker
        .filtered_rows
        .iter()
        .position(|row| row.cwd.as_deref() == Some(linked.as_path()))
        .unwrap_or_else(|| panic!("linked session missing from {:?}", live_picker.all_rows));
    live_picker.selected = linked_index;
    let linked_id = live_picker.filtered_rows[linked_index]
        .thread_id
        .expect("listed linked session id");
    let accept = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        live_picker.handle_key(accept).await.expect("resume selection"),
        Some(SessionSelection::Resume(SessionTarget { thread_id, .. })) if thread_id == linked_id
    ));
    live_picker.action = SessionPickerAction::Fork;
    assert!(matches!(
        live_picker.handle_key(accept).await.expect("fork selection"),
        Some(SessionSelection::Fork(SessionTarget { thread_id, .. })) if thread_id == linked_id
    ));
    drop(live_picker);
    drop(bg_rx);
    loader.task.await.unwrap();
    let local = crate::latest_session_lookup_params(
        /*model_providers*/ None,
        Some(&primary),
        /*include_non_interactive*/ false,
        crate::LatestSessionLookupMode::StateDbOnly,
    );
    assert_eq!(local.cwd, Some(single.clone()));
    assert_eq!(
        crate::latest_session_lookup_params(
            /*model_providers*/ None,
            Some(&primary),
            /*include_non_interactive*/ false,
            crate::LatestSessionLookupMode::StateDbOnly,
        ),
        local
    );
    std::fs::remove_file(linked.join(".git")).unwrap();
    state.set_query("Linked".to_string());
    assert_eq!(state.filtered_rows, vec![row.clone()]);
    state.start_initial_load();
    state.ingest_page(page(
        vec![row.clone()],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    assert!(state.filtered_rows.is_empty());
    state.filter_mode = SessionFilterMode::All;
    state.apply_filter();
    assert_eq!(state.filtered_rows, vec![row.clone()]);
    state.filter_mode = SessionFilterMode::Cwd;
    state.local_filter_cwd =
        local_picker_cwd_filter(&Some(primary), /*uses_remote_filesystem*/ true);
    assert_eq!(state.local_filter_cwd, None);
    state.apply_filter();
    assert_eq!(state.filtered_rows, vec![row]);
}

#[test]
fn local_picker_thread_list_params_include_cwd_filter() {
    let cwd_filter = picker_cwd_filter(
        Path::new("/tmp/project"),
        /*show_all*/ false,
        /*uses_remote_workspace*/ false,
        /*remote_cwd_override*/ None,
    );
    let params = thread_list_params(
        Some(String::from("cursor-1")),
        cwd_filter.map(|cwd| ThreadListCwdFilter::One(cwd.to_string_lossy().into_owned())),
        SessionStatus::Active,
        ProviderFilter::MatchDefault(String::from("openai")),
        ThreadSortKey::UpdatedAt,
        /*include_non_interactive*/ false,
        /*use_state_db_only*/ true,
    );

    assert_eq!(
        params.cwd,
        Some(ThreadListCwdFilter::One(String::from("/tmp/project")))
    );
    assert!(params.use_state_db_only);
}

#[tokio::test]
async fn local_db_first_treats_empty_later_page_as_end_of_db_listing() {
    let (mut state, requests) = local_db_first_state();
    state.start_initial_load();
    let db_request = requests.lock().unwrap()[0].clone();
    deliver_page(
        &mut state,
        &db_request,
        ok_page(
            vec![make_row(
                "/tmp/indexed.jsonl",
                "2025-01-03T00:00:00Z",
                "indexed metadata",
            )],
            Some("db-cursor"),
        ),
    )
    .await;

    state.maybe_load_more_for_scroll();
    let later_db_request = requests.lock().unwrap()[1].clone();
    assert_eq!(later_db_request.mode, PageLoadMode::StateDbOnly);
    deliver_page(
        &mut state,
        &later_db_request,
        ok_page(Vec::new(), /*next_cursor*/ None),
    )
    .await;

    assert_eq!(requests.lock().unwrap().len(), 2);
    assert_eq!(state.all_rows[0].preview, "indexed metadata");
    assert!(state.pagination.next_cursor.is_none());
}

#[tokio::test]
async fn local_db_first_falls_back_when_initial_db_page_is_empty() {
    let (mut state, requests) = local_db_first_state();
    state.start_initial_load();
    let db_request = requests.lock().unwrap()[0].clone();
    deliver_page(
        &mut state,
        &db_request,
        ok_page(Vec::new(), /*next_cursor*/ None),
    )
    .await;

    let fallback_request = requests.lock().unwrap()[1].clone();
    assert_eq!(fallback_request.mode, PageLoadMode::StoreDefault);
    assert!(fallback_request.cursor.is_none());
    state.relative_time_reference =
        Some(parse_timestamp_str("2025-01-04T00:00:00Z").expect("timestamp"));
    state.update_viewport(/*rows*/ 12, /*width*/ 80);
    let render_state = |state: &PickerState| {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let backend = VT100Backend::new(/*width*/ 80, /*height*/ 12);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 12,
        ));
        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, state);
        }
        terminal.flush().expect("flush");
        terminal.backend().to_string()
    };
    let loading_snapshot = render_state(&state);

    deliver_page(
        &mut state,
        &fallback_request,
        ok_page(
            vec![
                make_row(
                    "/tmp/first.jsonl",
                    "2025-01-03T00:00:00Z",
                    "first store result",
                ),
                make_row(
                    "/tmp/repaired.jsonl",
                    "2025-01-03T00:00:00Z",
                    "repaired store result",
                ),
                make_row(
                    "/tmp/third.jsonl",
                    "2025-01-03T00:00:00Z",
                    "third store result",
                ),
            ],
            /*next_cursor*/ None,
        ),
    )
    .await;

    assert_eq!(
        state
            .all_rows
            .iter()
            .map(|row| row.preview.as_str())
            .collect::<Vec<_>>(),
        vec![
            "first store result",
            "repaired store result",
            "third store result"
        ]
    );
    assert_eq!(state.pagination.num_scanned_files, 3);
    let repaired_snapshot = render_state(&state);
    assert_snapshot!(
        "resume_picker_db_fallback_transition",
        format!(
            "---- loading fallback ----\n{loading_snapshot}\n---- repaired results ----\n{repaired_snapshot}"
        )
    );
    state.pagination.complete_page(
        Some(PageCursor::AppServer("fallback-next".to_string())),
        /*num_scanned_files*/ 0,
        /*reached_scan_cap*/ false,
    );
    assert!(matches!(
        state.pagination.next_page(),
        Some((_, PageLoadMode::StoreDefault))
    ));
}

#[tokio::test]
async fn local_db_first_accepts_db_thread_id_and_latest_history_mode() {
    let (mut state, requests) = local_db_first_state();
    state.start_initial_load();
    let db_request = requests.lock().unwrap()[0].clone();
    let thread_id = ThreadId::new();
    let mut row = make_row(
        "/tmp/missing-rollout.jsonl",
        "2025-01-01T00:00:00Z",
        "indexed metadata",
    );
    row.thread_id = Some(thread_id);
    row.cwd = Some(PathBuf::from("/tmp/saved-cwd"));
    let mut listed_page =
        ok_page(vec![row], /*next_cursor*/ None).expect("indexed thread page should be available");
    listed_page
        .history_modes
        .insert(thread_id, ThreadHistoryMode::Legacy);
    deliver_page(&mut state, &db_request, Ok(listed_page)).await;

    let mut updated_row = make_row(
        "/tmp/missing-rollout.jsonl",
        "2025-01-02T00:00:00Z",
        "updated indexed metadata",
    );
    updated_row.thread_id = Some(thread_id);
    updated_row.cwd = Some(PathBuf::from("/tmp/saved-cwd"));
    let mut updated_page = ok_page(vec![updated_row], /*next_cursor*/ None)
        .expect("updated indexed thread page should be available");
    updated_page
        .history_modes
        .insert(thread_id, ThreadHistoryMode::Paginated);
    state.ingest_page(updated_page);

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect("enter should not abort the picker");
    assert!(matches!(
        selection,
        Some(SessionSelection::Resume(SessionTarget {
            thread_id: selected_thread_id,
            cwd: Some(cwd),
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..
        })) if selected_thread_id == thread_id && cwd == Path::new("/tmp/saved-cwd")
    ));
}

#[test]
fn row_search_matches_metadata_fields() {
    let thread_id =
        ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("first message"),
        thread_id: Some(thread_id),
        thread_name: Some(String::from("My session")),
        created_at: None,
        updated_at: None,
        cwd: Some(PathBuf::from("/tmp/codex-session-picker")),
        git_branch: Some(String::from("fcoury/session-picker")),
    };

    assert!(row.matches_query("session-picker"));
    assert!(row.matches_query("fcoury"));
    assert!(row.matches_query(&thread_id.to_string()[..8]));
}

#[test]
fn relative_time_formats_zero_seconds_as_now() {
    let reference = DateTime::parse_from_rfc3339("2026-05-02T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);

    assert_eq!(format_relative_time(reference, Some(reference)), "now");
    assert_eq!(
        format_relative_time(reference, Some(reference - Duration::seconds(1))),
        "1s ago"
    );
}

#[test]
fn long_relative_time_uses_words() {
    let reference = DateTime::parse_from_rfc3339("2026-05-02T12:00:00Z")
        .expect("valid timestamp")
        .with_timezone(&Utc);

    assert_eq!(format_relative_time_long(reference, reference), "now");
    assert_eq!(
        format_relative_time_long(reference, reference - Duration::minutes(20)),
        "20 minutes ago"
    );
    assert_eq!(
        format_relative_time_long(reference, reference - Duration::hours(1)),
        "1 hour ago"
    );
}

#[test]
fn expanded_session_details_include_metadata() {
    let thread_id =
        ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.clock_format = ClockFormat::TwentyFourHour;
    state.relative_time_reference = parse_timestamp_str("2026-05-02T14:48:19Z");
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("first message"),
        thread_id: Some(thread_id),
        thread_name: Some(String::from("feat(tui): add raw scrollback mode")),
        created_at: parse_timestamp_str("2026-05-02T14:31:08Z"),
        updated_at: parse_timestamp_str("2026-05-02T14:48:19Z"),
        cwd: Some(PathBuf::from("/Users/felipe.coury/code/codex")),
        git_branch: Some(String::from("codex/raw-scrollback-mode")),
    };

    let rendered = render_expanded_session_details(&row, &state, /*width*/ 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let expected_directory =
        format_directory_display(row.cwd.as_deref().expect("cwd"), /*max_width*/ None);

    assert!(rendered.contains(
        "Session:    feat(tui): add raw scrollback mode (019dabc1-0ef5-7431-b81c-03037f51f62c)"
    ));
    assert!(rendered.contains("Created:    17 minutes ago · 2026-05-02 14:31:08"));
    assert!(rendered.contains("Updated:    now · 2026-05-02 14:48:19"));
    assert!(rendered.contains(&format!("Directory:  {expected_directory}")));
    assert!(rendered.contains("Branch:      codex/raw-scrollback-mode"));
    assert!(rendered.contains("Conversation:"));
}

#[test]
fn footer_prioritizes_active_sort_timestamp() {
    let updated = render_footer_lines(
        ThreadSortKey::UpdatedAt,
        "5h ago",
        "3h ago",
        Some("main"),
        Some("tmp/codex"),
        /*show_cwd*/ true,
        /*width*/ 80,
    );
    let created = render_footer_lines(
        ThreadSortKey::CreatedAt,
        "5h ago",
        "3h ago",
        Some("main"),
        Some("tmp/codex"),
        /*show_cwd*/ true,
        /*width*/ 80,
    );

    assert_eq!(updated.len(), 1);
    assert_eq!(created.len(), 1);
    assert!(updated[0].to_string().starts_with("  3h ago"));
    assert!(created[0].to_string().starts_with("  5h ago"));
    assert!(!updated[0].to_string().contains("created 5h ago"));
    assert!(!created[0].to_string().contains("updated 3h ago"));
    assert_metadata_order(&updated[0], "⌁ tmp/codex", " main");
    assert_metadata_order(&created[0], "⌁ tmp/codex", " main");
}

#[test]
fn footer_marks_missing_branch() {
    let footer = render_footer_lines(
        ThreadSortKey::UpdatedAt,
        "5h ago",
        "3h ago",
        /*branch*/ None,
        Some("/tmp/codex"),
        /*show_cwd*/ true,
        /*width*/ 80,
    );

    assert_eq!(footer.len(), 1);
    let rendered = footer[0].to_string();
    assert!(rendered.contains("⌁ /tmp/codex"));
    assert!(rendered.contains(" no branch"));
    assert_metadata_order(&footer[0], "⌁ /tmp/codex", " no branch");
}

#[test]
fn footer_branch_expands_when_line_has_room() {
    let branch = "etraut/animations-false-improvements";
    let footer = render_footer_lines(
        ThreadSortKey::UpdatedAt,
        "5h ago",
        "4h ago",
        Some(branch),
        Some("~/code/codex.etraut-animations-false-improvements/codex-rs"),
        /*show_cwd*/ true,
        /*width*/ 140,
    );

    assert_eq!(footer.len(), 1);
    assert!(footer[0].to_string().contains(branch));
}

#[test]
fn footer_cwd_truncates_to_responsive_column() {
    let cwd = "~/code/codex.owner-extremely-long-worktree-name-that-needs-truncating/codex-rs";
    let branch = "owner/branch";
    let footer = render_footer_lines(
        ThreadSortKey::UpdatedAt,
        "5h ago",
        "4h ago",
        Some(branch),
        Some(cwd),
        /*show_cwd*/ true,
        /*width*/ 80,
    );

    assert_eq!(footer.len(), 1);
    let footer = footer[0].to_string();
    assert!(!footer.contains(cwd));
    assert!(footer.contains("⌁ ~/code/codex."));
    assert!(footer.contains("..."));
    assert!(footer.contains(" owner/branch"));
}

#[test]
fn footer_omits_cwd_when_hidden() {
    let footer = render_footer_lines(
        ThreadSortKey::UpdatedAt,
        "5h ago",
        "4h ago",
        Some("owner/branch"),
        Some("~/code/codex.owner-worktree/codex-rs"),
        /*show_cwd*/ false,
        /*width*/ 80,
    );

    assert_eq!(footer.len(), 1);
    let footer = footer[0].to_string();
    assert!(footer.contains("4h ago"));
    assert!(footer.contains(" owner/branch"));
    assert!(!footer.contains("⌁"));
    assert!(!footer.contains("~/code"));
}

fn assert_metadata_order(line: &Line<'_>, first: &str, second: &str) {
    let rendered = line.to_string();
    let first_index = rendered.find(first).expect("first metadata item");
    let second_index = rendered.find(second).expect("second metadata item");
    assert!(first_index < second_index);
}

#[test]
fn remote_thread_list_params_omit_provider_filter() {
    let params = thread_list_params(
        Some(String::from("cursor-1")),
        Some(ThreadListCwdFilter::One("repo/on/server".to_string())),
        SessionStatus::Active,
        ProviderFilter::Any,
        ThreadSortKey::UpdatedAt,
        /*include_non_interactive*/ false,
        /*use_state_db_only*/ false,
    );

    assert_eq!(params.cursor, Some(String::from("cursor-1")));
    assert_eq!(params.model_providers, None);
    assert_eq!(
        params.source_kinds,
        Some(vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode])
    );
    assert_eq!(
        params.cwd,
        Some(ThreadListCwdFilter::One(String::from("repo/on/server")))
    );
}

#[test]
fn remote_thread_list_params_can_include_non_interactive_sources() {
    let params = thread_list_params(
        Some(String::from("cursor-1")),
        /*cwd_filter*/ None,
        SessionStatus::Active,
        ProviderFilter::Any,
        ThreadSortKey::UpdatedAt,
        /*include_non_interactive*/ true,
        /*use_state_db_only*/ false,
    );

    assert_eq!(params.cursor, Some(String::from("cursor-1")));
    assert_eq!(params.model_providers, None);
    let source_kinds = crate::resume_source_kinds(/*include_non_interactive*/ true);
    assert_eq!(params.source_kinds, Some(source_kinds));
}

#[test]
fn remote_picker_sends_cwd_filter_without_local_post_filtering() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });
    let remote_cwd = Some(PathBuf::from("/srv/link-project"));
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::Any,
        /*show_all*/ false,
        remote_cwd.clone(),
        SessionPickerAction::Resume,
    );
    state.local_filter_cwd =
        local_picker_cwd_filter(&remote_cwd, /*uses_remote_filesystem*/ true);

    state.start_initial_load();

    {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].cwd_filter, remote_cwd);
        assert_eq!(guard[0].mode, PageLoadMode::StoreDefault);
    }

    let row = Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("remote session"),
        thread_id: Some(ThreadId::new()),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: Some(PathBuf::from("/srv/real-project")),
        git_branch: None,
    };

    assert!(state.row_matches_filter(&row));
}

#[test]
fn remote_picker_does_not_filter_rows_by_local_cwd() {
    let loader = page_only_loader(|_| {});
    let state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::Any,
        /*show_all*/ false,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("remote session"),
        thread_id: Some(ThreadId::new()),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: Some(PathBuf::from("/srv/remote-project")),
        git_branch: None,
    };

    assert!(state.row_matches_filter(&row));
}

#[test]
fn mixed_backend_resume_table_snapshot() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
    let unread_id = ThreadId::from_u128(/*value*/ 2);
    let mut rows = [
        Row {
            backend_id: None,
            history_mode: None,
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("Fix resume picker timestamps"),
            thread_id: None,
            thread_name: None,
            created_at: Some(now - Duration::minutes(16)),
            updated_at: Some(now - Duration::seconds(42)),
            cwd: None,
            git_branch: None,
        },
        Row {
            backend_id: None,
            history_mode: None,
            path: Some(PathBuf::from("/tmp/b.jsonl")),
            preview: String::from("Investigate lazy pagination cap"),
            thread_id: Some(unread_id),
            thread_name: None,
            created_at: Some(now - Duration::hours(1)),
            updated_at: Some(now - Duration::minutes(35)),
            cwd: None,
            git_branch: None,
        },
        Row {
            backend_id: None,
            history_mode: None,
            path: Some(PathBuf::from("/tmp/c.jsonl")),
            preview: String::from("Explain the codebase"),
            thread_id: None,
            thread_name: None,
            created_at: Some(now - Duration::hours(2)),
            updated_at: Some(now - Duration::hours(2)),
            cwd: None,
            git_branch: None,
        },
    ];
    let local_backend = state.backend_id;
    let cloud_backend = crate::backend_id::BackendId::new();
    rows[0].backend_id = Some(cloud_backend);
    for row in &mut rows[1..] {
        row.backend_id = Some(local_backend);
    }
    let mut local_page = page(
        rows[1..].to_vec(),
        /*next_cursor*/ None,
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    );
    local_page.read_states.insert(
        unread_id.to_string(),
        codex_app_server_protocol::ThreadReadState {
            first_unread: Some(codex_app_server_protocol::ThreadUnreadPosition::ThreadStart),
            revision: "revision".into(),
        },
    );
    state.ingest_page(local_page);
    state.selected = 0;
    state.ingest_page(page(
        vec![rows[0].clone()],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    state.relative_time_reference = Some(now);
    state.scroll_top = 0;
    state.update_viewport(/*rows*/ 12, /*width*/ 80);

    assert_eq!(
        state
            .filtered_rows
            .iter()
            .map(|row| row.backend_id)
            .collect::<Vec<_>>(),
        vec![
            Some(cloud_backend),
            Some(local_backend),
            Some(local_backend)
        ]
    );
    assert_eq!(state.selected, 1);

    let snapshot = render_picker_list(&state, /*width*/ 80, /*height*/ 12);
    assert_snapshot!("resume_picker_table", snapshot);
}

#[test]
fn resume_search_error_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.inline_error = Some(String::from(
        "Failed to read session metadata from /tmp/missing.jsonl",
    ));

    let width: u16 = 80;
    let height: u16 = 1;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let line = search_line(&state, frame.area().width);
        frame.render_widget_ref(&line, frame.area());
    }
    terminal.flush().expect("flush");

    let snapshot = terminal.backend().to_string();
    assert_snapshot!("resume_picker_search_error", snapshot);
}

#[test]
fn hint_line_switches_esc_label_for_search_mode() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    assert!(footer_lines_text(&state, /*width*/ 220).contains("esc start new"));

    state.query = String::from("picker");

    assert!(footer_lines_text(&state, /*width*/ 220).contains("esc clear search"));
}

#[test]
fn hint_line_labels_cancel_keys_as_exit_for_existing_session_resume_picker() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.launch_context = SessionPickerLaunchContext::ExistingSession {
        current_thread_id: None,
    };

    let wide = footer_lines_text(&state, /*width*/ 220);
    assert!(wide.contains("esc exit"));
    assert!(wide.contains("⌃c exit"));

    let compact = footer_lines_text(&state, /*width*/ 119);
    assert!(compact.contains("esc exit"));
    assert!(compact.contains("⌃c exit"));

    state.query = String::from("picker");

    assert!(footer_lines_text(&state, /*width*/ 220).contains("esc clear search"));
}

#[test]
fn hint_line_switches_density_label() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    assert!(footer_lines_text(&state, /*width*/ 220).contains("⌃o dense view"));
    assert!(footer_lines_text(&state, /*width*/ 220).contains("⌃t transcript"));
    assert!(footer_lines_text(&state, /*width*/ 220).contains("⌃e expand"));
    state.keymap.list.move_left = vec![crate::key_hint::ctrl(KeyCode::Char('h'))];
    state.keymap.list.move_right = vec![crate::key_hint::ctrl(KeyCode::Char('l'))];
    let remapped_footer = footer_lines_text(&state, /*width*/ 220);
    assert!(
        remapped_footer.contains("⌃h/⌃l change option"),
        "{remapped_footer}"
    );
    state.keymap.list.move_left.clear();
    state.keymap.list.move_right.clear();
    assert!(!footer_lines_text(&state, /*width*/ 220).contains("change option"));

    state.density = SessionListDensity::Dense;

    assert!(footer_lines_text(&state, /*width*/ 220).contains("⌃o comfortable view"));
}

#[test]
fn hint_line_compacts_on_narrow_width() {
    let loader = page_only_loader(|_| {});
    let state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let rendered = footer_lines_text(&state, /*width*/ 119);

    assert!(rendered.contains("esc new"));
    assert!(rendered.contains("tab focus"));
    assert!(rendered.contains("←/→ option"));
    assert!(rendered.contains("⌃o dense"));
    assert!(rendered.contains("⌃t preview"));
    assert!(rendered.contains("⌃e exp"));
    assert!(!rendered.contains("focus sort/filter"));
}

#[test]
fn hint_line_snapshot_uses_distributed_wide_footer() {
    let loader = page_only_loader(|_| {});
    let state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    assert_snapshot!(
        "resume_picker_footer_wide",
        footer_snapshot(&state, /*width*/ 220, /*list_height*/ 20)
    );
}

#[test]
fn hint_line_snapshot_uses_compact_footer() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("picker");
    state.density = SessionListDensity::Dense;

    assert_snapshot!(
        "resume_picker_footer_compact",
        footer_snapshot(&state, /*width*/ 96, /*list_height*/ 20)
    );
}

#[test]
fn hint_line_preserves_primary_action_labels_when_very_narrow() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.density = SessionListDensity::Dense;

    let width = 38;
    let lines = footer_hint_lines(&state, width);
    let rendered = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(lines.iter().all(|line| line.width() <= width as usize));
    assert_eq!(
        rendered,
        " enter resume   esc new   ⌃c quit\n ⌃o comfy   ⌃t preview   ↑/↓ browse"
    );
}

#[test]
fn hint_line_shows_loading_transcript_mode() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.pending_transcript_open = Some((state.backend_id, ThreadId::new()));

    let rendered = footer_lines_text(&state, /*width*/ 80);

    assert!(rendered.contains("loading transcript"));
    assert!(rendered.contains("⌃c quit"));
    assert!(!rendered.contains("enter"));
}

#[test]
fn picker_footer_percent_reports_scroll_progress() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = (0..10)
        .map(|idx| {
            make_row(
                &format!("/tmp/{idx}.jsonl"),
                "2026-05-02T12:00:00Z",
                &format!("row {idx}"),
            )
        })
        .collect();

    state.scroll_top = 0;
    assert_eq!(picker_footer_percent(&state, /*list_height*/ 6), 0);

    state.scroll_top = state.filtered_rows.len() - 1;
    assert_eq!(picker_footer_percent(&state, /*list_height*/ 6), 100);
}

#[test]
fn picker_footer_progress_label_shows_position_total_and_percent() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = (0..10)
        .map(|idx| {
            make_row(
                &format!("/tmp/{idx}.jsonl"),
                "2026-05-02T12:00:00Z",
                &format!("row {idx}"),
            )
        })
        .collect();
    state.selected = 2;

    let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

    assert_eq!(label, " 3 / 10 · 0% ");
    assert!(!label.contains('-'));
}

#[test]
fn picker_footer_progress_label_uses_known_count_when_more_pages_exist() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = (0..10)
        .map(|idx| {
            make_row(
                &format!("/tmp/{idx}.jsonl"),
                "2026-05-02T12:00:00Z",
                &format!("row {idx}"),
            )
        })
        .collect();
    state.selected = 2;
    state.pagination.next_cursor = Some(PageCursor::AppServer(String::from("cursor-1")));

    let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

    assert_eq!(label, " 3 / 10 · 0% ");
}

#[test]
fn picker_footer_progress_label_freezes_percent_while_loading() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = (0..10)
        .map(|idx| {
            make_row(
                &format!("/tmp/{idx}.jsonl"),
                "2026-05-02T12:00:00Z",
                &format!("row {idx}"),
            )
        })
        .collect();
    state.selected = 9;
    state.scroll_top = 9;
    state.pagination.next_cursor = Some(PageCursor::AppServer(String::from("cursor-1")));
    state.pagination.start_load(
        /*request_token*/ 1,
        /*search_token*/ None,
        PageLoadMode::StoreDefault,
    );
    state.frozen_footer_percent = Some(37);

    let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

    assert_eq!(label, " 10 / 10… · 37% ");
}

#[test]
fn picker_footer_percent_is_complete_when_not_scrollable() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    assert_eq!(picker_footer_percent(&state, /*list_height*/ 20), 100);

    state.filtered_rows = vec![make_row(
        "/tmp/1.jsonl",
        "2026-05-02T12:00:00Z",
        "single row",
    )];
    assert_eq!(picker_footer_percent(&state, /*list_height*/ 20), 100);
}

#[tokio::test]
async fn ctrl_o_toggles_density_without_typing_into_search() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("pick");

    state
        .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.density, SessionListDensity::Dense);
    assert_eq!(state.query, "pick");
}

#[tokio::test]
async fn ctrl_t_requests_selected_session_transcript() {
    let thread_id = ThreadId::new();
    let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader: PickerLoader = Arc::new(move |request| {
        if let PickerLoadRequest::Transcript { thread_id, .. } = request {
            request_sink.lock().unwrap().push(thread_id);
        }
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("preview"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.density, SessionListDensity::Comfortable);
    assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);
    let key = (state.backend_id, thread_id);
    assert_eq!(state.pending_transcript_open, Some(key));
    assert!(matches!(
        state.transcript_cells.get(&key),
        Some(SessionTranscriptState::Loading)
    ));
}

#[tokio::test]
async fn transcript_loading_consumes_picker_input() {
    let loader = page_only_loader(|_| {});
    let thread_id = ThreadId::new();
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![
        Row {
            backend_id: None,
            history_mode: None,
            path: None,
            preview: String::from("one"),
            thread_id: Some(ThreadId::new()),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
        },
        Row {
            backend_id: None,
            history_mode: None,
            path: None,
            preview: String::from("two"),
            thread_id: Some(ThreadId::new()),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
        },
    ];
    state.pending_transcript_open = Some((state.backend_id, thread_id));

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(selection.is_none());
    assert_eq!(state.selected, 0);
    assert_eq!(state.query, "");

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(selection.is_none());
    assert_eq!(state.query, "");
}

#[tokio::test]
async fn escape_cancels_transcript_loading_and_restores_picker_navigation() {
    let thread_id = ThreadId::new();
    let cancellation = Arc::new(Mutex::new(None));
    let cancellation_sink = cancellation.clone();
    let loader: PickerLoader = Arc::new(move |request| {
        if let PickerLoadRequest::Transcript { cancellation, .. } = request {
            *cancellation_sink.lock().unwrap() = Some(cancellation);
        }
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let mut first = make_row("/tmp/1.jsonl", "2026-05-02T12:00:00Z", "one");
    first.thread_id = Some(thread_id);
    state.filtered_rows = vec![
        first,
        make_row("/tmp/2.jsonl", "2026-05-02T12:00:00Z", "two"),
    ];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    let key = (state.backend_id, thread_id);
    assert_eq!(state.pending_transcript_open, Some(key));

    state
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.pending_transcript_open, None);
    assert!(!state.transcript_cells.contains_key(&key));
    assert!(
        cancellation
            .lock()
            .unwrap()
            .as_mut()
            .expect("transcript cancellation receiver")
            .try_recv()
            .is_ok()
    );

    state
        .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 1);
}

#[tokio::test]
async fn transcript_loading_still_allows_ctrl_c_exit() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.pending_transcript_open = Some((state.backend_id, ThreadId::new()));

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(matches!(selection, Some(SessionSelection::Exit)));
}

#[test]
fn transcript_loading_overlay_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let thread_id = ThreadId::new();
    state.pending_transcript_open = Some((state.backend_id, thread_id));
    state.filtered_rows = vec![
        Row {
            backend_id: None,
            history_mode: None,
            path: None,
            preview: String::from("Find pending threads and emails"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
        },
        Row {
            backend_id: None,
            history_mode: None,
            path: None,
            preview: String::from("Plan raw scrollback mode"),
            thread_id: Some(ThreadId::new()),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
        },
    ];
    state.update_viewport(/*rows*/ 7, /*width*/ 80);

    let width: u16 = 80;
    let height: u16 = 7;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
        render_transcript_loading_overlay(&mut frame, area);
    }
    terminal.flush().expect("flush");

    let snapshot = terminal
        .backend()
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!("resume_picker_transcript_loading_overlay", snapshot);
}

#[tokio::test]
async fn raw_ctrl_t_requests_selected_session_transcript() {
    let thread_id = ThreadId::new();
    let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader: PickerLoader = Arc::new(move |request| {
        if let PickerLoadRequest::Transcript { thread_id, .. } = request {
            request_sink.lock().unwrap().push(thread_id);
        }
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("preview"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('\u{0014}'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);
}

#[tokio::test]
async fn ctrl_t_on_row_without_thread_id_shows_inline_error() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("preview"),
        thread_id: None,
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(
        state.inline_error.as_deref(),
        Some("No transcript available for this session")
    );
}

#[tokio::test]
async fn loaded_transcript_waits_for_loading_frame_before_opening_overlay() {
    use crate::history_cell::PlainHistoryCell;

    let thread_id = ThreadId::new();
    let (tx, mut receipts) = tokio::sync::mpsc::unbounded_channel();
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        Arc::new(move |request| tx.send(request).unwrap()),
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.keymap = RuntimeKeymap::from_config(
        &serde_json::from_value(serde_json::json!({
            "pager": {"scroll_up": ["ctrl-space x", "ctrl-x ctrl-space"]},
            "editor": {"move_line_start": ["alt-a", "ctrl-x h"]}
        }))
        .unwrap(),
    )
    .unwrap();
    let owner = BackendId::new();
    let key = (owner, thread_id);
    state.pending_transcript_open = Some(key);
    let cells: TranscriptCells = vec![Arc::new(PlainHistoryCell::new(vec!["transcript".into()]))];

    state
        .transcript_cells
        .insert(key, SessionTranscriptState::Loading);
    state
        .handle_background_event(BackgroundEvent::Transcript {
            backend_id: owner,
            thread_id,
            transcript: Ok(LoadedTranscript {
                cells,
                unread_revision: Some("observed".into()),
            }),
        })
        .await
        .unwrap();

    assert!(state.overlay.is_none());
    assert_eq!(state.pending_transcript_open, Some(key));
    assert!(matches!(
        state.transcript_cells.get(&key),
        Some(SessionTranscriptState::Loaded(..))
    ));

    assert!(state.note_transcript_loading_frame_drawn());
    state.open_pending_transcript_if_ready();

    assert!(matches!(state.overlay, Some(Overlay::Transcript(_))));
    assert_eq!(state.pending_transcript_open, None);
    let select = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert_eq!(state.route_key_chord(select), Some(select));
    assert_eq!(
        state.route_key_chord(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        None
    );
    assert!(state.chord_matcher.is_pending());
    assert_eq!(state.route_key_chord(select), Some(select));
    assert!(!state.chord_matcher.is_pending());
    let mut tui = crate::tui::test_support::make_test_tui().expect("tui");
    assert!(receipts.try_recv().is_err());
    tui.defer_screen_size(Size::new(80, 24));
    tui.screen_size_for_event(&TuiEvent::Draw).unwrap();
    state
        .handle_overlay_event(&mut tui, TuiEvent::Draw)
        .unwrap();
    assert!(
        matches!(receipts.try_recv(), Ok(PickerLoadRequest::Acknowledge { backend_id, thread_id: id, revision })
            if backend_id == owner && id == thread_id && revision == "observed")
    );
    state
        .handle_overlay_event(
            &mut tui,
            TuiEvent::Key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)),
        )
        .unwrap();
    state
        .handle_overlay_event(&mut tui, TuiEvent::Paste("tail".into()))
        .unwrap();
    for (keys, insertion) in [
        (
            vec![KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)],
            "A",
        ),
        (
            vec![
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            ],
            "B",
        ),
    ] {
        for key in keys {
            if let Some(key) = state.route_key_chord(key) {
                state
                    .handle_overlay_event(&mut tui, TuiEvent::Key(key))
                    .unwrap();
            }
        }
        state
            .handle_overlay_event(&mut tui, TuiEvent::Paste(insertion.into()))
            .unwrap();
    }
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 12,
    );
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    let Some(Overlay::Transcript(overlay)) = &mut state.overlay else {
        panic!("transcript preview");
    };
    overlay.render(area, &mut buffer);
    let text = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(text.contains("BAtail"), "Find query: {text}");
}

#[tokio::test]
async fn cached_transcript_still_shows_loading_frame_before_opening_overlay() {
    use crate::history_cell::PlainHistoryCell;

    let thread_id = ThreadId::new();
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("preview"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];
    let key = (state.backend_id, thread_id);
    state.transcript_cells.insert(
        key,
        SessionTranscriptState::Loaded(LoadedTranscript {
            cells: vec![Arc::new(PlainHistoryCell::new(vec!["transcript".into()]))],
            unread_revision: None,
        }),
    );

    state
        .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(state.overlay.is_none());
    assert_eq!(state.pending_transcript_open, Some(key));

    assert!(state.note_transcript_loading_frame_drawn());
    state.open_pending_transcript_if_ready();

    assert!(matches!(state.overlay, Some(Overlay::Transcript(_))));
    assert_eq!(state.pending_transcript_open, None);
}

#[tokio::test]
async fn ctrl_o_persists_density_preference() {
    let tmp = tempdir().expect("tmpdir");
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.view_persistence = Some(SessionPickerViewPersistence {
        codex_home: tmp.path().to_path_buf(),
    });

    state
        .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.density, SessionListDensity::Dense);
    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert_eq!(
        contents,
        r#"[tui]
session_picker_view = "dense"
"#
    );
}

#[tokio::test]
async fn ctrl_o_keeps_toggled_density_when_persistence_fails() {
    let tmp = tempdir().expect("tmpdir");
    let codex_home_file = tmp.path().join("codex-home-file");
    std::fs::write(&codex_home_file, "not a directory").expect("write codex home file");
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.view_persistence = Some(SessionPickerViewPersistence {
        codex_home: codex_home_file,
    });

    state
        .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.density, SessionListDensity::Dense);
    assert!(
        state
            .inline_error
            .as_deref()
            .is_some_and(|error| error.contains("Failed to save view mode")),
        "expected persistence error, got {:?}",
        state.inline_error
    );
}

#[tokio::test]
async fn raw_ctrl_o_toggles_density_without_typing_into_search() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("pick");

    state
        .handle_key(KeyEvent::new(KeyCode::Char('\u{000f}'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.density, SessionListDensity::Dense);
    assert_eq!(state.query, "pick");
}

#[tokio::test]
async fn space_appends_to_search_query() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("resize");

    state
        .handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    state
        .handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.query, "resize r");
    assert_eq!(state.expanded_thread_id, None);
}

#[tokio::test]
async fn ctrl_e_toggles_selected_session_expansion() {
    let thread_id = ThreadId::new();
    let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader: PickerLoader = Arc::new(move |request| {
        if let PickerLoadRequest::Preview { thread_id, .. } = request {
            request_sink.lock().unwrap().push(thread_id);
        }
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("preview"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.expanded_thread_id, Some(thread_id));
    assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);

    state
        .handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(state.expanded_thread_id, None);
}

#[tokio::test]
async fn raw_ctrl_e_toggles_selected_session_expansion() {
    let thread_id = ThreadId::new();
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("preview"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    }];

    state
        .handle_key(KeyEvent::new(KeyCode::Char('\u{0005}'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.expanded_thread_id, Some(thread_id));
}

#[test]
fn toolbar_renders_sort_and_filter_tabs() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ false,
        Some(PathBuf::from("/tmp/project")),
        SessionPickerAction::Resume,
    );
    state.set_repo_filter_available(/*available*/ true);

    let width: u16 = 100;
    let backend = VT100Backend::new(width, /*height*/ 1);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, 1));

    {
        let mut frame = terminal.get_frame();
        let line = toolbar_for_width(&state, frame.area().width);
        frame.render_widget_ref(&line, frame.area());
    }
    terminal.flush().expect("flush");

    assert_snapshot!(
        "resume_picker_search_line_sort_filter_tabs",
        terminal.backend().to_string()
    );
}

#[test]
fn toolbar_compacts_on_narrow_width() {
    let loader = page_only_loader(|_| {});
    let state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ false,
        Some(PathBuf::from("/tmp/project")),
        SessionPickerAction::Resume,
    );

    let line = toolbar_for_width(&state, /*width*/ 40).to_string();

    assert!(line.contains("Filter: Cwd "));
    assert!(line.contains(" Active "));
    assert!(line.contains("Sort: Updated "));
    assert!(line.find("Filter: Cwd ") < line.find("Sort: Updated "));
}

fn dense_snapshot_row() -> Row {
    Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from(
            "Propose session picker redesign with enough title text to exercise truncation",
        ),
        thread_id: Some(
            ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id"),
        ),
        thread_name: None,
        created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
        updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
        cwd: Some(PathBuf::from(
            "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs",
        )),
        git_branch: Some(String::from("fcoury/session-picker")),
    }
}

fn render_dense_row_snapshot(show_all: bool, filter_cwd: Option<PathBuf>, width: u16) -> String {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let row = dense_snapshot_row();
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        show_all,
        filter_cwd,
        SessionPickerAction::Resume,
    );
    state.density = SessionListDensity::Dense;
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

    let backend = VT100Backend::new(width, /*height*/ 3);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, 3));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    terminal.backend().to_string()
}

#[test]
fn dense_session_snapshot_omits_cwd_in_cwd_filter() {
    assert_snapshot!(
        "resume_picker_dense_cwd",
        render_dense_row_snapshot(
            /*show_all*/ false,
            Some(PathBuf::from(
                "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs"
            )),
            /*width*/ 100,
        )
    );
}

#[test]
fn dense_session_snapshot_drops_metadata_when_narrow() {
    assert_snapshot!(
        "resume_picker_dense_narrow",
        render_dense_row_snapshot(
            /*show_all*/ true, /*filter_cwd*/ None, /*width*/ 48,
        )
    );
}

#[test]
fn dense_session_line_prefers_thread_name_over_preview() {
    let mut row = dense_snapshot_row();
    row.preview = String::from("Raw conversation preview");
    row.thread_name = Some(String::from("Named session"));

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

    let rendered = render_dense_session_lines(
        &row, &state, /*is_selected*/ false, /*is_expanded*/ false,
        /*is_zebra*/ false, /*width*/ 100,
    )
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    assert!(rendered.contains("Named session"));
    assert!(!rendered.contains("Raw conversation preview"));
}

#[test]
fn dense_selected_summary_line_uses_full_width_selection_style() {
    let line = dense_summary_line(DenseSummaryInput {
        unread: false,
        marker: selection_marker(/*is_selected*/ true, /*is_expanded*/ false),
        date: "15m ago",
        title: "Selected dense row",
        thread_id: None,
        use_theme_colors: true,
        is_selected: true,
        is_zebra: false,
        width: 80,
    });

    assert_eq!(line.width(), 80);
    assert_eq!(line.style.fg, selected_session_style().fg);
    assert_eq!(line.spans[0].content, "› ");
}

#[test]
fn dense_zebra_summary_line_uses_full_width_background() {
    let line = dense_summary_line(DenseSummaryInput {
        unread: false,
        marker: selection_marker(/*is_selected*/ false, /*is_expanded*/ false),
        date: "15m ago",
        title: "Zebra dense row",
        thread_id: None,
        use_theme_colors: true,
        is_selected: false,
        is_zebra: true,
        width: 80,
    });

    assert_eq!(line.width(), 80);
    assert_eq!(line.style.bg, dense_zebra_style().bg);
}

#[test]
fn comfortable_zebra_lines_use_full_width_background() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-05-02T12:00:00Z").expect("timestamp"));
    let row = make_row(
        "/tmp/a.jsonl",
        "2026-05-02T11:45:00Z",
        "Zebra comfortable row",
    );

    let lines = render_comfortable_session_lines(
        &row, &state, /*is_selected*/ false, /*is_expanded*/ false,
        /*is_zebra*/ true, /*width*/ 100,
    );

    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.width() == 100));
    assert!(
        lines
            .iter()
            .all(|line| line.style.bg == dense_zebra_style().bg)
    );
}

#[test]
fn dense_session_snapshot_uses_no_blank_lines_between_rows() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut first = dense_snapshot_row();
    first.preview = String::from("First dense row");
    let mut second = dense_snapshot_row();
    second.preview = String::from("Second dense row");
    second.git_branch = Some(String::from("fcoury/other-branch"));
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ false,
        Some(PathBuf::from(
            "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs",
        )),
        SessionPickerAction::Resume,
    );
    state.density = SessionListDensity::Dense;
    state.all_rows = vec![first.clone(), second.clone()];
    state.filtered_rows = vec![first, second];
    state.selected = 1;
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

    let backend = VT100Backend::new(/*width*/ 80, /*height*/ 2);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, 80, 2));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    assert_snapshot!(
        "resume_picker_dense_no_blank_lines",
        terminal.backend().to_string()
    );
}

#[test]
fn expanded_session_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let thread_id =
        ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("Investigate picker expansion"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
        updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
        cwd: Some(PathBuf::from("/tmp/codex")),
        git_branch: Some(String::from("fcoury/session-picker")),
    };
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.clock_format = ClockFormat::TwelveHour;
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));
    state.expanded_thread_id = Some(thread_id);
    state.transcript_previews.insert(
            thread_id,
            TranscriptPreviewState::Loaded(vec![
                TranscriptPreviewLine {
                    speaker: TranscriptPreviewSpeaker::User,
                    text: String::from("Show me the recent transcript"),
                },
                TranscriptPreviewLine {
                    speaker: TranscriptPreviewSpeaker::Assistant,
                    text: String::from(
                        r#"Here are the *last* lines: [docs](https://example.com) :codex-file-citation{path="/tmp/codex/report.xlsx"}."#,
                    ),
                },
            ]),
        );

    let width: u16 = 90;
    let height: u16 = 11;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    let rendered = terminal
        .backend()
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!("resume_picker_expanded_session", rendered);
}

#[test]
fn narrow_session_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/a.jsonl")),
        preview: String::from("Investigate picker expansion"),
        thread_id: Some(
            ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id"),
        ),
        thread_name: None,
        created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
        updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
        cwd: Some(PathBuf::from("/tmp/codex")),
        git_branch: Some(String::from("fcoury/session-picker")),
    };
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];
    state.relative_time_reference =
        Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

    let width: u16 = 58;
    let height: u16 = 6;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    assert_snapshot!(
        "resume_picker_narrow_session",
        terminal.backend().to_string()
    );
}

#[test]
fn session_list_more_indicators_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
    state.all_rows = (0..5)
        .map(|idx| Row {
            backend_id: None,
            history_mode: None,
            path: Some(PathBuf::from(format!("/tmp/{idx}.jsonl"))),
            preview: format!("item-{idx}"),
            thread_id: None,
            thread_name: None,
            created_at: Some(now - Duration::hours(idx)),
            updated_at: Some(now - Duration::minutes(idx * 5)),
            cwd: None,
            git_branch: None,
        })
        .collect();
    state.filtered_rows = state.all_rows.clone();
    state.relative_time_reference = Some(now);
    state.selected = 2;
    state.scroll_top = 1;
    state.update_viewport(/*rows*/ 6, /*width*/ 80);

    let width: u16 = 80;
    let height: u16 = 6;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    assert_snapshot!(
        "resume_picker_more_indicators",
        terminal.backend().to_string()
    );
}

#[test]
fn density_toggle_clears_stale_more_indicator() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
    state.all_rows = (0..4)
        .map(|idx| Row {
            backend_id: None,
            history_mode: None,
            path: Some(PathBuf::from(format!("/tmp/{idx}.jsonl"))),
            preview: format!("item-{idx}"),
            thread_id: None,
            thread_name: None,
            created_at: Some(now - Duration::hours(idx)),
            updated_at: Some(now - Duration::minutes(idx * 5)),
            cwd: None,
            git_branch: None,
        })
        .collect();
    state.filtered_rows = state.all_rows.clone();
    state.relative_time_reference = Some(now);

    let width: u16 = 80;
    let height: u16 = 6;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    state.update_viewport(height as usize, width);
    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");
    terminal.swap_buffers();
    assert!(terminal.backend().to_string().contains("↓ more"));

    state.density = SessionListDensity::Dense;
    state.update_viewport(height as usize, width);
    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        render_list(&mut frame, area, &state);
    }
    terminal.flush().expect("flush");

    assert!(!terminal.backend().to_string().contains("↓ more"));
}

#[test]
fn pageless_scrolling_deduplicates_and_keeps_order() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_row("/tmp/a.jsonl", "2025-01-03T00:00:00Z", "third"),
            make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "second"),
        ],
        Some("2025-01-02T00:00:00Z"),
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));

    state.ingest_page(page(
        vec![
            make_row("/tmp/a.jsonl", "2025-01-03T00:00:00Z", "duplicate"),
            make_row("/tmp/c.jsonl", "2025-01-01T00:00:00Z", "first"),
        ],
        Some("2025-01-01T00:00:00Z"),
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));

    state.ingest_page(page(
        vec![make_row("/tmp/d.jsonl", "2024-12-31T23:00:00Z", "very old")],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));

    let previews: Vec<_> = state
        .filtered_rows
        .iter()
        .map(|row| row.preview.as_str())
        .collect();
    assert_eq!(previews, vec!["third", "second", "first", "very old"]);

    let unique_paths = state
        .filtered_rows
        .iter()
        .map(|row| row.path.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_paths.len(), 4);
}

#[test]
fn page_ingestion_globally_sorts_rows_and_preserves_selection() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.sort_key = ThreadSortKey::CreatedAt;
    state.reset_pagination();
    state.ingest_page(page(
        vec![make_row(
            "/tmp/selected.jsonl",
            "2025-01-02T00:00:00Z",
            "selected",
        )],
        Some("2025-01-02T00:00:00Z"),
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));

    let mut newest_update = make_row(
        "/tmp/newest-update.jsonl",
        "2025-01-01T00:00:00Z",
        "newest update",
    );
    newest_update.updated_at = parse_timestamp_str("2025-01-04T00:00:00Z");
    state.ingest_page(page(
        vec![
            make_row(
                "/tmp/newest-created.jsonl",
                "2025-01-03T00:00:00Z",
                "newest created",
            ),
            newest_update,
        ],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));

    assert_eq!(
        state
            .filtered_rows
            .iter()
            .map(|row| row.preview.as_str())
            .collect::<Vec<_>>(),
        vec!["newest created", "selected", "newest update"]
    );
    assert_eq!(state.filtered_rows[state.selected].preview, "selected");

    state.sort_key = ThreadSortKey::UpdatedAt;
    state.ingest_page(page(
        Vec::new(),
        /*next_cursor*/ None,
        /*num_scanned_files*/ 0,
        /*reached_scan_cap*/ false,
    ));
    assert_eq!(
        state
            .filtered_rows
            .iter()
            .map(|row| row.preview.as_str())
            .collect::<Vec<_>>(),
        vec!["newest update", "newest created", "selected"]
    );
    assert_eq!(state.filtered_rows[state.selected].preview, "selected");
}

#[test]
fn ensure_minimum_rows_prefetches_when_underfilled() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
            make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
        ],
        Some("2025-01-03T00:00:00Z"),
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));

    assert!(recorded_requests.lock().unwrap().is_empty());
    state.ensure_minimum_rows_for_view(/*minimum_rows*/ 10);
    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert!(guard[0].search_token.is_none());
}

#[test]
fn ensure_minimum_rows_does_not_prefetch_when_comfortable_cards_fill_view() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
            make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
            make_row("/tmp/c.jsonl", "2025-01-03T00:00:00Z", "three"),
            make_row("/tmp/d.jsonl", "2025-01-04T00:00:00Z", "four"),
        ],
        Some("2025-01-05T00:00:00Z"),
        /*num_scanned_files*/ 4,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 6, /*width*/ 80);

    state.ensure_minimum_rows_for_view(/*minimum_rows*/ 6);

    assert!(recorded_requests.lock().unwrap().is_empty());
}

#[test]
fn ensure_minimum_rows_still_prefetches_when_dense_rows_underfill_view() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.density = SessionListDensity::Dense;
    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
            make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
        ],
        Some("2025-01-03T00:00:00Z"),
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 10, /*width*/ 80);

    state.ensure_minimum_rows_for_view(/*minimum_rows*/ 10);

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert!(guard[0].search_token.is_none());
}

#[test]
fn list_viewport_width_matches_rendered_list_inset() {
    assert_eq!(list_viewport_width(/*width*/ 80), 76);
    assert_eq!(list_viewport_width(/*width*/ 3), 0);
}

#[tokio::test]
async fn toggle_sort_key_reloads_with_new_sort() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    state.start_initial_load();
    {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].sort_key, ThreadSortKey::UpdatedAt);
    }

    state
        .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    state
        .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    state
        .handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard[1].sort_key, ThreadSortKey::CreatedAt);
}

#[tokio::test]
async fn default_filter_focus_arrows_reload_with_new_filter() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ false,
        Some(PathBuf::from("/tmp/project")),
        SessionPickerAction::Resume,
    );

    state.start_initial_load();
    {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].cwd_filter, Some(PathBuf::from("/tmp/project")));
    }

    state
        .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.filter_mode, SessionFilterMode::All);
    state.set_repo_filter_available(/*available*/ true);
    assert_eq!(state.filter_mode, SessionFilterMode::All);

    let cwd_filter = Some(Path::new("/tmp/project"));
    for (key, filter_mode, expected_cwd) in [
        (KeyCode::Left, SessionFilterMode::Repo, cwd_filter),
        (KeyCode::Left, SessionFilterMode::Cwd, cwd_filter),
        (KeyCode::Right, SessionFilterMode::Repo, cwd_filter),
        (KeyCode::Right, SessionFilterMode::All, None),
    ] {
        state
            .handle_key(KeyEvent::new(key, KeyModifiers::NONE))
            .await
            .unwrap();
        let guard = recorded_requests.lock().unwrap();
        let request = guard.last().expect("filter reload request");
        assert_eq!(
            (request.filter_mode, request.cwd_filter.as_deref()),
            (filter_mode, expected_cwd)
        );
    }
}

#[tokio::test]
async fn all_filter_can_switch_back_to_cwd_when_cwd_candidate_exists() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        Some(PathBuf::from("/tmp/project")),
        SessionPickerAction::Resume,
    );

    state.start_initial_load();
    {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].cwd_filter, None);
    }

    state
        .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard[1].cwd_filter, Some(PathBuf::from("/tmp/project")));
}

#[tokio::test]
async fn status_changes_when_directory_filter_is_unavailable() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::Any,
        /*show_all*/ false,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    assert_eq!(
        toolbar_for_width(&state, /*width*/ 80)
            .to_string()
            .matches("Cwd")
            .count(),
        0
    );

    state.start_initial_load();
    state
        .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    state
        .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    state
        .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard[0].status, SessionStatus::Active);
    assert_eq!(guard[0].cwd_filter, None);
    assert_eq!(guard[1].status, SessionStatus::Archived);
    assert_eq!(guard[1].cwd_filter, None);
}

#[tokio::test]
async fn page_navigation_uses_view_rows() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let mut items = Vec::new();
    for idx in 0..20 {
        let ts = format!("2025-01-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 20,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    assert_eq!(state.selected, 0);
    state
        .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 5);

    state
        .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 10);

    state
        .handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 5);

    state
        .handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 19);

    state
        .handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 0);

    state.selected = 9;
    state.pending_page_down_target = Some(19);
    let newer_items = (20..50)
        .map(|idx| {
            let timestamp = format!("2026-01-{:02}T00:00:00Z", idx - 19);
            make_row(
                &format!("/tmp/item-{idx}.jsonl"),
                &timestamp,
                &format!("item-{idx}"),
            )
        })
        .collect();
    state.ingest_page(page(
        newer_items,
        /*next_cursor*/ None,
        /*num_scanned_files*/ 30,
        /*reached_scan_cap*/ false,
    ));
    assert_eq!(state.selected, 39);
    assert_eq!(state.pending_page_down_target, Some(49));
}

#[tokio::test]
async fn page_and_jump_navigation_use_list_keymap() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.keymap.list.page_down = vec![crate::key_hint::ctrl(KeyCode::Char('d'))];
    state.keymap.list.page_up = vec![crate::key_hint::ctrl(KeyCode::Char('u'))];
    state.keymap.list.jump_bottom = vec![crate::key_hint::ctrl(KeyCode::Char('y'))];
    state.keymap.list.jump_top = vec![crate::key_hint::ctrl(KeyCode::Char('a'))];

    let mut items = Vec::new();
    for idx in 0..20 {
        let ts = format!("2025-01-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 20,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    state
        .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 0);

    state
        .handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(state.selected, 5);

    state
        .handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(state.selected, 0);

    state
        .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(state.selected, 19);

    state
        .handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(state.selected, 0);
}

#[tokio::test]
async fn ctrl_c_exits_even_when_cancel_is_remapped_to_ctrl_c() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.keymap.list.cancel = vec![crate::key_hint::ctrl(KeyCode::Char('c'))];

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(matches!(selection, Some(SessionSelection::Exit)));
}

#[tokio::test]
async fn end_jumps_to_last_known_row_and_starts_loading_more() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let items = (0..10)
        .map(|idx| {
            make_row(
                &format!("/tmp/{idx}.jsonl"),
                "2026-05-02T12:00:00Z",
                &format!("row {idx}"),
            )
        })
        .collect();
    state.reset_pagination();
    state.ingest_page(page(
        items,
        Some("cursor-1"),
        /*num_scanned_files*/ 10,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    state
        .handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.selected, 9);
    assert!(state.pagination.is_loading());
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);
    assert_eq!(
        picker_footer_progress_label(&state, /*list_height*/ 5, /*width*/ 80),
        " 10 / 10… · 100% "
    );
}

#[tokio::test]
async fn enter_on_row_without_resolvable_thread_id_shows_inline_error() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let row = Row {
        backend_id: None,
        history_mode: None,
        path: Some(PathBuf::from("/tmp/missing.jsonl")),
        preview: String::from("missing metadata"),
        thread_id: None,
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    };
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect("enter should not abort the picker");

    assert!(selection.is_none());
    assert_eq!(
        state.inline_error,
        Some(String::from(
            "Failed to read session metadata from /tmp/missing.jsonl"
        ))
    );
}

#[tokio::test]
async fn enter_on_pathless_thread_uses_thread_id() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    let thread_id = ThreadId::new();
    let row = Row {
        backend_id: None,
        history_mode: None,
        path: None,
        preview: String::from("pathless thread"),
        thread_id: Some(thread_id),
        thread_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
    };
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect("enter should not abort the picker");

    match selection {
        Some(SessionSelection::Resume(SessionTarget {
            backend_id: _,
            path: None,
            thread_id: selected_thread_id,
            cwd: None,
            history_mode: None,
        })) => assert_eq!(selected_thread_id, thread_id),
        other => panic!("unexpected selection: {other:?}"),
    }
}

#[test]
fn app_server_row_keeps_pathless_threads() {
    let thread_id = ThreadId::new();
    let thread = Thread {
        originator: None,
        environments: None,
        id: thread_id.to_string(),
        extra: None,
        session_id: thread_id.to_string(),
        forked_from_id: None,
        parent_thread_id: None,
        preview: String::from("remote thread"),
        ephemeral: false,
        section: None,
        section_entered_at: None,
        project_id: None,
        daybreak_enabled: None,
        history_mode: Default::default(),
        model_provider: String::from("openai"),
        model: None,
        reasoning_effort: None,
        created_at: 1,
        updated_at: 2,
        recency_at: Some(2),
        status: codex_app_server_protocol::ThreadStatus::Idle,
        path: None,
        cwd: test_path_buf("/tmp").abs(),
        cli_version: String::from("0.0.0"),
        source: codex_app_server_protocol::SessionSource::Cli,
        can_accept_direct_input: None,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: Some(String::from("Named thread")),
        turns: Vec::new(),
    };

    let row = row_from_app_server_thread(thread).expect("row should be preserved");

    assert_eq!(row.path, None);
    assert_eq!(row.thread_id, Some(thread_id));
    assert_eq!(row.thread_name, Some(String::from("Named thread")));
}

#[test]
fn thread_to_transcript_cells_renders_core_message_types() {
    use crate::thread_transcript::thread_to_transcript_cells;

    let thread_id = ThreadId::new();
    let mut thread = Thread {
        originator: None,
        environments: None,
        id: thread_id.to_string(),
        extra: None,
        session_id: thread_id.to_string(),
        forked_from_id: None,
        parent_thread_id: None,
        preview: String::from("preview"),
        ephemeral: false,
        section: None,
        section_entered_at: None,
        project_id: None,
        daybreak_enabled: None,
        history_mode: Default::default(),
        model_provider: String::from("openai"),
        model: None,
        reasoning_effort: None,
        created_at: 1,
        updated_at: 2,
        recency_at: Some(2),
        status: codex_app_server_protocol::ThreadStatus::Idle,
        path: None,
        cwd: test_path_buf("/tmp").abs(),
        cli_version: String::from("0.0.0"),
        source: codex_app_server_protocol::SessionSource::Cli,
        can_accept_direct_input: None,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: None,
        turns: vec![codex_app_server_protocol::Turn {
            id: String::from("turn-1"),
            root_turn_id: None,
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: vec![
                ThreadItem::untimed(ThreadItemPayload::UserMessage {
                    id: String::from("user-1"),
                    client_id: None,
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: String::from("hello from user"),
                        text_elements: Vec::new(),
                    }],
                }),
                ThreadItem::untimed(ThreadItemPayload::AgentMessage {
                    moderation: None,
                    id: String::from("agent-1"),
                    text: String::from("hello from assistant"),
                    phase: None,
                    memory_citation: None,
                    delivery: None,
                    questions: None,
                }),
                ThreadItem::untimed(ThreadItemPayload::Plan {
                    id: String::from("plan-1"),
                    text: String::from("1. Do the thing"),
                }),
            ],
            status: codex_app_server_protocol::TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }],
    };

    thread.turns.push(codex_app_server_protocol::Turn {
        id: String::from("turn-2"),
        items: Vec::new(),
        status: codex_app_server_protocol::TurnStatus::Failed,
        error: Some(codex_app_server_protocol::TurnError {
            message: String::from("Connection lost"),
            codex_error_info: None,
            additional_details: None,
            misalignment: None,
        }),
        ..thread.turns[0].clone()
    });
    let rendered = thread_to_transcript_cells(
        thread,
        RawReasoningVisibility::Visible,
        /*config*/ None,
    )
    .into_iter()
    .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    assert!(rendered.contains("hello from user"));
    assert!(rendered.contains("hello from assistant"));
    assert!(rendered.contains("Proposed Plan"));
    assert!(rendered.contains("Do the thing"));
    insta::assert_snapshot!(rendered.lines().last().unwrap(), @"■ Connection lost");
}

#[test]
fn thread_to_transcript_cells_hides_raw_reasoning_when_not_enabled() {
    use crate::thread_transcript::thread_to_transcript_cells;

    let thread_id = ThreadId::new();
    let thread = Thread {
        originator: None,
        environments: None,
        id: thread_id.to_string(),
        extra: None,
        session_id: thread_id.to_string(),
        forked_from_id: None,
        parent_thread_id: None,
        preview: String::from("preview"),
        ephemeral: false,
        section: None,
        section_entered_at: None,
        project_id: None,
        daybreak_enabled: None,
        history_mode: Default::default(),
        model_provider: String::from("openai"),
        model: None,
        reasoning_effort: None,
        created_at: 1,
        updated_at: 2,
        recency_at: Some(2),
        status: codex_app_server_protocol::ThreadStatus::Idle,
        path: None,
        cwd: test_path_buf("/tmp").abs(),
        cli_version: String::from("0.0.0"),
        source: codex_app_server_protocol::SessionSource::Cli,
        can_accept_direct_input: None,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: None,
        turns: vec![codex_app_server_protocol::Turn {
            id: String::from("turn-1"),
            root_turn_id: None,
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: vec![ThreadItem::untimed(ThreadItemPayload::Reasoning {
                id: String::from("reasoning-1"),
                summary: Vec::new(),
                content: vec![String::from("private raw chain of thought")],
            })],
            status: codex_app_server_protocol::TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }],
    };

    let hidden = thread_to_transcript_cells(
        thread.clone(),
        RawReasoningVisibility::Hidden,
        /*config*/ None,
    )
    .into_iter()
    .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let visible = thread_to_transcript_cells(
        thread,
        RawReasoningVisibility::Visible,
        /*config*/ None,
    )
    .into_iter()
    .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    assert!(!hidden.contains("private raw chain of thought"));
    assert!(visible.contains("private raw chain of thought"));
}

#[test]
fn thread_to_transcript_cells_retains_summary_and_raw_reasoning_when_enabled() {
    use crate::thread_transcript::thread_to_transcript_cells;

    let thread_id = ThreadId::new();
    let thread = Thread {
        originator: None,
        environments: None,
        id: thread_id.to_string(),
        extra: None,
        session_id: thread_id.to_string(),
        forked_from_id: None,
        parent_thread_id: None,
        preview: String::from("preview"),
        ephemeral: false,
        section: None,
        section_entered_at: None,
        project_id: None,
        daybreak_enabled: None,
        history_mode: Default::default(),
        model_provider: String::from("openai"),
        model: None,
        reasoning_effort: None,
        created_at: 1,
        updated_at: 2,
        recency_at: Some(2),
        status: codex_app_server_protocol::ThreadStatus::Idle,
        path: None,
        cwd: test_path_buf("/tmp").abs(),
        cli_version: String::from("0.0.0"),
        source: codex_app_server_protocol::SessionSource::Cli,
        can_accept_direct_input: None,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: None,
        turns: vec![codex_app_server_protocol::Turn {
            id: String::from("turn-1"),
            root_turn_id: None,
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: vec![ThreadItem::untimed(ThreadItemPayload::Reasoning {
                id: String::from("reasoning-1"),
                summary: vec![String::from("public summary")],
                content: vec![String::from("raw reasoning content")],
            })],
            status: codex_app_server_protocol::TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }],
    };

    let rendered = thread_to_transcript_cells(
        thread,
        RawReasoningVisibility::Visible,
        /*config*/ None,
    )
    .into_iter()
    .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    assert_eq!(rendered, "• public summary\n  \n  raw reasoning content");
}

#[tokio::test]
async fn moving_to_last_card_scrolls_when_cards_exceed_viewport() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let mut items = Vec::new();
    for idx in 0..3 {
        let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 3,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    state
        .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.scroll_top, 1);

    state
        .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.selected, 2);
    assert_eq!(state.scroll_top, 2);
}

#[tokio::test]
async fn up_from_bottom_keeps_viewport_stable_when_card_remains_visible() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let mut items = Vec::new();
    for idx in 0..10 {
        let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 10,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    state.selected = state.filtered_rows.len().saturating_sub(1);
    state.ensure_selected_visible();

    let initial_top = state.scroll_top;
    assert_eq!(initial_top, state.filtered_rows.len().saturating_sub(1));

    state
        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.scroll_top, initial_top.saturating_sub(1));
    assert_eq!(state.selected, state.filtered_rows.len().saturating_sub(2));
}

#[tokio::test]
async fn up_scrolls_only_after_crossing_top_edge() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let mut items = Vec::new();
    for idx in 0..10 {
        let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 10,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);
    state.selected = 8;
    state.scroll_top = 8;

    state
        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.selected, 7);
    assert_eq!(state.scroll_top, 7);
}

#[test]
fn list_reports_more_rows_above_and_below() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let mut items = Vec::new();
    for idx in 0..5 {
        let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        let path = format!("/tmp/item-{idx}.jsonl");
        items.push(make_row(&path, &ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(
        items, /*next_cursor*/ None, /*num_scanned_files*/ 5,
        /*reached_scan_cap*/ false,
    ));
    state.update_viewport(/*rows*/ 5, /*width*/ 80);

    assert!(!state.has_more_above());
    assert!(state.has_more_below(/*viewport_height*/ 5));

    state.scroll_top = 2;

    assert!(state.has_more_above());
    assert!(state.has_more_below(/*viewport_height*/ 5));
}

#[tokio::test]
async fn set_query_loads_until_match_and_respects_scan_cap() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.reset_pagination();
    state.ingest_page(page(
        vec![make_row(
            "/tmp/start.jsonl",
            "2025-01-01T00:00:00Z",
            "alpha",
        )],
        Some("2025-01-02T00:00:00Z"),
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    recorded_requests.lock().unwrap().clear();

    state.set_query("target".to_string());
    let first_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        guard[0].clone()
    };
    state.set_query("later".to_string());
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);

    state
        .handle_background_event(BackgroundEvent::Page {
            request_token: first_request.request_token,
            search_token: first_request.search_token,
            mode: first_request.mode,
            page: Ok(page(
                vec![make_row("/tmp/beta.jsonl", "2025-01-02T00:00:00Z", "beta")],
                Some("2025-01-03T00:00:00Z"),
                /*num_scanned_files*/ 5,
                /*reached_scan_cap*/ false,
            )),
        })
        .await
        .unwrap();

    let second_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        guard[1].clone()
    };
    assert!(state.search_state.is_active());
    assert!(state.filtered_rows.is_empty());

    state
        .handle_background_event(BackgroundEvent::Page {
            request_token: second_request.request_token,
            search_token: second_request.search_token,
            mode: second_request.mode,
            page: Ok(page(
                vec![make_row(
                    "/tmp/match.jsonl",
                    "2025-01-03T00:00:00Z",
                    "later log",
                )],
                Some("2025-01-04T00:00:00Z"),
                /*num_scanned_files*/ 7,
                /*reached_scan_cap*/ false,
            )),
        })
        .await
        .unwrap();

    assert!(!state.filtered_rows.is_empty());
    assert!(!state.search_state.is_active());

    recorded_requests.lock().unwrap().clear();
    state.set_query("missing".to_string());
    let active_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        guard[0].clone()
    };

    state
        .handle_background_event(BackgroundEvent::Page {
            request_token: second_request.request_token,
            search_token: second_request.search_token,
            mode: second_request.mode,
            page: Ok(page(
                Vec::new(),
                /*next_cursor*/ None,
                /*num_scanned_files*/ 0,
                /*reached_scan_cap*/ false,
            )),
        })
        .await
        .unwrap();
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);

    let mut pending_page = page(
        Vec::new(),
        /*next_cursor*/ None,
        /*num_scanned_files*/ 3,
        /*reached_scan_cap*/ false,
    );
    pending_page.next_cursor = Some(PageCursor::SourcesPending);
    state
        .handle_background_event(BackgroundEvent::Page {
            request_token: active_request.request_token,
            search_token: active_request.search_token,
            mode: active_request.mode,
            page: Ok(pending_page),
        })
        .await
        .unwrap();
    assert!(state.search_state.is_active());
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);
    state.set_query("updated".to_string());
    assert!(state.search_state.is_active());
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);

    state
        .handle_background_event(BackgroundEvent::PartialPage {
            listing_token: state.listing_token,
            page: page(
                vec![make_row("/s", "2025-01-04T00:00:00Z", "slow")],
                Some("2025-01-05T00:00:00Z"),
                /*num_scanned_files*/ 1,
                /*reached_scan_cap*/ false,
            ),
        })
        .await
        .unwrap();
    assert_eq!(recorded_requests.lock().unwrap().len(), 2);
    state.pagination.reached_scan_cap = true;
    state.continue_search_if_needed();

    assert!(state.filtered_rows.is_empty());
    assert!(!state.search_state.is_active());
    assert!(state.pagination.reached_scan_cap);
}

#[tokio::test]
async fn paste_appends_to_existing_query() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("resize");

    state.handle_paste(String::from("results"));

    assert_eq!(state.query, "resize results");
}

#[tokio::test]
async fn whitespace_only_paste_is_ignored() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.query = String::from("resize");

    state.handle_paste(String::from("  \n\t  "));

    assert_eq!(state.query, "resize");
}

#[tokio::test]
async fn paste_uses_existing_search_loading_path() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = page_only_loader(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.reset_pagination();
    state.ingest_page(page(
        vec![make_row(
            "/tmp/start.jsonl",
            "2025-01-01T00:00:00Z",
            "alpha",
        )],
        Some("2025-01-02T00:00:00Z"),
        /*num_scanned_files*/ 1,
        /*reached_scan_cap*/ false,
    ));
    recorded_requests.lock().unwrap().clear();

    state.handle_paste(String::from("target"));

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(state.query, "target");
    assert_eq!(guard.len(), 1);
    assert!(guard[0].search_token.is_some());
}

#[tokio::test]
async fn esc_with_empty_query_starts_fresh() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .expect("handle key");

    assert!(matches!(selection, Some(SessionSelection::StartFresh)));
}

#[tokio::test]
async fn esc_with_query_clears_search_and_preserves_selected_result() {
    let loader = page_only_loader(|_| {});
    let mut state = PickerState::new(
        crate::backend_id::BackendId::new(),
        FrameRequester::test_dummy(),
        loader,
        ProviderFilter::MatchDefault(String::from("openai")),
        /*show_all*/ true,
        /*filter_cwd*/ None,
        SessionPickerAction::Resume,
    );
    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_row("/tmp/alpha.jsonl", "2025-01-03T00:00:00Z", "alpha"),
            make_row("/tmp/beta.jsonl", "2025-01-02T00:00:00Z", "beta"),
        ],
        /*next_cursor*/ None,
        /*num_scanned_files*/ 2,
        /*reached_scan_cap*/ false,
    ));
    state.set_query(String::from("beta"));

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .expect("handle key");

    assert!(selection.is_none());
    assert!(state.query.is_empty());
    assert_eq!(state.filtered_rows.len(), 2);
    assert_eq!(
        state.filtered_rows[state.selected].path.as_deref(),
        Some(Path::new("/tmp/beta.jsonl"))
    );
}
