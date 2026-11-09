//! Skill invocation, plugin lifecycle/measurement and external-agent-import facts; skill ids.

use super::*;

impl AnalyticsReducer {
    pub(super) async fn ingest_skill_invoked(
        &mut self,
        input: SkillInvokedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let SkillInvokedInput {
            tracking,
            invocations,
        } = input;
        for invocation in invocations {
            let (skill_id, skill_scope) = match invocation.location {
                SkillInvocationLocation::Host { path, scope } => {
                    let skill_scope = match scope {
                        SkillScope::User => "user",
                        SkillScope::Repo => "repo",
                        SkillScope::System => "system",
                        SkillScope::Admin => "admin",
                    };
                    let repo_root = path
                        .to_abs_path()
                        .ok()
                        .and_then(|path| get_git_repo_root(path.as_path()));
                    let repo_url = if let Some(root) = repo_root.as_ref() {
                        get_git_origin_url(root).await
                    } else {
                        None
                    };
                    let skill_id = skill_id_for_local_skill(
                        repo_url.as_ref().map(SanitizedGitUrl::as_str),
                        repo_root.as_deref(),
                        &path,
                        invocation.skill_name.as_str(),
                    );
                    (skill_id, Some(skill_scope.to_string()))
                }
                SkillInvocationLocation::Resource {
                    id,
                    skill_id,
                    scope,
                } => {
                    if matches!(invocation.invocation_type, InvocationType::Implicit) {
                        let turn_state = self.turn_state(tracking.turn_id.clone());
                        if !turn_state.resource_skill_invocations.insert(id.clone()) {
                            continue;
                        }
                    }
                    let skill_id = skill_id
                        .unwrap_or_else(|| format!("{:x}", sha1::Sha1::digest(id.as_bytes())));
                    let skill_scope = scope
                        .map(|scope| match scope {
                            SkillScope::User => "user",
                            SkillScope::Repo => "repo",
                            SkillScope::System => "system",
                            SkillScope::Admin => "admin",
                        })
                        .map(str::to_owned);
                    (skill_id, skill_scope)
                }
            };
            let turn = tracking.turn_metadata.as_deref();
            out.push(TrackEventRequest::SkillInvocation(
                SkillInvocationEventRequest {
                    event_type: "skill_invocation",
                    skill_id,
                    skill_name: invocation.skill_name.clone(),
                    event_params: SkillInvocationEventParams {
                        turn_trigger: turn.and_then(TurnAnalyticsMetadata::turn_trigger),
                        thread_id: Some(tracking.thread_id.clone()),
                        turn_id: Some(tracking.turn_id.clone()),
                        voice_session_id: self
                            .turns
                            .get(&tracking.turn_id)
                            .and_then(|turn| turn.voice_session_id.clone()),
                        invoke_type: Some(invocation.invocation_type),
                        model_slug: Some(tracking.model_slug.clone()),
                        product_client_id: Some(tracking.product_client_id.clone()),
                        skill_scope,
                        plugin_id: invocation.plugin_id,
                        remote_plugin_id: invocation.remote_plugin_id,
                    },
                },
            ));
        }
    }

    pub(super) fn ingest_plugin_install_requested(
        &mut self,
        input: PluginInstallRequestedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let PluginInstallRequestedInput { tracking, request } = input;
        out.push(TrackEventRequest::PluginInstallRequested(
            CodexPluginInstallRequestedEventRequest {
                event_type: "codex_plugin_install_requested",
                event_params: codex_plugin_install_requested_metadata(&tracking, request),
            },
        ));
    }

    pub(super) fn ingest_plugin_state_changed(
        &mut self,
        input: PluginStateChangedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let PluginStateChangedInput { plugin, state } = input;
        let event = CodexPluginEventRequest {
            event_type: plugin_state_event_type(state),
            event_params: codex_plugin_metadata(plugin),
        };
        out.push(match state {
            PluginState::Installed => TrackEventRequest::PluginInstalled(event),
            PluginState::Uninstalled => TrackEventRequest::PluginUninstalled(event),
            PluginState::Enabled => TrackEventRequest::PluginEnabled(event),
            PluginState::Disabled => TrackEventRequest::PluginDisabled(event),
        });
    }

    pub(super) fn ingest_plugin_install_failed(
        &mut self,
        input: PluginInstallFailedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let PluginInstallFailedInput {
            plugin,
            source,
            error_type,
            sub_error_type,
        } = input;
        out.push(TrackEventRequest::PluginInstallFailed(
            CodexPluginInstallFailedEventRequest {
                event_type: "codex_plugin_install_failed",
                event_params: CodexPluginInstallFailedMetadata {
                    plugin: codex_plugin_metadata(plugin),
                    source,
                    error_type,
                    sub_error_type,
                },
            },
        ));
    }

    pub(super) fn ingest_external_agent_config_import_completed(
        &mut self,
        input: ExternalAgentConfigImportCompletedInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        out.push(TrackEventRequest::ExternalAgentConfigImportCompleted(
            CodexOnboardingExternalAgentImportCompleteEventRequest {
                event_type: "codex_onboarding_external_agent_import_complete",
                event_params: CodexOnboardingExternalAgentImportCompleteMetadata {
                    import_id: input.import_id,
                    source: input.source,
                    provider_id: input.provider_id,
                    item_type: input.item_type,
                    success_count: input.success_count,
                    failed_count: input.failed_count,
                    product_client_id: Some(originator().value),
                },
            },
        ));
    }

    pub(super) fn ingest_external_agent_config_import_failure(
        &mut self,
        input: ExternalAgentConfigImportFailureInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        out.push(TrackEventRequest::ExternalAgentConfigImportFailure(
            CodexOnboardingExternalAgentImportFailureEventRequest {
                event_type: "codex_onboarding_external_agent_import_failure",
                event_params: CodexOnboardingExternalAgentImportFailureMetadata {
                    import_id: input.import_id,
                    source: input.source,
                    provider_id: input.provider_id,
                    item_type: input.item_type,
                    failure_stage: input.failure_stage,
                    error_type: input.error_type,
                    sub_error_type: input.sub_error_type,
                    product_client_id: Some(originator().value),
                },
            },
        ));
    }

    pub(super) fn ingest_plugin_measurements(
        &mut self,
        input: PluginMeasurementsInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        if input.rows.is_empty()
            || input.rows.len() > MAX_PLUGIN_MEASUREMENTS_PER_BATCH
            || !valid_plugin_measurement_identifier(&input.operation)
        {
            return;
        }
        let PluginMeasurementsInput {
            thread_id,
            turn_id,
            item_id,
            originator,
            model_slug,
            reasoning_effort,
            plugin_id,
            execution_id,
            operation,
            rows,
        } = input;
        out.extend(
            rows.into_iter()
                .filter(valid_plugin_measurement_row)
                .map(|row| {
                    TrackEventRequest::PluginMeasurement(CodexPluginMeasurementEventRequest {
                        event_type: "codex_plugin_measurement_event",
                        event_params: CodexPluginMeasurementEventParams {
                            thread_id: thread_id.clone(),
                            turn_id: turn_id.clone(),
                            item_id: item_id.clone(),
                            originator: originator.clone(),
                            model_slug: model_slug.clone(),
                            reasoning_effort: reasoning_effort.clone(),
                            plugin_id: plugin_id.clone(),
                            execution_id: execution_id.clone(),
                            operation: operation.clone(),
                            measurement_name: row.measurement_name,
                            number_value: row.number_value,
                            dimensions: (!row.dimensions.is_empty()).then_some(row.dimensions),
                        },
                    })
                }),
        );
    }
}

pub(crate) fn skill_id_for_local_skill(
    repo_url: Option<&str>,
    repo_root: Option<&Path>,
    skill_path: &PathUri,
    skill_name: &str,
) -> String {
    let path = normalize_path_for_skill_id(repo_url, repo_root, skill_path);
    let prefix = if let Some(url) = repo_url {
        format!("repo_{url}")
    } else {
        "personal".to_string()
    };
    let raw_id = format!("{prefix}_{path}_{skill_name}");
    let mut hasher = sha1::Sha1::new();
    sha1::Digest::update(&mut hasher, raw_id.as_bytes());
    format!("{:x}", sha1::Digest::finalize(hasher))
}

/// Returns a normalized path for skill ID construction.
///
/// - Repo-scoped skills use a path relative to the repo root.
/// - User/admin/system skills use an absolute path.
pub(crate) fn normalize_path_for_skill_id(
    repo_url: Option<&str>,
    repo_root: Option<&Path>,
    skill_path: &PathUri,
) -> String {
    // Foreign paths have no host repository to inspect; retain their native spelling for telemetry.
    let Ok(skill_path) = skill_path.to_abs_path() else {
        return skill_path.inferred_native_path_string().replace('\\', "/");
    };
    let resolved_path =
        std::fs::canonicalize(skill_path.as_path()).unwrap_or_else(|_| skill_path.to_path_buf());
    match (repo_url, repo_root) {
        (Some(_), Some(root)) => {
            let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            resolved_path
                .strip_prefix(&resolved_root)
                .unwrap_or(resolved_path.as_path())
                .to_string_lossy()
                .replace('\\', "/")
        }
        _ => resolved_path.to_string_lossy().replace('\\', "/"),
    }
}
