//! Maps provider response streams into Codex `ResponseStream`s (item ids, telemetry, traces).

use super::*;

pub(super) fn map_response_stream(
    mut api_stream: codex_api::ResponseStream,
    session_telemetry: SessionTelemetry,
    inference_trace_attempt: InferenceTraceAttempt,
    client: &ModelClientState,
    interceptors: Vec<Box<dyn codex_extension_api::ModelResponseInterceptor>>,
    last_response_tx: Option<oneshot::Sender<LastResponse>>,
) -> ResponseStream {
    let routing_affinity = api_stream.routing_affinity.take();
    let upstream_request_id = api_stream.upstream_request_id.take();
    let interrupt = api_stream.interrupt.take();
    let mut stream = map_response_events(
        upstream_request_id,
        crate::model_request::intercept_stream(Box::pin(api_stream), interceptors),
        session_telemetry,
        inference_trace_attempt,
        client,
        last_response_tx,
    );
    stream.interrupt = interrupt;
    stream.routing_affinity = routing_affinity;
    stream
}

pub(super) fn map_response_events<S>(
    upstream_request_id: Option<String>,
    api_stream: S,
    session_telemetry: SessionTelemetry,
    inference_trace_attempt: InferenceTraceAttempt,
    client: &ModelClientState,
    mut last_response_tx: Option<oneshot::Sender<LastResponse>>,
) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, ApiError>>
        + Unpin
        + Send
        + 'static,
{
    let (tx_event, rx_event) =
        mpsc::channel::<Result<ResponseEvent>>(RESPONSE_STREAM_CHANNEL_CAPACITY);
    let consumer_dropped = CancellationToken::new();
    let consumer_dropped_for_stream = consumer_dropped.clone();
    let provider = Arc::clone(&client.provider);
    let item_id_generator = Arc::clone(&client.item_id_generator);

    tokio::spawn(async move {
        let mut logged_error = false;
        let collect_output_items =
            last_response_tx.is_some() || inference_trace_attempt.needs_output_items();
        let mut items_added: Vec<ResponseItem> = Vec::new();
        let mut active_item_id: Option<ResponseItemId> = None;
        let (request_start, mut ttft_ms) = (Instant::now(), None);
        let mut api_stream = api_stream;
        let upstream_request_id = upstream_request_id.as_deref();
        if let Some(upstream_request_id) = upstream_request_id {
            feedback_tags!(last_model_request_id = upstream_request_id);
        }
        loop {
            let event = tokio::select! {
                _ = consumer_dropped.cancelled() => {
                    inference_trace_attempt.record_cancelled(
                        STREAM_DROPPED_REASON,
                        upstream_request_id,
                        &items_added,
                    );
                    return;
                }
                event = api_stream.next() => event,
            };
            let Some(event) = event else {
                break;
            };
            match event {
                Ok(ResponseEvent::OutputItemDone(mut item)) => {
                    // Assign IDs after interceptors, which may replace provider items.
                    // Reuse the added event's ID so tracing and the harness agree.
                    if item.id().is_none_or(|id| id.is_empty()) {
                        item.set_id(
                            active_item_id
                                .take()
                                .or_else(|| item.id_prefix().map(item_id_generator.as_ref())),
                        );
                    }
                    active_item_id = None;
                    if collect_output_items {
                        items_added.push(item.clone());
                    }
                    if tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                        .await
                        .is_err()
                    {
                        inference_trace_attempt.record_cancelled(
                            STREAM_DROPPED_REASON,
                            upstream_request_id,
                            &items_added,
                        );
                        return;
                    }
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                    usage_metadata,
                    end_turn,
                }) => {
                    feedback_tags!(last_model_response_id = &response_id);
                    if let Some(usage) = &token_usage {
                        session_telemetry.sse_event_completed(usage, ttft_ms);
                    }
                    inference_trace_attempt.record_completed(
                        &response_id,
                        upstream_request_id,
                        &token_usage,
                        &items_added,
                    );
                    if let Some(sender) = last_response_tx.take() {
                        let _ = sender.send(LastResponse {
                            response_id: response_id.clone(),
                            items_added,
                        });
                    }
                    items_added = Vec::new();
                    if tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage,
                            usage_metadata,
                            end_turn,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(mut event) => {
                    if let ResponseEvent::OutputItemAdded(item) = &mut event {
                        if item.id().is_none_or(|id| id.is_empty()) {
                            item.set_id(item.id_prefix().map(item_id_generator.as_ref()));
                        }
                        active_item_id = item.id().cloned();
                    }
                    if matches!(&event, ResponseEvent::OutputItemAdded(_)) && ttft_ms.is_none() {
                        ttft_ms = Some(
                            i64::try_from(request_start.elapsed().as_millis()).unwrap_or(i64::MAX),
                        );
                    }
                    if tx_event.send(Ok(event)).await.is_err() {
                        inference_trace_attempt.record_cancelled(
                            STREAM_DROPPED_REASON,
                            upstream_request_id,
                            &items_added,
                        );
                        return;
                    }
                }
                Err(err) => {
                    let response_debug_context =
                        extract_response_debug_context_from_api_error(&err);
                    let upstream_request_id =
                        upstream_request_id.or(response_debug_context.request_id.as_deref());
                    if let Some(upstream_request_id) = upstream_request_id {
                        feedback_tags!(last_model_request_id = upstream_request_id);
                    }
                    let mapped = provider.map_api_error(err);
                    // Terminal response output is trace evidence. Only streamed done items
                    // belong to the continuation cache or reach tool dispatch.
                    let completed_item_count = items_added.len();
                    if inference_trace_attempt.needs_output_items()
                        && let CodexErrorDetails::ResponseIncomplete(incomplete) = mapped.details()
                    {
                        for raw in &incomplete.partial_output {
                            // Other discards unknown payloads and cannot be replayed as a trace item.
                            if let Ok(item) = ResponseItem::deserialize(raw)
                                && !matches!(item, ResponseItem::Other)
                                && !item.id().is_some_and(|id| {
                                    items_added.iter().any(|seen| seen.id() == Some(id))
                                })
                            {
                                items_added.push(item);
                            }
                        }
                    }
                    inference_trace_attempt.record_failed(
                        &mapped,
                        upstream_request_id,
                        &items_added,
                    );
                    items_added.truncate(completed_item_count);
                    if !logged_error {
                        session_telemetry.see_event_completed_failed(&mapped);
                        logged_error = true;
                    }
                    if tx_event.send(Err(mapped)).await.is_err() {
                        return;
                    }
                }
            }
        }
        inference_trace_attempt.record_failed(
            "stream closed before response.completed",
            upstream_request_id,
            &items_added,
        );
    });

    ResponseStream {
        routing_affinity: None,
        rx_event,
        interrupt: None,
        consumer_dropped: consumer_dropped_for_stream,
    }
}
