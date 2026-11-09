//! Entry points that configure the session picker and run its terminal event loop.

use super::*;

fn resume_picker_backends(
    uses_remote_filesystem: bool,
    config: &Config,
    include_non_interactive: bool,
    app_servers: ResumePickerAppServers,
) -> (Vec<AppServerPickerBackend>, crate::backend_id::BackendId) {
    let displayed_owner = app_servers.displayed_owner;
    let backends = app_servers
        .ordered
        .into_iter()
        .map(|session| {
            let is_displayed_owner = session.backend_id() == displayed_owner;
            AppServerPickerBackend {
                uses_remote_filesystem: if is_displayed_owner {
                    uses_remote_filesystem
                } else {
                    session.uses_remote_workspace()
                },
                archive_request_handle: session.request_handle(),
                config: (!session.uses_remote_workspace()).then(|| config.clone()),
                session,
                include_non_interactive,
                raw_reasoning_visibility: raw_reasoning_visibility(config),
            }
        })
        .collect();
    (backends, displayed_owner)
}

/// Interactive session picker that lists app-server threads with simple search,
/// lazy transcript previews, and pagination.
///
/// Sessions render as compact multi-line records with stable metadata first and
/// the conversation preview last. Users can focus the toolbar controls with
/// Tab, change the focused control with the arrow keys, and expand the
/// selected session with Ctrl+E to load recent transcript context on demand.
///
/// Sessions are loaded on-demand via cursor-based pagination. The backend
/// `thread/list` API returns pages ordered by the selected sort key, and the
/// picker deduplicates across pages to handle overlapping windows when new
/// sessions appear during pagination.
///
/// Filtering happens in two layers:
/// 1. Provider, source, and eligible working-directory filtering at the backend.
/// 2. Typed search filtering over loaded rows in the picker.
pub async fn run_resume_picker_with_app_server(
    uses_remote_filesystem: bool,
    tui: &mut Tui,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    run_resume_picker_with_app_servers(
        uses_remote_filesystem,
        tui,
        config,
        local_settings,
        show_all,
        include_non_interactive,
        ResumePickerAppServers::singleton(app_server),
    )
    .await
}

pub(crate) async fn run_resume_picker_with_app_servers(
    uses_remote_filesystem: bool,
    tui: &mut Tui,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    show_all: bool,
    include_non_interactive: bool,
    app_servers: ResumePickerAppServers,
) -> Result<SessionSelection> {
    let (backends, displayed_owner) = resume_picker_backends(
        uses_remote_filesystem,
        config,
        include_non_interactive,
        app_servers,
    );
    run_resume_picker_with_launch_context(
        uses_remote_filesystem,
        tui,
        config,
        local_settings,
        show_all,
        backends,
        displayed_owner,
        SessionPickerLaunchContext::Startup,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "picker configuration follows the existing session entry point"
)]
pub(crate) async fn run_resume_picker_from_existing_session_with_app_servers(
    uses_remote_filesystem: bool,
    tui: &mut Tui,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    show_all: bool,
    include_non_interactive: bool,
    app_servers: ResumePickerAppServers,
    current_thread_id: Option<ThreadId>,
) -> Result<SessionSelection> {
    let (backends, displayed_owner) = resume_picker_backends(
        uses_remote_filesystem,
        config,
        include_non_interactive,
        app_servers,
    );
    run_resume_picker_with_launch_context(
        uses_remote_filesystem,
        tui,
        config,
        local_settings,
        show_all,
        backends,
        displayed_owner,
        SessionPickerLaunchContext::ExistingSession { current_thread_id },
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "keep local preferences separate while the legacy Config parameter is still required"
)]
async fn run_resume_picker_with_launch_context(
    uses_remote_filesystem: bool,
    tui: &mut Tui,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    show_all: bool,
    app_servers: Vec<AppServerPickerBackend>,
    displayed_owner: crate::backend_id::BackendId,
    launch_context: SessionPickerLaunchContext,
) -> Result<SessionSelection> {
    let has_multiple_backends = app_servers.len() > 1;
    let Some(app_server) = app_servers
        .iter()
        .find(|backend| backend.session.backend_id() == displayed_owner)
    else {
        unreachable!("displayed owner must belong to the resume picker's nonempty backend set");
    };
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.session.uses_remote_workspace();
    // A single path cannot describe both local and remote workspaces. Until the
    // picker owns per-backend filters, a mixed listing starts unfiltered.
    let cwd_filter = (!has_multiple_backends)
        .then(|| {
            picker_cwd_filter(
                config.cwd.as_path(),
                /*show_all*/ false,
                uses_remote_workspace,
                app_server.session.remote_cwd_override(),
            )
        })
        .flatten();
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_filesystem);
    let provider_filter = ProviderFilter::All;
    let runtime_keymap = picker_runtime_keymap(local_settings)?;
    let options = SessionPickerRunOptions {
        backend_id: app_server.session.backend_id(),
        use_theme_colors: local_settings.tui.status_line_use_colors,
        copy_on_select: local_settings.copy_on_select(&codex_terminal_detection::terminal_info()),
        copy_on_select_clear_selection: local_settings.tui.copy_on_select_clear_selection,
        mouse_scroll_speed: local_settings.tui.mouse_scroll_speed.unwrap_or(1.0),
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Resume,
        launch_context,
        provider_filter,
        initial_density: SessionListDensity::from(
            local_settings.tui.session_picker_view.unwrap_or_default(),
        ),
        view_persistence: Some(SessionPickerViewPersistence {
            codex_home: local_settings.codex_home.to_path_buf(),
        }),
        keymap: runtime_keymap,
        initial_page_mode: if uses_remote_workspace || has_multiple_backends {
            PageLoadMode::StoreDefault
        } else {
            PageLoadMode::StateDbOnly
        },
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loaders(app_servers, bg_tx),
        bg_rx,
    )
    .await
}

pub async fn run_fork_picker_with_app_server(
    uses_remote_filesystem: bool,
    tui: &mut Tui,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    show_all: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    let archive_request_handle = app_server.request_handle();
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.uses_remote_workspace();
    let cwd_filter = picker_cwd_filter(
        config.cwd.as_path(),
        /*show_all*/ false,
        uses_remote_workspace,
        app_server.remote_cwd_override(),
    );
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_filesystem);
    let provider_filter = picker_provider_filter(config, &app_server).await?;
    let runtime_keymap = picker_runtime_keymap(local_settings)?;
    let options = SessionPickerRunOptions {
        backend_id: app_server.backend_id(),
        use_theme_colors: local_settings.tui.status_line_use_colors,
        copy_on_select: local_settings.copy_on_select(&codex_terminal_detection::terminal_info()),
        copy_on_select_clear_selection: local_settings.tui.copy_on_select_clear_selection,
        mouse_scroll_speed: local_settings.tui.mouse_scroll_speed.unwrap_or(1.0),
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Fork,
        launch_context: SessionPickerLaunchContext::Startup,
        provider_filter,
        initial_density: SessionListDensity::from(
            local_settings.tui.session_picker_view.unwrap_or_default(),
        ),
        view_persistence: Some(SessionPickerViewPersistence {
            codex_home: local_settings.codex_home.to_path_buf(),
        }),
        keymap: runtime_keymap,
        initial_page_mode: if uses_remote_workspace {
            PageLoadMode::StoreDefault
        } else {
            PageLoadMode::StateDbOnly
        },
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loader(
            uses_remote_filesystem,
            app_server,
            archive_request_handle,
            /*include_non_interactive*/ false,
            raw_reasoning_visibility(config),
            (!uses_remote_workspace).then(|| config.clone()),
            bg_tx,
        ),
        bg_rx,
    )
    .await
}

async fn run_session_picker_with_loader(
    tui: &mut Tui,
    options: SessionPickerRunOptions,
    background: PickerBackground,
    bg_rx: mpsc::UnboundedReceiver<BackgroundEvent>,
) -> Result<SessionSelection> {
    let PickerBackground { loader, task } = background;
    let result = run_session_picker_event_loop(tui, options, loader, bg_rx).await;

    drop(task);
    result
}

async fn run_session_picker_event_loop(
    tui: &mut Tui,
    options: SessionPickerRunOptions,
    picker_loader: PickerLoader,
    bg_rx: mpsc::UnboundedReceiver<BackgroundEvent>,
) -> Result<SessionSelection> {
    let (repo_filter_tx, repo_filter_rx) = mpsc::unbounded_channel();
    if let Some(cwd) = options.local_filter_cwd.clone() {
        tokio::task::spawn_blocking(move || {
            let _ = repo_filter_tx.send(local_repo_filter_available(&cwd));
        });
    }
    let alt = AltScreenGuard::enter(tui);
    let mut state = PickerState::new(
        options.backend_id,
        alt.tui.frame_requester(),
        picker_loader,
        options.provider_filter,
        options.show_all,
        options.local_filter_cwd.clone(),
        options.action,
    );
    state.filter_mode =
        SessionFilterMode::from_show_all(options.show_all, options.filter_cwd.as_deref());
    state.filter_cwd = options.filter_cwd;
    state.local_filter_cwd = options.local_filter_cwd;
    state.use_theme_colors = options.use_theme_colors;
    state.copy_on_select = options.copy_on_select;
    state.copy_on_select_clear_selection = options.copy_on_select_clear_selection;
    state.mouse_scroll_speed = options.mouse_scroll_speed;
    state.density = options.initial_density;
    state.view_persistence = options.view_persistence;
    state.keymap = options.keymap;
    state.launch_context = options.launch_context;
    state.initial_page_mode = options.initial_page_mode;
    state.start_initial_load();
    state.request_frame();

    if let Ok(size) = alt.tui.terminal.size() {
        let list_height = usize::from(layout::areas(Rect::from(size)).list.height);
        state.update_viewport(list_height, list_viewport_width(size.width));
        state.ensure_minimum_rows_for_view(list_height);
    }
    draw_picker(alt.tui, &state, alt.tui.terminal.last_known_screen_size)?;
    if state.launch_context == SessionPickerLaunchContext::Startup {
        alt.tui.discard_pending_input_before_interactive_screen()?;
    }
    let mut tui_events = alt.tui.event_stream().fuse();
    let mut background_events = UnboundedReceiverStream::new(bg_rx).fuse();
    let mut repo_filter_events = UnboundedReceiverStream::new(repo_filter_rx).fuse();

    loop {
        tokio::select! {
            Some(ev) = tui_events.next() => {
                alt.tui.clipboard.poll();
                let screen_size = alt.tui.screen_size_for_event(&ev)?;
                if matches!(&ev, TuiEvent::Paste(_) | TuiEvent::FocusLost)
                    || matches!(&ev, TuiEvent::Mouse(mouse) if mouse.kind != crossterm::event::MouseEventKind::Moved)
                {
                    state.chord_matcher.cancel();
                }
                let ev = if let TuiEvent::Key(key) = ev {
                    let Some(key) = state.route_key_chord(key) else {
                        continue;
                    };
                    TuiEvent::Key(key)
                } else {
                    ev
                };
                if state.overlay.is_some() {
                    state.handle_overlay_event(alt.tui, ev)?;
                    continue;
                }
                match ev {
                    TuiEvent::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Release) {
                            continue;
                        }
                        if let Some(sel) = state.handle_key(key).await? {
                            return Ok(sel);
                        }
                    }
                    TuiEvent::Paste(pasted) => {
                        state.handle_paste(pasted);
                    }
                    TuiEvent::Draw | TuiEvent::Resume | TuiEvent::Resize(_) | TuiEvent::FocusGained => {
                        let list_width = list_viewport_width(screen_size.width);
                        let list_height =
                            usize::from(layout::areas(Rect::from(screen_size)).list.height);
                        state.update_viewport(list_height, list_width);
                        state.ensure_minimum_rows_for_view(list_height);
                        draw_picker(alt.tui, &state, screen_size)?;
                        if state.note_transcript_loading_frame_drawn() {
                            state.open_pending_transcript_if_ready();
                        }
                    }
                    TuiEvent::FocusLost | TuiEvent::Mouse(_) => {}
                }
            }
            Some(event) = background_events.next() => {
                if let Some(selection) = state.handle_background_event(event).await? {
                    return Ok(selection);
                }
            }
            Some(available) = repo_filter_events.next() => {
                state.set_repo_filter_available(available);
            }
            else => break,
        }
    }

    // Fallback – treat as cancel/new
    Ok(SessionSelection::StartFresh)
}

fn raw_reasoning_visibility(config: &Config) -> RawReasoningVisibility {
    if config.show_raw_agent_reasoning {
        RawReasoningVisibility::Visible
    } else {
        RawReasoningVisibility::Hidden
    }
}

pub(super) fn local_picker_cwd_filter(
    cwd_filter: &Option<PathBuf>,
    uses_remote_filesystem: bool,
) -> Option<PathBuf> {
    if uses_remote_filesystem {
        None
    } else {
        cwd_filter.clone()
    }
}

async fn picker_provider_filter(
    config: &Config,
    app_server: &AppServerSession,
) -> Result<ProviderFilter> {
    Ok(app_server
        .history_model_provider(config)
        .await?
        .map_or(ProviderFilter::Any, ProviderFilter::MatchDefault))
}

fn picker_runtime_keymap(config: &crate::local_settings::LocalSettings) -> Result<RuntimeKeymap> {
    RuntimeKeymap::from_config(&config.tui.keymap)
        .map_err(|err| color_eyre::eyre::eyre!("invalid keymap configuration: {err}"))
}

pub(super) fn picker_cwd_filter(
    config_cwd: &Path,
    show_all: bool,
    uses_remote_workspace: bool,
    remote_cwd_override: Option<&Path>,
) -> Option<PathBuf> {
    if show_all {
        None
    } else if uses_remote_workspace {
        remote_cwd_override.map(Path::to_path_buf)
    } else {
        Some(config_cwd.to_path_buf())
    }
}

fn draw_picker(tui: &mut Tui, state: &PickerState, screen_size: Size) -> std::io::Result<()> {
    tui.draw(screen_size.height, |frame| layout::render(frame, state))
}
