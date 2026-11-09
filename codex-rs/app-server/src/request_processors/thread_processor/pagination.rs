//! Page sizing, cursors, and page assembly for turns, threads, and background terminals.

use super::*;

pub(crate) fn thread_turns_page_size(limit: Option<u32>) -> usize {
    limit
        .map(|value| value as usize)
        .unwrap_or(THREAD_TURNS_DEFAULT_LIMIT)
        .clamp(1, THREAD_TURNS_MAX_LIMIT)
}

pub(super) fn thread_backwards_cursor_for_sort_key(
    thread: &StoredThread,
    sort_key: StoreThreadSortKey,
    sort_direction: SortDirection,
) -> Option<String> {
    if sort_key == StoreThreadSortKey::SectionPosition {
        let position = match sort_direction {
            SortDirection::Asc => thread.section_position?.checked_add(1)?,
            SortDirection::Desc => thread.section_position?.checked_sub(1)?,
        };
        return Some(format!("{position}|{}", thread.thread_id));
    }

    let timestamp = match sort_key {
        StoreThreadSortKey::CreatedAt => thread.created_at,
        StoreThreadSortKey::UpdatedAt => thread.updated_at,
        StoreThreadSortKey::RecencyAt => thread.recency_at,
        StoreThreadSortKey::SectionPosition => unreachable!("section positions use rank cursors"),
    };
    // The state DB stores unique millisecond timestamps. Offset the reverse cursor by one
    // millisecond so the opposite-direction query includes the page anchor.
    let timestamp = match sort_direction {
        SortDirection::Asc => timestamp.checked_add_signed(ChronoDuration::milliseconds(1))?,
        SortDirection::Desc => timestamp.checked_sub_signed(ChronoDuration::milliseconds(1))?,
    };
    Some(timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn paginate_thread_turns(
    turns: Vec<Turn>,
    cursor: Option<&str>,
    limit: Option<u32>,
    sort_direction: SortDirection,
) -> Result<ThreadTurnsPage, JSONRPCErrorError> {
    if turns.is_empty() {
        return Ok(ThreadTurnsPage {
            turns: Vec::new(),
            next_cursor: None,
            backwards_cursor: None,
        });
    }

    let anchor = cursor.map(parse_thread_turns_cursor).transpose()?;
    let page_size = limit
        .map(|value| value as usize)
        .unwrap_or(THREAD_TURNS_DEFAULT_LIMIT)
        .clamp(1, THREAD_TURNS_MAX_LIMIT);

    let anchor_index = anchor
        .as_ref()
        .and_then(|anchor| turns.iter().position(|turn| turn.id == anchor.turn_id));
    if anchor.is_some() && anchor_index.is_none() {
        return Err(invalid_request(
            "invalid cursor: anchor turn is no longer present",
        ));
    }

    let mut keyed_turns: Vec<_> = turns.into_iter().enumerate().collect();
    match sort_direction {
        SortDirection::Asc => {
            if let (Some(anchor), Some(anchor_index)) = (anchor.as_ref(), anchor_index) {
                keyed_turns.retain(|(index, _)| {
                    if anchor.include_anchor {
                        *index >= anchor_index
                    } else {
                        *index > anchor_index
                    }
                });
            }
        }
        SortDirection::Desc => {
            keyed_turns.reverse();
            if let (Some(anchor), Some(anchor_index)) = (anchor.as_ref(), anchor_index) {
                keyed_turns.retain(|(index, _)| {
                    if anchor.include_anchor {
                        *index <= anchor_index
                    } else {
                        *index < anchor_index
                    }
                });
            }
        }
    }

    let more_turns_available = keyed_turns.len() > page_size;
    keyed_turns.truncate(page_size);
    let backwards_cursor = keyed_turns
        .first()
        .map(|(_, turn)| serialize_thread_turns_cursor(&turn.id, /*include_anchor*/ true))
        .transpose()?;
    let next_cursor = if more_turns_available {
        keyed_turns
            .last()
            .map(|(_, turn)| serialize_thread_turns_cursor(&turn.id, /*include_anchor*/ false))
            .transpose()?
    } else {
        None
    };
    let turns = keyed_turns.into_iter().map(|(_, turn)| turn).collect();

    Ok(ThreadTurnsPage {
        turns,
        next_cursor,
        backwards_cursor,
    })
}

fn serialize_thread_turns_cursor(
    turn_id: &str,
    include_anchor: bool,
) -> Result<String, JSONRPCErrorError> {
    serde_json::to_string(&ThreadTurnsCursor {
        turn_id: turn_id.to_string(),
        include_anchor,
    })
    .map_err(|err| internal_error(format!("failed to serialize cursor: {err}")))
}

fn parse_thread_turns_cursor(cursor: &str) -> Result<ThreadTurnsCursor, JSONRPCErrorError> {
    serde_json::from_str(cursor).map_err(|_| invalid_request(format!("invalid cursor: {cursor}")))
}

pub(super) fn build_thread_turns_page_response(
    items: &[RolloutItem],
    loaded_status: ThreadStatus,
    has_live_running_thread: bool,
    active_turn: Option<Turn>,
    options: ThreadTurnsPageOptions<'_>,
) -> Result<ThreadTurnsListResponse, JSONRPCErrorError> {
    let mut turns = reconstruct_thread_turns_for_turns_list(
        items,
        loaded_status,
        has_live_running_thread,
        active_turn,
    );
    apply_thread_turns_items_view(&mut turns, options.items_view);
    let page = paginate_thread_turns(turns, options.cursor, options.limit, options.sort_direction)?;
    Ok(ThreadTurnsListResponse {
        data: page.turns,
        next_cursor: page.next_cursor,
        backwards_cursor: page.backwards_cursor,
    })
}

pub(crate) fn build_thread_resume_initial_turns_page(
    items: &[RolloutItem],
    loaded_status: ThreadStatus,
    has_live_running_thread: bool,
    active_turn: Option<Turn>,
    params: &ThreadResumeInitialTurnsPageParams,
) -> Result<codex_app_server_protocol::TurnsPage, JSONRPCErrorError> {
    build_thread_turns_page_response(
        items,
        loaded_status,
        has_live_running_thread,
        active_turn,
        ThreadTurnsPageOptions {
            cursor: None,
            limit: params.limit,
            sort_direction: params.sort_direction.unwrap_or(SortDirection::Desc),
            items_view: params.items_view.unwrap_or(TurnItemsView::Summary),
        },
    )
    .map(Into::into)
}

pub(crate) fn apply_thread_turns_items_view(turns: &mut [Turn], items_view: TurnItemsView) {
    for turn in turns {
        if !matches!(items_view, TurnItemsView::Full) && turn.items_view != items_view {
            turn.items = items_view.project_items(&turn.items);
        }
        turn.items_view = items_view;
    }
}

pub(super) fn reconstruct_thread_turns_for_turns_list(
    items: &[RolloutItem],
    loaded_status: ThreadStatus,
    has_live_running_thread: bool,
    active_turn: Option<Turn>,
) -> Vec<Turn> {
    let has_live_in_progress_turn = has_live_running_thread
        || active_turn
            .as_ref()
            .is_some_and(|turn| matches!(turn.status, TurnStatus::InProgress));
    let mut turns = build_legacy_api_turns_from_rollout_items(items);
    normalize_thread_turns_status(&mut turns, loaded_status, has_live_in_progress_turn);
    if let Some(active_turn) = active_turn {
        merge_turn_history_with_active_turn(&mut turns, active_turn);
    }
    turns
}

pub(crate) fn normalize_thread_turns_status(
    turns: &mut [Turn],
    loaded_status: ThreadStatus,
    has_live_in_progress_turn: bool,
) {
    let status = resolve_thread_status(loaded_status, has_live_in_progress_turn);
    if matches!(status, ThreadStatus::Active { .. }) {
        return;
    }
    for turn in turns {
        if matches!(turn.status, TurnStatus::InProgress) {
            turn.status = TurnStatus::Interrupted;
        }
    }
}

pub(super) fn paginate_background_terminals(
    terminals: &[ThreadBackgroundTerminal],
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<(Vec<ThreadBackgroundTerminal>, Option<String>), JSONRPCErrorError> {
    let start = match cursor {
        Some(cursor) => {
            let cursor = cursor
                .parse::<i32>()
                .map_err(|err| invalid_request(format!("invalid cursor: {err}")))?;
            terminals
                .iter()
                .position(|terminal| {
                    terminal
                        .process_id
                        .parse::<i32>()
                        .is_ok_and(|process_id| process_id > cursor)
                })
                .unwrap_or(terminals.len())
        }
        None => 0,
    };
    let effective_limit = limit.unwrap_or(terminals.len() as u32).max(1) as usize;
    let end = start.saturating_add(effective_limit).min(terminals.len());
    let next_cursor = (end < terminals.len()).then(|| terminals[end - 1].process_id.clone());
    Ok((terminals[start..end].to_vec(), next_cursor))
}
