//! `Permissions` accessors plus sandbox, permission-profile, and network-proxy `Config` methods.

use super::*;

impl Permissions {
    /// Build permissions from the constrained values required for a minimal
    /// in-process configuration.
    pub fn from_approval_and_profile(
        approval_policy: Constrained<AskForApproval>,
        permission_profile: Constrained<PermissionProfile>,
    ) -> ConstraintResult<Self> {
        Ok(Self {
            approval_policy,
            permission_profile_state: PermissionProfileState::from_constrained_legacy(
                permission_profile,
            )?,
            managed_deny_read_policy: None,
            workspace_roots: Vec::new(),
            network: None,
            allow_login_shell: true,
            shell_environment_policy: ShellEnvironmentPolicy::default(),
            windows_sandbox_mode: None,
            windows_sandbox_type: SandboxType::None,
        })
    }

    pub(crate) fn permission_profile_state(&self) -> &PermissionProfileState {
        &self.permission_profile_state
    }

    pub(crate) fn set_permission_profile_state(
        &mut self,
        permission_profile_state: PermissionProfileState,
    ) {
        self.permission_profile_state = permission_profile_state;
    }

    /// Apply a permission profile snapshot emitted by core session state.
    ///
    /// This is a trusted-state bridge for consumers of `SessionConfigured`.
    /// Config loading and app-server selection should resolve named profiles
    /// through config instead of constructing a snapshot directly.
    pub fn set_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: PermissionProfileSnapshot,
    ) -> ConstraintResult<()> {
        self.permission_profile_state
            .set_permission_profile_snapshot(snapshot)
    }

    /// Replace the current permission constraints with a trusted session
    /// snapshot. This is only for clients that must mirror core session state
    /// after their local config constraints reject the snapshot.
    pub fn replace_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: PermissionProfileSnapshot,
    ) -> ConstraintResult<()> {
        let permission_profile = Constrained::allow_only(snapshot.permission_profile().clone());
        self.permission_profile_state =
            PermissionProfileState::from_constrained_snapshot(permission_profile, snapshot)?;
        Ok(())
    }

    /// Borrow the canonical profile before runtime workspace-root
    /// materialization has been applied.
    pub fn permission_profile(&self) -> &PermissionProfile {
        self.permission_profile_state.permission_profile()
    }

    pub fn can_set_permission_profile(
        &self,
        permission_profile: &PermissionProfile,
    ) -> ConstraintResult<()> {
        let permission_profile =
            self.permission_profile_preserving_managed_denied_reads(permission_profile.clone());
        self.permission_profile_state
            .can_set_legacy_permission_profile(&permission_profile)
    }

    pub fn set_workspace_roots(&mut self, workspace_roots: Vec<AbsolutePathBuf>) {
        self.workspace_roots = workspace_roots;
    }

    pub fn workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    /// Workspace roots that came from user-visible configuration or runtime
    /// selection. Internal Codex-only writable roots are intentionally excluded.
    pub fn user_visible_workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    pub fn profile_workspace_roots(&self) -> &[ProfileWorkspaceRoot] {
        self.permission_profile_state.profile_workspace_roots()
    }

    /// Effective runtime permissions after config requirements and runtime
    /// workspace-root materialization have been applied.
    pub fn effective_permission_profile(&self) -> PermissionProfile {
        self.permission_profile()
            .clone()
            .materialize_project_roots_with_workspace_roots(&self.workspace_roots)
    }

    /// Named profile selected by config, if the current profile has one.
    pub fn active_permission_profile(&self) -> Option<ActivePermissionProfile> {
        self.permission_profile_state.active_permission_profile()
    }

    /// Effective filesystem sandbox policy derived from the canonical profile.
    pub fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        self.effective_permission_profile()
            .file_system_sandbox_policy()
    }

    /// Effective network sandbox policy derived from the canonical profile.
    pub fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        self.permission_profile().network_sandbox_policy()
    }

    /// Legacy compatibility projection derived from the canonical profile.
    pub fn legacy_sandbox_policy(&self, cwd: &Path) -> SandboxPolicy {
        let permission_profile = self.effective_permission_profile();
        compatibility_sandbox_policy_for_permission_profile(&permission_profile, cwd)
    }

    /// Check whether a legacy sandbox policy can be applied to this permission
    /// set after projecting it into the canonical permission profile.
    pub fn can_set_legacy_sandbox_policy(
        &self,
        sandbox_policy: &SandboxPolicy,
        cwd: &Path,
    ) -> ConstraintResult<()> {
        let file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(sandbox_policy, cwd);
        let network_sandbox_policy = NetworkSandboxPolicy::from(sandbox_policy);
        let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &file_system_sandbox_policy,
            network_sandbox_policy,
        );
        self.can_set_permission_profile(&permission_profile)
    }

    /// Set permissions from a legacy sandbox policy and keep every permission
    /// projection in sync.
    pub fn set_legacy_sandbox_policy(
        &mut self,
        sandbox_policy: SandboxPolicy,
        cwd: &Path,
    ) -> ConstraintResult<()> {
        self.can_set_legacy_sandbox_policy(&sandbox_policy, cwd)?;
        let file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&sandbox_policy, cwd);
        let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
        let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
            &file_system_sandbox_policy,
            network_sandbox_policy,
        );
        self.workspace_roots = match &sandbox_policy {
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } => {
                let mut workspace_roots = vec![
                    AbsolutePathBuf::from_absolute_path(cwd)
                        .unwrap_or_else(|_| AbsolutePathBuf::resolve_path_against_base(cwd, "/")),
                ];
                for root in writable_roots {
                    if !workspace_roots.iter().any(|existing| existing == root) {
                        workspace_roots.push(root.clone());
                    }
                }
                workspace_roots
            }
            SandboxPolicy::DangerFullAccess
            | SandboxPolicy::ExternalSandbox { .. }
            | SandboxPolicy::ReadOnly { .. } => vec![
                AbsolutePathBuf::from_absolute_path(cwd)
                    .unwrap_or_else(|_| AbsolutePathBuf::resolve_path_against_base(cwd, "/")),
            ],
        };

        self.set_permission_profile(permission_profile)
    }

    /// Set permissions from the canonical profile.
    pub fn set_permission_profile(
        &mut self,
        permission_profile: PermissionProfile,
    ) -> ConstraintResult<()> {
        let permission_profile =
            self.permission_profile_preserving_managed_denied_reads(permission_profile);
        self.permission_profile_state
            .set_legacy_permission_profile(permission_profile)
    }

    fn permission_profile_preserving_managed_denied_reads(
        &self,
        permission_profile: PermissionProfile,
    ) -> PermissionProfile {
        let Some(managed_deny_read_policy) = self.managed_deny_read_policy.as_ref() else {
            return permission_profile;
        };
        if matches!(
            sandbox_mode_requirement_for_permission_profile(&permission_profile),
            SandboxModeRequirement::DangerFullAccess | SandboxModeRequirement::ExternalSandbox
        ) {
            return permission_profile;
        }
        let enforcement = permission_profile.enforcement();
        let (mut file_system_policy, network_policy) = permission_profile.to_runtime_permissions();
        file_system_policy.preserve_deny_read_restrictions_from(managed_deny_read_policy);
        PermissionProfile::from_runtime_permissions_with_enforcement(
            enforcement,
            &file_system_policy,
            network_policy,
        )
    }
}

pub(super) fn build_network_proxy_spec(
    mut configured_network_proxy_config: NetworkProxyConfig,
    network_requirements: Option<Sourced<codex_config::NetworkConstraints>>,
    permission_profile: &PermissionProfile,
    environment_overrides: &HashMap<String, String>,
    features: &ManagedFeatures,
    feature_settings: Option<&FeaturesToml>,
) -> std::io::Result<Option<NetworkProxySpec>> {
    if features.enabled(Feature::CredentialMasking) {
        configured_network_proxy_config.set_credential_broker_enabled(/*enabled*/ true);
        if let Some(credentials) = network_proxy_toml_config(feature_settings)
            .and_then(|network_proxy| network_proxy.credentials.as_ref())
        {
            configured_network_proxy_config
                .credential_providers
                .clone_from(credentials);
        }
    }
    configured_network_proxy_config.configure_credential_broker_environment(environment_overrides);
    PreparedNetworkConfig {
        configured_proxy: configured_network_proxy_config,
    }
    .build(network_requirements, permission_profile)
}

impl Config {
    pub fn legacy_sandbox_policy(&self) -> SandboxPolicy {
        self.permissions.legacy_sandbox_policy(self.cwd.as_path())
    }

    pub fn set_legacy_sandbox_policy(
        &mut self,
        sandbox_policy: SandboxPolicy,
    ) -> ConstraintResult<()> {
        self.workspace_roots_explicit = matches!(
            &sandbox_policy,
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } if !writable_roots.is_empty()
        );
        self.permissions
            .set_legacy_sandbox_policy(sandbox_policy, self.cwd.as_path())?;
        self.workspace_roots = self.permissions.workspace_roots().to_vec();
        Ok(())
    }

    /// Combine runtime and profile roots without interpreting them on the current host.
    pub fn effective_workspace_roots(&self) -> Vec<PathUri> {
        let mut workspace_roots = self
            .workspace_roots
            .iter()
            .map(PathUri::from_abs_path)
            .collect::<Vec<_>>();
        workspace_roots.extend(
            self.permissions
                .profile_workspace_roots()
                .iter()
                .map(|root| root.as_uri().clone()),
        );
        // Preserve spelling changes even when Windows path comparison folds case.
        let mut seen = HashSet::new();
        workspace_roots.retain(|root| seen.insert(root.to_string()));
        workspace_roots
    }

    pub fn set_windows_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxModeToml::Unelevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxModeToml::Unelevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    pub fn set_windows_elevated_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxModeToml::Elevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxModeToml::Elevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    pub fn managed_network_requirements_enabled(&self) -> bool {
        !matches!(
            self.permissions.permission_profile(),
            PermissionProfile::Disabled
        ) && self.config_layer_stack.requirements().network.is_some()
    }

    /// Resolves a named permission profile from effective config and managed requirements.
    pub fn resolve_permission_profile(
        &self,
        profile_name: &str,
    ) -> std::io::Result<PermissionProfileToml> {
        let cfg =
            ConfigToml::try_from(self.config_layer_stack.effective_config()).map_err(|err| {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "failed to read effective config for selected permission profile: {err}"
                    ),
                )
            })?;
        let permissions = merge_managed_permission_profiles(
            cfg.permissions.as_ref(),
            self.config_layer_stack.requirements_toml(),
        )?
        .unwrap_or_default();

        permissions::resolve_permission_profile(&permissions, profile_name)
    }

    pub fn network_proxy_spec_for_active_permission_profile(
        &self,
        active_permission_profile: &ActivePermissionProfile,
        permission_profile: &PermissionProfile,
    ) -> std::io::Result<Option<NetworkProxySpec>> {
        let profile_allows_network_proxy =
            profile_allows_configured_network_proxy(permission_profile);
        let cfg =
            ConfigToml::try_from(self.config_layer_stack.effective_config()).map_err(|err| {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "failed to read effective config for selected permission profile: {err}"
                    ),
                )
            })?;
        let configured_network_proxy_config = if profile_allows_network_proxy {
            let permissions = merge_managed_permission_profiles(
                cfg.permissions.as_ref(),
                self.config_layer_stack.requirements_toml(),
            )?;
            network_proxy_config_for_profile_selection(
                permissions.as_ref(),
                active_permission_profile.id.as_str(),
            )?
        } else {
            NetworkProxyConfig::default()
        };
        let prepared_network = PreparedNetworkConfig::from_inputs(NetworkConfigInputs {
            configured_proxy: configured_network_proxy_config,
            feature_enabled: profile_allows_network_proxy
                && self.features.enabled(Feature::NetworkProxy),
            features: cfg.features.as_ref(),
            candidate_permission_profile: permission_profile,
            credential_broker_base_url: cfg.openai_base_url.as_deref(),
        });
        build_network_proxy_spec(
            prepared_network.configured_proxy,
            self.config_layer_stack.requirements().network.clone(),
            permission_profile,
            &self.permissions.shell_environment_policy.r#set,
            &self.features,
            cfg.features.as_ref(),
        )
    }

    pub(crate) fn credential_masking_warning(
        &self,
        permission_profile: &PermissionProfile,
    ) -> Option<&'static str> {
        if !self.features.enabled(Feature::CredentialMasking) {
            return None;
        }
        // The session bypasses the configured proxy for unsandboxed execution,
        // even when managed requirements keep the proxy spec enabled.
        if matches!(permission_profile, PermissionProfile::Disabled) {
            return Some(
                "Credential masking is inactive because sandboxing is disabled. Commands will receive unmasked credentials. Select a sandboxed permission profile to use credential masking.",
            );
        }
        if self
            .permissions
            .network
            .as_ref()
            .is_some_and(NetworkProxySpec::credential_broker_enabled)
        {
            return None;
        }
        if self
            .permissions
            .network
            .as_ref()
            .is_some_and(NetworkProxySpec::enabled)
        {
            Some(
                "Credential masking is inactive because shell environment overrides contain conflicting case-insensitive provider keys. Commands will receive unmasked credentials. Remove the conflicting entries from `shell_environment_policy.set`.",
            )
        } else {
            Some(
                "Credential masking is inactive because the network proxy is disabled. Commands will receive unmasked credentials. Enable `features.network_proxy` or configure managed network requirements to use credential masking.",
            )
        }
    }

    pub fn bundled_skills_enabled(&self) -> bool {
        codex_config::bundled_skills_enabled_from_stack(&self.config_layer_stack)
    }

    /// Returns whether effective requirements allow selecting a concrete profile.
    pub fn is_permission_profile_allowed(
        &self,
        profile_id: &str,
        permission_profile: &PermissionProfile,
    ) -> bool {
        permission_profile_is_allowed(&self.config_layer_stack, profile_id, permission_profile)
    }
}
