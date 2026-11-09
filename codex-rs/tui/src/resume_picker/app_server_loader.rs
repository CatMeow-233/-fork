//! App-server workers that serve picker page, preview, transcript, and archive requests.

use super::*;

pub(super) fn spawn_app_server_page_loader(
    uses_remote_filesystem: bool,
    app_server: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<Config>,
    output: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerBackground {
    spawn_app_server_page_loaders(
        vec![AppServerPickerBackend {
            uses_remote_filesystem,
            session: app_server,
            archive_request_handle,
            include_non_interactive,
            raw_reasoning_visibility,
            config,
        }],
        output,
    )
}

pub(super) fn spawn_app_server_page_loaders(
    backends: Vec<AppServerPickerBackend>,
    output: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerBackground {
    let mut sources = Vec::with_capacity(backends.len());
    for backend in backends {
        sources.push(spawn_app_server_source(backend));
    }
    sources::spawn(sources, output)
}

fn spawn_app_server_source(backend: AppServerPickerBackend) -> sources::Source {
    let AppServerPickerBackend {
        uses_remote_filesystem,
        session: app_server,
        archive_request_handle,
        include_non_interactive,
        raw_reasoning_visibility,
        config,
    } = backend;
    let backend_id = app_server.backend_id();
    let (bg_tx, events) = mpsc::unbounded_channel();
    let (request_tx, mut request_rx) = mpsc::unbounded_channel::<PickerLoadRequest>();

    let task = tokio::spawn(async move {
        let mut app_server = app_server;
        let mut page_cwd_filter = PageCwdFilter::default();
        while let Some(request) = request_rx.recv().await {
            match request {
                PickerLoadRequest::Page(request) => {
                    let mode = request.mode;
                    let provider_filter = request.provider_filter;
                    let cwd_filter = page_cwd_filter.for_request(
                        request.cursor.as_ref(),
                        request.cwd_filter.as_deref(),
                        request.filter_mode,
                        uses_remote_filesystem,
                    );
                    let cursor = request.cursor.map(|cursor| match cursor {
                        PageCursor::AppServer(cursor) => cursor,
                        PageCursor::Catalog | PageCursor::SourcesPending => {
                            unreachable!("catalog cursors never reach a source")
                        }
                    });
                    let params = thread_list_params(
                        cursor,
                        cwd_filter,
                        request.status,
                        provider_filter,
                        request.sort_key,
                        include_non_interactive,
                        matches!(request.mode, PageLoadMode::StateDbOnly),
                    );
                    let page = tokio::select! {
                        page = load_app_server_page(&mut app_server, params) => page,
                        _ = bg_tx.closed() => continue,
                    };
                    let _ = bg_tx.send(BackgroundEvent::Page {
                        request_token: request.request_token,
                        search_token: request.search_token,
                        mode,
                        page,
                    });
                }
                PickerLoadRequest::Acknowledge {
                    thread_id,
                    revision,
                    ..
                } => match app_server.acknowledge_read(thread_id, revision).await {
                    Ok(state) => {
                        let _ = bg_tx.send(BackgroundEvent::Acknowledged {
                            backend_id,
                            thread_id,
                            revision: unread_revision(Some(state)),
                        });
                    }
                    Err(error) => {
                        warn!(%thread_id, %error, "Could not acknowledge viewed transcript")
                    }
                },
                PickerLoadRequest::Preview { thread_id, .. } => {
                    let preview = tokio::select! {
                        preview = load_transcript_preview(&mut app_server, thread_id, config.as_ref()) => preview,
                        _ = bg_tx.closed() => continue,
                    };
                    let _ = bg_tx.send(BackgroundEvent::Preview { thread_id, preview });
                }
                PickerLoadRequest::Transcript {
                    thread_id,
                    cancellation,
                    ..
                } => {
                    tokio::select! {
                        transcript = load_session_transcript(
                            &mut app_server,
                            thread_id,
                            raw_reasoning_visibility,
                            config.as_ref(),
                        ) => {
                            let _ = bg_tx.send(BackgroundEvent::Transcript {
                                backend_id, thread_id,
                                transcript: transcript.map(|cells| LoadedTranscript {
                                    cells, unread_revision: app_server.unread_revisions.remove(&thread_id),
                                }),
                            });
                        }
                        _ = cancellation => {}
                        _ = bg_tx.closed() => {}
                    }
                }
                PickerLoadRequest::Archive { thread_id, .. } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadArchiveResponse>(ClientRequest::ThreadArchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-archive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadArchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|_| ())
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Archive { thread_id, result });
                }
                PickerLoadRequest::Unarchive { thread_id, .. } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadUnarchiveResponse>(ClientRequest::ThreadUnarchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-unarchive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadUnarchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|response| SessionTarget {
                            backend_id: app_server.backend_id(),
                            path: response.thread.path,
                            thread_id,
                            cwd: Some(response.thread.cwd.to_path_buf()),
                            history_mode: Some(response.thread.history_mode),
                        })
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Unarchive { thread_id, result });
                }
            }
        }
        if let Err(err) = app_server.shutdown().await {
            warn!(%err, "Failed to shut down app-server picker session");
        }
    });

    sources::Source {
        backend_id,
        loader: Arc::new(move |request: PickerLoadRequest| {
            let _ = request_tx.send(request);
        }),
        events,
        task,
    }
}

async fn load_app_server_page(
    app_server: &mut AppServerSession,
    params: ThreadListParams,
) -> std::io::Result<PickerPage> {
    let response = app_server
        .thread_list(params)
        .await
        .map_err(std::io::Error::other)?;
    let num_scanned_files = response.data.len();
    let (rows, history_modes): (Vec<_>, HashMap<_, _>) = response
        .data
        .into_iter()
        .filter_map(|thread| {
            let history_mode = thread.history_mode;
            let row = row_from_app_server_thread(thread)?;
            let thread_id = row.thread_id?;
            Some((row, (thread_id, history_mode)))
        })
        .unzip();

    Ok(PickerPage {
        rows,
        read_states: response.read_states.unwrap_or_default(),
        history_modes,
        next_cursor: response.next_cursor.map(PageCursor::AppServer),
        num_scanned_files,
        reached_scan_cap: false,
    })
}

pub(super) fn row_from_app_server_thread(thread: Thread) -> Option<Row> {
    let thread_id = match ThreadId::from_string(&thread.id) {
        Ok(thread_id) => thread_id,
        Err(err) => {
            warn!(thread_id = thread.id, %err, "Skipping app-server picker row with invalid id");
            return None;
        }
    };
    let preview = thread.preview.trim();
    Some(Row {
        backend_id: None,
        history_mode: None,
        path: thread.path,
        preview: if preview.is_empty() {
            String::from("(no message yet)")
        } else {
            preview.to_string()
        },
        thread_id: Some(thread_id),
        thread_name: thread.name,
        created_at: chrono::DateTime::from_timestamp(thread.created_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        updated_at: chrono::DateTime::from_timestamp(thread.updated_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        cwd: Some(thread.cwd.to_path_buf()),
        git_branch: thread.git_info.and_then(|git_info| git_info.branch),
    })
}

pub(super) fn thread_list_params(
    cursor: Option<String>,
    cwd_filter: Option<ThreadListCwdFilter>,
    status: SessionStatus,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
    use_state_db_only: bool,
) -> ThreadListParams {
    ThreadListParams {
        current_account_only: true,
        excluded_thread_ids: None,
        attached_to_thread_id: None,
        originators: None,
        cursor,
        limit: Some(PAGE_SIZE as u32),
        sort_key: Some(sort_key),
        sort_direction: None,
        model_providers: match provider_filter {
            ProviderFilter::Any => None,
            ProviderFilter::All => Some(Vec::new()),
            ProviderFilter::MatchDefault(default_provider) => Some(vec![default_provider]),
        },
        source_kinds: Some(crate::resume_source_kinds(include_non_interactive)),
        archived: Some(status == SessionStatus::Archived),
        section_id: None,
        project_id: None,
        parent_thread_id: None,
        ancestor_thread_id: None,
        cwd: cwd_filter,
        use_state_db_only,
        search_term: None,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn parse_timestamp_str(ts: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}
