//! Approval, permission, and user-input requests and their responses.

use super::*;

impl Session {
    /// Adds an execpolicy amendment to both the in-memory and on-disk policies so future
    /// commands can use the newly approved prefix.
    pub(crate) async fn persist_execpolicy_amendment(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> Result<(), ExecPolicyUpdateError> {
        let codex_home = self
            .state
            .lock()
            .await
            .session_configuration
            .codex_home()
            .clone();

        self.services
            .exec_policy
            .append_amendment_and_update(&codex_home, amendment)
            .await?;

        Ok(())
    }

    pub(crate) async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .filter(|task| task.turn_context.sub_id == sub_id)
            .map(|task| Arc::clone(&task.turn_context))
    }

    pub(super) async fn active_turn_context_and_cancellation_token(
        &self,
    ) -> Option<(Arc<TurnContext>, CancellationToken)> {
        let active = self.active_turn.lock().await;
        let task = active.as_ref()?.task.as_ref()?;
        Some((
            Arc::clone(&task.turn_context),
            task.cancellation_token.child_token(),
        ))
    }

    pub(crate) async fn persist_network_policy_amendment(
        &self,
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
        on_policy_applied: impl FnOnce() + Send,
    ) -> anyhow::Result<()> {
        let _refresh_guard = self
            .managed_network_proxy_refresh_lock
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("managed network proxy refresh semaphore closed"))?;
        let host =
            Self::validated_network_policy_amendment_host(amendment, network_approval_context)?;
        let codex_home = self
            .state
            .lock()
            .await
            .session_configuration
            .codex_home()
            .clone();
        let started_proxy = self.services.network_proxy.load_full();
        let proxy = started_proxy.as_deref().map(StartedNetworkProxy::proxy);
        self.services
            .exec_policy
            .apply_network_rule_and_persist(
                &codex_home,
                &host,
                amendment,
                network_approval_context,
                proxy.as_ref(),
                on_policy_applied,
            )
            .await
    }

    pub(super) fn validated_network_policy_amendment_host(
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<String> {
        let approved_host = normalize_host(&network_approval_context.host);
        let amendment_host = normalize_host(&amendment.host);
        if amendment_host != approved_host {
            return Err(anyhow::anyhow!(
                "network policy amendment host '{}' does not match approved host '{}'",
                amendment.host,
                network_approval_context.host
            ));
        }
        Ok(approved_host)
    }

    pub(crate) async fn record_network_policy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &NetworkPolicyAmendment,
    ) {
        let message: ResponseItem = ContextualUserFragment::into(NetworkRuleSaved::new(amendment));
        let turn_context = self.turn_context_for_sub_id(sub_id).await;
        self.inject_no_new_turn(vec![message], turn_context.as_deref())
            .await;
    }

    pub(super) fn user_wait_timing_owner(
        &self,
        turn_context: &TurnContext,
        call_id: Option<&str>,
    ) -> Option<Arc<codex_direct_tool_timing::ToolTimingGuard>> {
        if !turn_context
            .config
            .features
            .enabled(Feature::CriticalPathTiming)
        {
            return None;
        }
        // Retain the invocation identity before request preparation can suspend.
        call_id.and_then(|call_id| {
            turn_context
                .turn_timing_state
                .direct_tool_timings
                .owner_for_turn(call_id, &turn_context.sub_id)
        })
    }

    pub(super) fn start_user_wait_timing(
        &self,
        turn_context: &TurnContext,
        owner: Option<&codex_direct_tool_timing::ToolTimingGuard>,
    ) -> Option<codex_direct_tool_timing::ToolBlockingTimingGuard> {
        let timing = owner.and_then(codex_direct_tool_timing::ToolTimingGuard::start_user_wait);
        if timing.is_none()
            && turn_context
                .config
                .features
                .enabled(Feature::CriticalPathTiming)
        {
            turn_context.turn_timing_state.invalidate_critical_path();
        }
        timing
    }

    /// Keep tool results paused and attribute a queued network prompt to its triggering call.
    pub(crate) fn hold_queued_network_approval(
        &self,
        turn: &TurnContext,
        call_id: Option<&str>,
    ) -> (
        crate::elicitation::ElicitationRegistration,
        Option<codex_direct_tool_timing::ToolBlockingTimingGuard>,
    ) {
        let owner = self.user_wait_timing_owner(turn, call_id);
        (
            self.services.elicitations.register(),
            self.start_user_wait_timing(turn, owner.as_deref()),
        )
    }

    /// Emit an exec approval request event and await the user's decision.
    ///
    /// The request is keyed by `call_id` + `approval_id` so matching responses
    /// are delivered to the correct in-flight turn. If the pending approval is
    /// cleared before a response arrives, treat it as an abort so interrupted
    /// turns do not continue on a synthetic denial.
    ///
    /// Note that if `available_decisions` is `None`, then the other fields will
    /// be used to derive the available decisions via
    /// [ExecApprovalRequestEvent::default_available_decisions].
    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_command_approval(
        &self,
        turn_context: &TurnContext,
        kind: ExecApprovalKind,
        model_context: ModelInvocationContext,
        call_id: String,
        timing_call_id: Option<&str>,
        approval_id: Option<String>,
        environment_id: Option<String>,
        command: Vec<String>,
        cwd: PathUri,
        reason: Option<String>,
        network_approval: Option<(NetworkApprovalContext, NetworkApprovalTurn)>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
        additional_permissions: Option<AdditionalPermissionProfile>,
        available_decisions: Option<Vec<ReviewDecision>>,
        plugin_attribution_override: Option<PluginCommandAttribution>,
    ) -> ReviewDecision {
        let (network_approval_context, turn_binding) = network_approval.unzip();
        let user_wait_owner = self.user_wait_timing_owner(turn_context, timing_call_id);
        let _elicitation = self.services.elicitations.register();
        //  command-level approvals use `call_id`.
        // `approval_id` identifies subcommand callbacks and stdin writes.
        let effective_approval_id = approval_id.clone().unwrap_or_else(|| call_id.clone());
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let (prev_entry, _network_prompt) = {
            let mut active = self.active_turn.lock().await;
            // The proxy reviews with the active turn's settings. A sandbox retry
            // keeps its originating step's settings even after a background cell yields.
            if let Some(binding) = turn_binding
                && active
                    .as_ref()
                    .and_then(|turn| turn.task.as_ref())
                    .is_none_or(|task| {
                        binding == NetworkApprovalTurn::Originating
                            && task.turn_context.sub_id != turn_context.sub_id
                    })
            {
                return ReviewDecision::Abort;
            }
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    let prev = ts.insert_pending_approval(
                        effective_approval_id.clone(),
                        tx_approve,
                        kind,
                        turn_context.sub_id.clone(),
                    );
                    let prompt = network_approval_context.as_ref().map(|_| {
                        ts.track_network_prompt(
                            &self.tx_event,
                            &turn_context.sub_id,
                            &effective_approval_id,
                        )
                    });
                    (prev, prompt)
                }
                None => (None, None),
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {effective_approval_id}");
        }

        let parsed_cmd = parse_command(&command);
        let proposed_network_policy_amendments = network_approval_context.as_ref().map(|context| {
            vec![
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Allow,
                },
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Deny,
                },
            ]
        });
        let available_decisions = available_decisions.unwrap_or_else(|| {
            ExecApprovalRequestEvent::default_available_decisions(
                network_approval_context.as_ref(),
                proposed_execpolicy_amendment.as_ref(),
                proposed_network_policy_amendments.as_deref(),
                additional_permissions.as_ref(),
            )
        });
        let plugin_attribution = plugin_attribution_override.or_else(|| {
            cwd.to_abs_path()
                .ok()
                .and_then(|cwd| turn_context.plugin_attribution_for_command(&command, &cwd))
        });
        let (plugin_id, script_path) = plugin_attribution
            .as_ref()
            .map(PluginCommandAttribution::serialized_fields)
            .unzip();
        let event = EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            model_context: Some(model_context),
            kind,
            call_id,
            plugin_id,
            script_path,
            approval_id,
            turn_id: turn_context.sub_id.clone(),
            environment_id,
            started_at_ms: now_unix_timestamp_ms(),
            command,
            cwd: cwd.into(),
            reason,
            network_approval_context,
            proposed_execpolicy_amendment,
            proposed_network_policy_amendments,
            additional_permissions,
            available_decisions: Some(available_decisions),
            parsed_cmd,
        });
        let _user_wait = self.start_user_wait_timing(turn_context, user_wait_owner.as_deref());
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or(ReviewDecision::Abort)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_patch_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        changes: HashMap<PathBuf, FileChange>,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> ReviewDecision {
        let user_wait_owner = self.user_wait_timing_owner(turn_context, Some(&call_id));
        let _elicitation = self.services.elicitations.register();
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let approval_id = call_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(
                        approval_id.clone(),
                        tx_approve,
                        ExecApprovalKind::Command,
                        turn_context.sub_id.clone(),
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {approval_id}");
        }

        let event = EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            started_at_ms: now_unix_timestamp_ms(),
            changes,
            reason,
            grant_root,
        });
        let _user_wait = self.start_user_wait_timing(turn_context, user_wait_owner.as_deref());
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or(ReviewDecision::Abort)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn request_permissions_for_environment(
        self: &Arc<Self>,
        step_context: &Arc<StepContext>,
        call_id: String,
        request: PreparedPermissionRequest,
        cancellation_token: CancellationToken,
    ) -> Option<RequestPermissionsResponse> {
        let turn_context = &step_context.turn;
        let user_wait_owner = self.user_wait_timing_owner(turn_context, Some(&call_id));
        let approval_policy = step_context.settings.approval_policy();
        let Some(environment) = step_context
            .environments
            .turn_environments()
            .find(|candidate| candidate.selection.environment_id == request.environment_id)
            .cloned()
        else {
            return Some(RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            });
        };
        let cwd = environment.cwd();
        match approval_policy {
            AskForApproval::Never => {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                    strict_auto_review: false,
                });
            }
            AskForApproval::Granular(granular_config)
                if !granular_config.allows_request_permissions() =>
            {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                    strict_auto_review: false,
                });
            }
            AskForApproval::OnRequest
            | AskForApproval::UnlessTrusted
            | AskForApproval::Granular(_) => {}
        }

        let action = request.approval_action(&call_id, &turn_context.sub_id);
        let requested_permissions = request.permissions;
        let sandbox_context = environment.sandbox_context(/*additional_permissions*/ None);
        let context = sandbox_context.policy_context();
        {
            let approval_context = ApprovalContext {
                exec_requirement: None,
                review_context: crate::guardian::GuardianReviewContext::from(step_context),
                cancellation_token: Some(cancellation_token.clone()),
                call_id: call_id.clone(),
                tool_name: ToolName::plain("request_permissions"),
                strict_auto_review: false,
                approval_reason: None,
                retry_reason: None,
                network_approval_context: None,
            };
            let decision = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => return None,
                decision = self.request_guardian_approval(
                    action,
                    &approval_context,
                ) => decision,
            };
            if let Some(decision) = decision {
                let (permissions, scope) = match decision {
                    ReviewDecision::Approved
                    | ReviewDecision::ApprovedExecpolicyAmendment { .. }
                    | ReviewDecision::NetworkPolicyAmendment {
                        network_policy_amendment:
                            NetworkPolicyAmendment {
                                action: NetworkPolicyRuleAction::Allow,
                                ..
                            },
                    } => (requested_permissions.clone(), PermissionGrantScope::Turn),
                    ReviewDecision::ApprovedForSession => {
                        (requested_permissions.clone(), PermissionGrantScope::Session)
                    }
                    ReviewDecision::ApprovedMcpPolicyAmendment
                    | ReviewDecision::NetworkPolicyAmendment {
                        network_policy_amendment:
                            NetworkPolicyAmendment {
                                action: NetworkPolicyRuleAction::Deny,
                                ..
                            },
                    }
                    | ReviewDecision::Abort
                    | ReviewDecision::Denied { .. }
                    | ReviewDecision::TimedOut => (
                        RequestPermissionProfile::default(),
                        PermissionGrantScope::Turn,
                    ),
                };
                let response = RequestPermissionsResponse {
                    permissions,
                    scope,
                    strict_auto_review: false,
                };
                let response = Self::normalize_request_permissions_response(
                    requested_permissions,
                    response,
                    &context,
                );
                self.record_granted_request_permissions_for_turn(
                    &response,
                    &environment.selection.environment_id,
                    turn_context,
                );
                return Some(response);
            }
        }

        let _elicitation = self.services.elicitations.register();
        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_request_permissions(
                        call_id.clone(),
                        PendingRequestPermissions {
                            tx_response,
                            requested_permissions: requested_permissions.clone(),
                            environment: environment.clone(),
                            turn_context: Arc::clone(turn_context),
                        },
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending request_permissions for call_id: {call_id}");
        }

        let event = EventMsg::RequestPermissions(RequestPermissionsEvent {
            call_id: call_id.clone(),
            turn_id: turn_context.sub_id.clone(),
            environment_id: Some(environment.selection.environment_id.clone()),
            started_at_ms: now_unix_timestamp_ms(),
            reason: request.reason,
            permissions: requested_permissions,
            cwd: Some(cwd.clone().into()),
        });
        let _user_wait = self.start_user_wait_timing(turn_context, user_wait_owner.as_deref());
        self.send_event(turn_context.as_ref(), event).await;
        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                drop(_user_wait);
                let mut active = self.active_turn.lock().await;
                if let Some(at) = active.as_mut() {
                    let mut ts = at.turn_state.lock().await;
                    let _ = ts.remove_pending_request_permissions(&call_id);
                }
                None
            }
            response = rx_response => response.ok(),
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn request_user_input(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> Option<AcceptedUserInputResponse> {
        let user_wait_owner = self.user_wait_timing_owner(turn_context, Some(&call_id));
        let _elicitation = self.services.elicitations.register();
        let sub_id = turn_context.sub_id.clone();
        let (tx_response, rx_response) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_user_input(sub_id, tx_response)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending user input for sub_id: {event_id}");
        }

        let event = EventMsg::RequestUserInput(RequestUserInputEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            questions: args.questions,
            is_blocking: args.is_blocking,
            auto_resolution_ms: args.auto_resolution_ms,
        });
        turn_context
            .turn_metadata_state
            .mark_user_input_requested_during_turn();
        let _user_wait = self.start_user_wait_timing(turn_context, user_wait_owner.as_deref());
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_user_input_response(
        &self,
        sub_id: &str,
        response: RequestUserInputResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let sender = at.turn_state.lock().await.remove_pending_user_input(sub_id);
                    match sender {
                        Some(sender) => Some((sender, self.reserve_user_input_order().await)),
                        None => None,
                    }
                }
                None => None,
            }
        };
        match entry {
            Some((tx_response, acceptance_order)) => {
                tx_response
                    .send(AcceptedUserInputResponse {
                        response,
                        acceptance_order,
                    })
                    .ok();
            }
            None => {
                warn!("No pending user input found for sub_id: {sub_id}");
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_request_permissions_response(
        &self,
        call_id: &str,
        response: RequestPermissionsResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_request_permissions(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(entry) => {
                let sandbox_context = entry
                    .environment
                    .sandbox_context(/*additional_permissions*/ None);
                let response = Self::normalize_request_permissions_response(
                    entry.requested_permissions,
                    response,
                    &sandbox_context.policy_context(),
                );
                self.record_granted_request_permissions_for_turn(
                    &response,
                    &entry.environment.selection.environment_id,
                    &entry.turn_context,
                );
                entry.tx_response.send(response).ok();
            }
            None => {
                warn!("No pending request_permissions found for call_id: {call_id}");
            }
        }
    }

    pub(super) fn normalize_request_permissions_response(
        requested_permissions: RequestPermissionProfile,
        response: RequestPermissionsResponse,
        context: &FileSystemSandboxPolicyContext<'_>,
    ) -> RequestPermissionsResponse {
        if response.strict_auto_review && matches!(response.scope, PermissionGrantScope::Session) {
            return RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            };
        }

        if response.permissions.is_empty() {
            return response;
        }

        RequestPermissionsResponse {
            permissions: intersect_permission_profiles_with_context(
                requested_permissions.into(),
                response.permissions.into(),
                context,
            )
            .into(),
            scope: response.scope,
            strict_auto_review: response.strict_auto_review,
        }
    }

    pub(super) fn record_granted_request_permissions_for_turn(
        &self,
        response: &RequestPermissionsResponse,
        environment_id: &str,
        turn_context: &TurnContext,
    ) {
        if response.permissions.is_empty() {
            return;
        }
        match response.scope {
            PermissionGrantScope::Turn => {
                let permissions: AdditionalPermissionProfile = response.permissions.clone().into();
                turn_context.record_granted_permissions(
                    environment_id,
                    permissions,
                    response.strict_auto_review,
                );
            }
            PermissionGrantScope::Session => {
                self.services.record_granted_permissions(
                    environment_id,
                    response.permissions.clone().into(),
                );
            }
        }
    }

    pub(crate) async fn active_turn_context_and_strict_auto_review(
        &self,
    ) -> Option<(
        Arc<TurnContext>,
        Arc<ResolvedStepSettings>,
        TurnEnvironmentSnapshot,
        bool,
    )> {
        let active = self.active_turn.lock().await;
        let active = active.as_ref()?;
        let task = active.task.as_ref()?;
        let turn_context = Arc::clone(&task.turn_context);
        let settings = turn_context.next_step_settings.load_full();
        let strict_auto_review = turn_context.strict_auto_review_enabled();
        let environments = self.services.turn_environments.snapshot_now();
        Some((turn_context, settings, environments, strict_auto_review))
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_dynamic_tool_response(&self, call_id: &str, response: DynamicToolResponse) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending dynamic tool call found for call_id: {call_id}");
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(super) async fn take_pending_approval(
        &self,
        approval_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Option<(PendingApproval, Option<String>)> {
        let active = self.active_turn.lock().await;
        let turn = active.as_ref()?;
        // A live Cancel interrupts the turn servicing the prompt, which can differ from its origin.
        let turn_id = turn.task.as_ref().map(|task| &task.turn_context.sub_id);
        let pending = turn
            .turn_state
            .lock()
            .await
            .remove_pending_approval(approval_id, expected_turn_id)?;
        (!pending.sender.is_closed()).then(|| (pending, turn_id.cloned()))
    }

    pub async fn notify_approval(&self, approval_id: &str, decision: ReviewDecision) {
        if let Some((pending, _)) = self
            .take_pending_approval(approval_id, /*expected_turn_id*/ None)
            .await
        {
            let decision = pending.validate(decision);
            let _ = pending.sender.send(decision);
        } else {
            warn!("No pending approval found for call_id: {approval_id}");
        }
    }
}
