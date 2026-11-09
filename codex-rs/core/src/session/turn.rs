//! Turn execution: runs one user turn by sampling the model until it needs no follow-up.
//!
//! This module holds the state types shared while a turn streams; the work lives in:
//! - `run_loop`: the turn loop that drives sampling requests and follow-ups.
//! - `input_recording`: input hooks and recording of accepted turn input.
//! - `context_injection`: skill, plugin, app, and extension context for the turn.
//! - `auto_compaction`: pre-sampling and mid-turn auto-compaction.
//! - `sampling_request`: prompt and tool router construction, plus request retries.
//! - `response_stream`: consuming a single model response stream.
//! - `assistant_text`: streamed assistant text deltas and final message extraction.
//! - `plan_mode`: proposed plan items streamed in plan mode.
//! - `realtime_text`: event text forwarded to the realtime conversation.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::compact::AutoCompactionDisabled;
use crate::compact::run_inline_auto_compact_task;
use crate::compact_remote_v2::BackgroundCompactionCandidate;
use crate::compact_remote_v2::run_inline_remote_auto_compact_task as run_inline_remote_auto_compact_task_v2;
use crate::connectors;
use crate::context::ContextualUserFragment;
use crate::context::UserVerificationNotice;
use crate::context_manager::rejected_audio;
use crate::cyber_access_program;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::feedback_tags;
use crate::hook_runtime::drain_async_hook_results;
use crate::hook_runtime::hooks_for_model_request;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::legacy_notify_input_messages;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use crate::hook_runtime::run_legacy_after_agent_hook;
use crate::hook_runtime::run_model_request_hooks;
use crate::hook_runtime::run_pending_session_start_hooks;
use crate::hook_runtime::run_turn_stop_hooks;
use crate::mcp_skill_dependencies::maybe_prompt_and_install_mcp_dependencies;
use crate::mentions::build_connector_slug_counts;
use crate::mentions::collect_explicit_app_ids;
use crate::mentions::collect_explicit_plugin_mentions;
use crate::mentions::collect_tool_mentions_from_messages;
use crate::plugins::build_plugin_injections;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_retry::ResponsesStreamRequest;
use crate::responses_retry::ResponsesStreamRetryState;
use crate::responses_retry::handle_response_stream_error;
use crate::session::PreviousTurnSettings;
use crate::session::TurnInput;
use crate::session::daemon_recovery::RecordedTurnInput;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::skills::emit_explicit_skill_invocations;
use crate::stream_events_utils::HandleOutputCtx;
use crate::stream_events_utils::InFlightFuture;
use crate::stream_events_utils::TurnItemContributorPolicy;
use crate::stream_events_utils::finalize_non_tool_response_item;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::handle_output_item_done;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::stream_events_utils::mark_thread_memory_mode_polluted_if_external_context;
use crate::stream_events_utils::raw_assistant_output_text_from_item;
use crate::stream_events_utils::record_completed_response_item_with_finalized_facts;
use crate::tasks::emit_compact_metric;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::router::ToolSuggestCandidates;
use crate::tools::router::ToolSuggestPresentation;
use crate::tools::router::record_registration_metrics;
use crate::tools::spec_plan::build_tool_router;
use crate::tools::spec_plan::tool_suggest_enabled;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::turn_timing::record_turn_ttft_metric;
use crate::util::error_or_panic;
use codex_analytics::AppInvocation;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_analytics::InvocationType;
use codex_analytics::TurnResolvedConfigFact;
use codex_analytics::build_track_events_context;
use codex_async_utils::OrCancelExt;
use codex_connectors::AppToolPolicyEvaluator;
use codex_core_plugins::RecommendedPluginCandidatesInput;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputEnvironment;
use codex_features::Feature;
use codex_file_system::EnvironmentAccess;
use codex_file_system::FindUpErrorPolicy;
use codex_file_system::find_nearest_ancestor_with_markers;
use codex_hooks::ModelRequestHookEvent::PostModelRequest;
use codex_hooks::ModelRequestHookEvent::PreModelRequest;
use codex_hooks::ModelRequestStatus;
use codex_login::CodexAuth;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::ResponseItemId;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::PlanItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::AudioReference;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentReasoningSectionBreakEvent;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookExecutionMode;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::PlanDeltaEvent;
use codex_protocol::protocol::ReasoningContentDeltaEvent;
use codex_protocol::protocol::ReasoningRawContentDeltaEvent;
use codex_protocol::protocol::SafetyBufferingEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnDiffEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use codex_skills::ToolMentionKind;
use codex_skills::app_id_from_path;
use codex_skills::build_skill_name_counts;
use codex_skills::collect_explicit_skill_mentions;
use codex_skills::tool_kind_for_path;
use codex_skills_extension::HostSkillPrompts;
use codex_skills_extension::HostSkillsSnapshot;
use codex_skills_extension::InjectedHostSkillPrompts;
use codex_skills_extension::validate_required_skills;
use codex_thread_store::PersistContext;
use codex_tools::DiscoverableTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::filter_request_plugin_install_discoverable_tools_for_client;
use codex_utils_path_uri::PathUri;
use codex_utils_stream_parser::AssistantTextChunk;
use codex_utils_stream_parser::AssistantTextStreamParser;
use codex_utils_stream_parser::ProposedPlanSegment;
use codex_utils_stream_parser::extract_proposed_plan_text;
use codex_utils_stream_parser::strip_citations;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;

// Session-level items that the child modules reach through `super::` paths.
use super::RequestEffortUsage;
use super::TokenUsageContext;
use super::async_compaction;
use super::context_window;
use super::extension_metrics;
use super::file_audio;
use super::rollout_budget;
use super::time_reminder;
use super::token_budget;

mod assistant_text;
mod auto_compaction;
mod context_injection;
mod input_recording;
mod plan_mode;
mod realtime_text;
mod response_stream;
mod run_loop;
mod sampling_request;

use assistant_text::emit_streamed_assistant_text_delta;
use assistant_text::flush_assistant_text_segments_all;
use assistant_text::flush_assistant_text_segments_for_item;
pub(crate) use assistant_text::get_last_assistant_message_from_turn;
use auto_compaction::run_auto_compact;
use auto_compaction::run_pre_sampling_compact;
use context_injection::build_skills_and_plugins;
#[cfg(test)]
pub(super) use context_injection::collect_explicit_app_ids_from_skill_items;
use context_injection::required_mcp_servers_for_input;
pub(crate) use input_recording::recordable_inputs;
pub(crate) use input_recording::run_hooks_and_record_inputs;
use input_recording::turn_input_has_uploaded_audio_file;
pub(crate) use input_recording::turn_user_input;
use input_recording::unsupported_uploaded_audio_error;
use plan_mode::handle_assistant_item_done_in_plan_mode;
use plan_mode::handle_plan_segments;
pub(super) use realtime_text::realtime_text_for_event;
use response_stream::try_run_sampling_request;
pub(crate) use run_loop::run_turn;
pub(crate) use sampling_request::build_prompt;
pub(crate) use sampling_request::built_tools;
pub(crate) use sampling_request::prepare_tool_recommendations;
use sampling_request::run_sampling_request;

const POST_SAMPLING_TOKEN_ESTIMATE_TARGET: &str = "codex_core::post_sampling_token_estimate";

/// Explicit MCP startup requirements retained across restarts within one user turn.
#[derive(Default)]
pub(crate) struct McpStartupRequirements {
    required_servers: Vec<String>,
    required_plugins: HashSet<String>,
}

pub(crate) struct PreparedToolRecommendations {
    auth: Option<CodexAuth>,
    endpoint_candidates: Option<Vec<DiscoverableTool>>,
}

#[derive(Debug)]
struct SamplingRequestResult {
    needs_follow_up: bool,
    /// Queued mail interrupted sampling and must be drained before the next request.
    mailbox_preempted: bool,
    last_agent_message: Option<String>,
}

/// Ephemeral per-response state for streaming a single proposed plan.
/// This is intentionally not persisted or stored in session/state since it
/// only exists while a response is actively streaming. The final plan text
/// is extracted from the completed assistant message.
/// Tracks a single proposed plan item across a streaming response.
struct ProposedPlanItemState {
    item_id: String,
    started: bool,
    completed: bool,
}

/// Aggregated state used only while streaming a plan-mode response.
/// Includes per-item parsers, deferred agent message bookkeeping, and the plan item lifecycle.
struct PlanModeStreamState {
    /// Agent message items started by the model but deferred until we see non-plan text.
    pending_agent_message_items: HashMap<String, TurnItem>,
    /// Agent message items whose start notification has been emitted.
    started_agent_message_items: HashSet<String>,
    /// Leading whitespace buffered until we see non-whitespace text for an item.
    leading_whitespace_by_item: HashMap<String, String>,
    /// Tracks plan item lifecycle while streaming plan output.
    plan_item_state: ProposedPlanItemState,
}

#[derive(Debug, Default)]
pub(super) struct AssistantMessageStreamParsers {
    plan_mode: bool,
    parsers_by_item: HashMap<String, AssistantTextStreamParser>,
}

type ParsedAssistantTextDelta = AssistantTextChunk;

impl AssistantMessageStreamParsers {
    pub(super) fn new(plan_mode: bool) -> Self {
        Self {
            plan_mode,
            parsers_by_item: HashMap::new(),
        }
    }

    fn parser_mut(&mut self, item_id: &str) -> &mut AssistantTextStreamParser {
        let plan_mode = self.plan_mode;
        self.parsers_by_item
            .entry(item_id.to_string())
            .or_insert_with(|| AssistantTextStreamParser::new(plan_mode))
    }

    pub(super) fn seed_item_text(&mut self, item_id: &str, text: &str) -> ParsedAssistantTextDelta {
        if text.is_empty() {
            return ParsedAssistantTextDelta::default();
        }
        self.parser_mut(item_id).push_str(text)
    }

    pub(super) fn parse_delta(&mut self, item_id: &str, delta: &str) -> ParsedAssistantTextDelta {
        self.parser_mut(item_id).push_str(delta)
    }

    pub(super) fn finish_item(&mut self, item_id: &str) -> ParsedAssistantTextDelta {
        let Some(mut parser) = self.parsers_by_item.remove(item_id) else {
            return ParsedAssistantTextDelta::default();
        };
        parser.finish()
    }

    fn drain_finished(&mut self) -> Vec<(String, ParsedAssistantTextDelta)> {
        let parsers_by_item = std::mem::take(&mut self.parsers_by_item);
        parsers_by_item
            .into_iter()
            .map(|(item_id, mut parser)| (item_id, parser.finish()))
            .collect()
    }
}

/// Agent messages are text-only today; concatenate all text entries.
pub(super) fn agent_message_text(item: &codex_protocol::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub(super) enum RealtimeEventText {
    Handoff(String, Option<MessagePhase>),
    QuietReasoning(String),
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
