//! Input hooks and recording of accepted turn input, minus audio the model cannot read.

use super::*;

#[instrument(level = "trace", skip_all)]
pub(crate) async fn run_hooks_and_record_inputs(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_info: &ModelInfo,
    input: &[TurnInput],
    persist_context: PersistContext,
) -> CodexResult<bool> {
    let mut blocked_input = false;
    let mut accepted_user_input = false;
    for input_item in recordable_inputs(sess, model_info, input) {
        let input_item = input_item?;
        let hook_outcome = inspect_pending_input(sess, turn_context, &input_item).await;
        if hook_outcome.should_stop {
            blocked_input = true;
            record_additional_contexts(sess, turn_context, hook_outcome.additional_contexts).await;
        } else {
            if matches!(input_item.as_ref(), TurnInput::UserInput { content, .. } if !content.is_empty())
            {
                accepted_user_input = true;
            }
            // Tool outputs retain their durability barrier, including in mixed input batches.
            let input_persist_context = if persist_context == PersistContext::SteeredUserInput
                && matches!(input_item.as_ref(), TurnInput::FunctionCallOutput(_))
            {
                PersistContext::Standard
            } else {
                persist_context
            };
            record_pending_input(
                sess,
                turn_context,
                model_info,
                input_item.into_owned(),
                hook_outcome.additional_contexts,
                input_persist_context,
            )
            .await;
        }
    }
    // The batch has left the input queue. Persist every accepted item before the
    // optional lookup can suspend this task long enough for an interrupt to abort it.
    if !input.is_empty() {
        let ids = std::mem::take(&mut sess.state.lock().await.pending_audio_duration_ids);
        super::file_audio::resolve(sess, ids).await;
    }
    Ok(blocked_input && !accepted_user_input)
}

/// Yields usable input in order, then reports unsupported audio after valid siblings can be recorded.
pub(crate) fn recordable_inputs<'a>(
    sess: &Session,
    model_info: &ModelInfo,
    input: &'a [TurnInput],
) -> impl Iterator<Item = CodexResult<Cow<'a, TurnInput>>> {
    let rejected_audio = !model_info.input_modalities.contains(&InputModality::Audio)
        && input.iter().any(turn_input_has_uploaded_audio_file);
    // Only finalized evidence may enter reusable Guardian history.
    let items: &[TurnInput] = if sess
        .services
        .thread_extension_data
        .get::<crate::guardian::PendingReviewContext>()
        .is_some()
    {
        &[]
    } else {
        input
    };
    items
        .iter()
        .filter_map(move |item| {
            let mut item = Cow::Borrowed(item);
            if rejected_audio
                && turn_input_has_uploaded_audio_file(&item)
                && let TurnInput::UserInput { content, .. } = item.to_mut()
            {
                content.retain(|part| !is_uploaded_audio_file(part));
                if content.is_empty() {
                    return None;
                }
            }
            Some(Ok(item))
        })
        .chain(rejected_audio.then(|| Err(unsupported_uploaded_audio_error())))
}

pub(super) fn turn_input_has_uploaded_audio_file(input: &TurnInput) -> bool {
    matches!(input, TurnInput::UserInput { content, .. } if content.iter().any(is_uploaded_audio_file))
}

pub(super) fn unsupported_uploaded_audio_error() -> CodexErr {
    CodexErr::UnsupportedOperation(
        "The selected model does not support uploaded audio. Choose an audio-capable model or remove the attachment.".to_string(),
    )
}

fn is_uploaded_audio_file(input: &UserInput) -> bool {
    matches!(
        input,
        UserInput::Audio {
            audio: AudioReference::File { .. }
        }
    )
}

pub(crate) fn turn_user_input(input: &[TurnInput]) -> Vec<UserInput> {
    input
        .iter()
        .filter_map(|item| match item {
            TurnInput::UserInput { content, .. } => Some(content.as_slice()),
            TurnInput::ResponseItem(_)
            | TurnInput::FunctionCallOutput(_)
            | TurnInput::InterAgentCommunication(_) => None,
        })
        .flatten()
        .cloned()
        .collect()
}
