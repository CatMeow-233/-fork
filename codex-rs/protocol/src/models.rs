use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use codex_utils_image::PromptImageMode;
use codex_utils_image::data_url_from_bytes;
use codex_utils_image::load_for_prompt_bytes;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::ser::Serializer;
use serde_with::serde_as;
use ts_rs::TS;

use crate::local_media::audio_mime_for_path;
use crate::permissions::FileSystemAccessMode;
use crate::permissions::FileSystemPath;
use crate::permissions::FileSystemSandboxEntry;
use crate::permissions::FileSystemSandboxKind;
use crate::permissions::FileSystemSandboxPolicy;
use crate::permissions::FileSystemSpecialPath;
use crate::permissions::NetworkSandboxPolicy;
use crate::permissions::RawFileSystemSandboxEntry;
use crate::protocol::SandboxPolicy;
use crate::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_image::ImageProcessingError;
use schemars::JsonSchema;

use crate::ResponseItemId;
use crate::mcp::CallToolResult;
use codex_utils_path_uri::PathUri;

mod configuration_update;
mod executed_tool_calls;

mod item_metadata;

pub use crate::local_media::MAX_PROMPT_AUDIO_INPUT_BYTES;
pub use crate::local_media::snapshot_local_user_input;
pub use crate::permission_profile_snapshot::PermissionProfileSnapshot;
pub use crate::permission_profile_snapshot::ProfileWorkspaceRoot;
pub use configuration_update::ConfigurationReasoning;
pub use executed_tool_calls::ExecutedToolCall;
pub use executed_tool_calls::ExecutedToolCallArguments;
pub use executed_tool_calls::ExecutedToolCallTruncation;
pub use executed_tool_calls::MAX_TOOL_RESULT_SOURCE_FIELD_BYTES;
pub use executed_tool_calls::ToolResultMetadata;
pub use executed_tool_calls::ToolResultSource;
pub use executed_tool_calls::ToolResultSources;
pub use executed_tool_calls::bound_executed_tool_calls_for_message;
pub use executed_tool_calls::executed_tool_call_metadata_bytes;
pub use executed_tool_calls::normalize_executed_tool_call_completeness;
pub use item_metadata::ContentItemKind;
pub use item_metadata::ContentItemMetadata;
pub use item_metadata::ContentItemNamespace;
pub use item_metadata::ContentItemProvenance;

/// Controls the per-command sandbox override requested by a shell-like tool call.
#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    /// Run with the turn's configured sandbox policy unchanged.
    #[default]
    UseDefault,
    /// Request to run outside the sandbox.
    RequireEscalated,
    /// Request to stay in the sandbox while widening permissions for this
    /// command only.
    WithAdditionalPermissions,
}

impl SandboxPermissions {
    /// True if SandboxPermissions requires full unsandboxed execution (i.e. RequireEscalated)
    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, SandboxPermissions::RequireEscalated)
    }

    /// True if SandboxPermissions requests any explicit per-command override
    /// beyond `UseDefault`.
    pub fn requests_sandbox_override(self) -> bool {
        !matches!(self, SandboxPermissions::UseDefault)
    }

    /// True if SandboxPermissions uses the sandboxed per-command permission
    /// widening flow.
    pub fn uses_additional_permissions(self) -> bool {
        matches!(self, SandboxPermissions::WithAdditionalPermissions)
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, JsonSchema, TS)]
pub struct FileSystemPermissions {
    #[schemars(with = "Vec<RawFileSystemSandboxEntry>")]
    #[ts(as = "Vec<RawFileSystemSandboxEntry>")]
    pub entries: Vec<FileSystemSandboxEntry>,
    pub glob_scan_max_depth: Option<NonZeroUsize>,
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyReadWriteRoots {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<AbsolutePathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<AbsolutePathBuf>>,
}

impl FileSystemPermissions {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn from_read_write_roots(
        read: Option<Vec<AbsolutePathBuf>>,
        write: Option<Vec<AbsolutePathBuf>>,
    ) -> Self {
        Self::from_paths(read, write)
    }

    pub fn from_read_write_path_uris(
        read: Option<Vec<PathUri>>,
        write: Option<Vec<PathUri>>,
    ) -> Self {
        Self::from_paths(read, write)
    }

    fn from_paths<P>(read: Option<Vec<P>>, write: Option<Vec<P>>) -> Self
    where
        P: Into<FileSystemPath>,
    {
        let mut entries = Vec::new();
        if let Some(read) = read {
            entries.extend(
                read.into_iter().map(|path| {
                    FileSystemSandboxEntry::new(path.into(), FileSystemAccessMode::Read)
                }),
            );
        }
        if let Some(write) = write {
            entries.extend(
                write.into_iter().map(|path| {
                    FileSystemSandboxEntry::new(path.into(), FileSystemAccessMode::Write)
                }),
            );
        }
        Self {
            entries,
            glob_scan_max_depth: None,
        }
    }

    pub fn legacy_read_write_roots(&self) -> Option<LegacyReadWriteRoots> {
        self.as_legacy_permissions()
    }

    fn as_legacy_permissions(&self) -> Option<LegacyReadWriteRoots> {
        if self.glob_scan_max_depth.is_some() {
            return None;
        }

        let mut read = Vec::new();
        let mut write = Vec::new();

        for entry in &self.entries {
            let FileSystemPath::Path { path } = &entry.path else {
                return None;
            };
            let path = path.to_abs_path().ok()?;
            match entry.access {
                FileSystemAccessMode::Read => read.push(path),
                FileSystemAccessMode::Write => write.push(path),
                FileSystemAccessMode::Deny => return None,
            }
        }

        Some(LegacyReadWriteRoots {
            read: (!read.is_empty()).then_some(read),
            write: (!write.is_empty()).then_some(write),
        })
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalFileSystemPermissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<RawFileSystemSandboxEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    glob_scan_max_depth: Option<NonZeroUsize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FileSystemPermissionsDe {
    Canonical(CanonicalFileSystemPermissions),
    Legacy(LegacyReadWriteRoots),
}

impl Serialize for FileSystemPermissions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(legacy) = self.as_legacy_permissions() {
            legacy.serialize(serializer)
        } else {
            CanonicalFileSystemPermissions {
                entries: self
                    .entries
                    .clone()
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()
                    .map_err(serde::ser::Error::custom)?,
                glob_scan_max_depth: self.glob_scan_max_depth,
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for FileSystemPermissions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match FileSystemPermissionsDe::deserialize(deserializer)? {
            FileSystemPermissionsDe::Canonical(CanonicalFileSystemPermissions {
                entries,
                glob_scan_max_depth,
            }) => Ok(Self {
                entries: entries
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()
                    .map_err(serde::de::Error::custom)?,
                glob_scan_max_depth,
            }),
            FileSystemPermissionsDe::Legacy(LegacyReadWriteRoots { read, write }) => {
                Ok(Self::from_read_write_roots(read, write))
            }
        }
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct NetworkPermissions {
    pub enabled: Option<bool>,
}

impl NetworkPermissions {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

/// Partial permission overlay used for per-command requests and approved
/// session/turn grants.
#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AdditionalPermissionProfile {
    pub network: Option<NetworkPermissions>,
    pub file_system: Option<FileSystemPermissions>,
}

impl AdditionalPermissionProfile {
    pub fn is_empty(&self) -> bool {
        self.network.is_none() && self.file_system.is_none()
    }
}

#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEnforcement {
    /// Codex owns sandbox construction for this profile.
    #[default]
    Managed,
    /// No outer filesystem sandbox should be applied.
    Disabled,
    /// Filesystem isolation is enforced by an external caller.
    External,
}

impl SandboxEnforcement {
    pub fn from_legacy_sandbox_policy(sandbox_policy: &SandboxPolicy) -> Self {
        match sandbox_policy {
            SandboxPolicy::DangerFullAccess => Self::Disabled,
            SandboxPolicy::ExternalSandbox { .. } => Self::External,
            SandboxPolicy::ReadOnly { .. } | SandboxPolicy::WorkspaceWrite { .. } => Self::Managed,
        }
    }
}

/// Filesystem permissions for profiles where Codex owns sandbox construction.
#[derive(Debug, Clone, Eq, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum ManagedFileSystemPermissions {
    /// Apply a managed filesystem sandbox from the listed entries.
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    Restricted {
        #[schemars(with = "Vec<RawFileSystemSandboxEntry>")]
        #[ts(as = "Vec<RawFileSystemSandboxEntry>")]
        entries: Vec<FileSystemSandboxEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        glob_scan_max_depth: Option<NonZeroUsize>,
    },
    /// Apply a managed sandbox that allows all filesystem access.
    Unrestricted,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SerializedManagedFileSystemPermissions {
    #[serde(rename_all = "snake_case")]
    Restricted {
        entries: Vec<RawFileSystemSandboxEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob_scan_max_depth: Option<NonZeroUsize>,
    },
    Unrestricted,
}

impl Serialize for ManagedFileSystemPermissions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Restricted {
                entries,
                glob_scan_max_depth,
            } => SerializedManagedFileSystemPermissions::Restricted {
                entries: entries
                    .clone()
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()
                    .map_err(serde::ser::Error::custom)?,
                glob_scan_max_depth: *glob_scan_max_depth,
            }
            .serialize(serializer),
            Self::Unrestricted => {
                SerializedManagedFileSystemPermissions::Unrestricted.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ManagedFileSystemPermissions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(
            match SerializedManagedFileSystemPermissions::deserialize(deserializer)? {
                SerializedManagedFileSystemPermissions::Restricted {
                    entries,
                    glob_scan_max_depth,
                } => Self::Restricted {
                    entries: entries
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()
                        .map_err(serde::de::Error::custom)?,
                    glob_scan_max_depth,
                },
                SerializedManagedFileSystemPermissions::Unrestricted => Self::Unrestricted,
            },
        )
    }
}

impl ManagedFileSystemPermissions {
    fn from_sandbox_policy(file_system_sandbox_policy: &FileSystemSandboxPolicy) -> Self {
        match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::Restricted => Self::Restricted {
                entries: file_system_sandbox_policy.entries.clone(),
                glob_scan_max_depth: file_system_sandbox_policy
                    .glob_scan_max_depth
                    .and_then(NonZeroUsize::new),
            },
            FileSystemSandboxKind::Unrestricted => Self::Unrestricted,
            FileSystemSandboxKind::ExternalSandbox => unreachable!(
                "external filesystem policies are represented by PermissionProfile::External"
            ),
        }
    }

    pub fn to_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        match self {
            Self::Restricted {
                entries,
                glob_scan_max_depth,
            } => FileSystemSandboxPolicy {
                kind: FileSystemSandboxKind::Restricted,
                glob_scan_max_depth: glob_scan_max_depth.map(usize::from),
                entries: entries.clone(),
            },
            Self::Unrestricted => FileSystemSandboxPolicy::unrestricted(),
        }
    }
}

/// Reserved identifier for the built-in read-only permission profile.
pub const BUILT_IN_PERMISSION_PROFILE_READ_ONLY: &str = ":read-only";

/// Reserved identifier for the built-in workspace-write permission profile.
pub const BUILT_IN_PERMISSION_PROFILE_WORKSPACE: &str = ":workspace";

/// Reserved identifier for the built-in full-access permission profile.
pub const BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS: &str = ":danger-full-access";

/// Canonical active runtime permissions for a conversation, turn, or command.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum PermissionProfile {
    /// Codex owns sandbox construction for this profile.
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    Managed {
        file_system: ManagedFileSystemPermissions,
        network: NetworkSandboxPolicy,
    },
    /// Do not apply an outer sandbox.
    Disabled,
    /// Filesystem isolation is enforced by an external caller.
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    External { network: NetworkSandboxPolicy },
}

/// Metadata for the named or implicit built-in permissions profile that
/// produced the active `PermissionProfile`.
///
/// The runtime must honor `PermissionProfile`; this sidecar exists so clients
/// can display stable profile identity without trying to reverse-engineer a
/// name from the compiled permissions.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct ActivePermissionProfile {
    /// Profile identifier from `default_permissions` or the implicit built-in
    /// default, such as `:workspace` or a user-defined `[permissions.<id>]`
    /// profile.
    pub id: String,

    /// Optional parent profile identifier from the selected permissions
    /// profile's `extends` setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub extends: Option<String>,
}

impl ActivePermissionProfile {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            extends: None,
        }
    }

    pub fn read_only() -> Self {
        Self::new(BUILT_IN_PERMISSION_PROFILE_READ_ONLY)
    }
}

impl Default for PermissionProfile {
    fn default() -> Self {
        Self::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: Vec::new(),
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        }
    }
}

impl PermissionProfile {
    /// Managed read-only filesystem access with restricted network access.
    pub fn read_only() -> Self {
        let file_system = FileSystemSandboxPolicy::read_only();
        Self::Managed {
            file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
            network: NetworkSandboxPolicy::Restricted,
        }
    }

    /// Intersects managed filesystem permissions with read-only access and restricts network.
    ///
    /// Returns `None` when filesystem enforcement belongs to an external caller.
    pub fn intersect_with_read_only(&self) -> Option<Self> {
        let mut file_system = self.file_system_sandbox_policy();
        match file_system.kind {
            FileSystemSandboxKind::Restricted => {
                for entry in &mut file_system.entries {
                    entry.access = match entry.access {
                        FileSystemAccessMode::Read | FileSystemAccessMode::Write => {
                            FileSystemAccessMode::Read
                        }
                        FileSystemAccessMode::Deny => FileSystemAccessMode::Deny,
                    };
                }
            }
            FileSystemSandboxKind::Unrestricted => {
                file_system = FileSystemSandboxPolicy::read_only();
            }
            FileSystemSandboxKind::ExternalSandbox => return None,
        }
        Some(Self::from_runtime_permissions(
            &file_system,
            NetworkSandboxPolicy::Restricted,
        ))
    }

    /// Managed workspace-write filesystem access with restricted network
    /// access.
    ///
    /// The returned profile contains symbolic `:workspace_roots` entries that
    /// must be resolved against the active permission root before enforcement.
    pub fn workspace_write() -> Self {
        Self::workspace_write_with(
            &[],
            NetworkSandboxPolicy::Restricted,
            /*exclude_tmpdir_env_var*/ false,
            /*exclude_slash_tmp*/ false,
        )
    }

    /// Managed workspace-write filesystem access with the legacy
    /// `sandbox_workspace_write` knobs applied directly to the profile.
    ///
    /// The returned profile contains symbolic `:workspace_roots` entries that
    /// must be resolved against the active permission root before enforcement.
    pub fn workspace_write_with(
        writable_roots: &[AbsolutePathBuf],
        network: NetworkSandboxPolicy,
        exclude_tmpdir_env_var: bool,
        exclude_slash_tmp: bool,
    ) -> Self {
        let file_system = FileSystemSandboxPolicy::workspace_write(
            writable_roots,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
        );
        Self::Managed {
            file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
            network,
        }
    }

    /// Legacy workspace-write settings interpreted for executor-owned paths.
    pub fn workspace_write_with_path_uris(
        writable_roots: &[PathUri],
        network: NetworkSandboxPolicy,
        exclude_tmpdir_env_var: bool,
        exclude_slash_tmp: bool,
    ) -> Self {
        let file_system = FileSystemSandboxPolicy::workspace_write_with_path_uris(
            writable_roots,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
        );
        Self::Managed {
            file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
            network,
        }
    }

    pub fn materialize_project_roots_with_workspace_roots(
        self,
        workspace_roots: &[AbsolutePathBuf],
    ) -> Self {
        self.materialize_project_roots_with(|policy| {
            policy.materialize_project_roots_with_workspace_roots(workspace_roots)
        })
    }

    pub fn materialize_project_roots_with_path_uris(self, workspace_roots: &[PathUri]) -> Self {
        self.materialize_project_roots_with(|policy| {
            policy.materialize_project_roots_with_path_uris(workspace_roots)
        })
    }

    fn materialize_project_roots_with(
        self,
        materialize: impl FnOnce(FileSystemSandboxPolicy) -> FileSystemSandboxPolicy,
    ) -> Self {
        match self {
            Self::Managed {
                file_system,
                network,
            } => {
                let file_system = materialize(file_system.to_sandbox_policy());
                Self::Managed {
                    file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
                    network,
                }
            }
            Self::Disabled => Self::Disabled,
            Self::External { network } => Self::External { network },
        }
    }

    pub fn from_runtime_permissions(
        file_system_sandbox_policy: &FileSystemSandboxPolicy,
        network_sandbox_policy: NetworkSandboxPolicy,
    ) -> Self {
        let enforcement = match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::Restricted | FileSystemSandboxKind::Unrestricted => {
                SandboxEnforcement::Managed
            }
            FileSystemSandboxKind::ExternalSandbox => SandboxEnforcement::External,
        };
        Self::from_runtime_permissions_with_enforcement(
            enforcement,
            file_system_sandbox_policy,
            network_sandbox_policy,
        )
    }

    pub fn from_runtime_permissions_with_enforcement(
        enforcement: SandboxEnforcement,
        file_system_sandbox_policy: &FileSystemSandboxPolicy,
        network_sandbox_policy: NetworkSandboxPolicy,
    ) -> Self {
        match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::ExternalSandbox => Self::External {
                network: network_sandbox_policy,
            },
            FileSystemSandboxKind::Unrestricted if enforcement == SandboxEnforcement::Disabled => {
                Self::Disabled
            }
            FileSystemSandboxKind::Restricted | FileSystemSandboxKind::Unrestricted => {
                Self::Managed {
                    file_system: ManagedFileSystemPermissions::from_sandbox_policy(
                        file_system_sandbox_policy,
                    ),
                    network: network_sandbox_policy,
                }
            }
        }
    }

    pub fn from_legacy_sandbox_policy(sandbox_policy: &SandboxPolicy) -> Self {
        Self::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &FileSystemSandboxPolicy::from(sandbox_policy),
            NetworkSandboxPolicy::from(sandbox_policy),
        )
    }

    pub fn from_legacy_sandbox_policy_for_cwd(sandbox_policy: &SandboxPolicy, cwd: &Path) -> Self {
        Self::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(sandbox_policy, cwd),
            NetworkSandboxPolicy::from(sandbox_policy),
        )
    }

    pub fn enforcement(&self) -> SandboxEnforcement {
        match self {
            Self::Managed { .. } => SandboxEnforcement::Managed,
            Self::Disabled => SandboxEnforcement::Disabled,
            Self::External { .. } => SandboxEnforcement::External,
        }
    }

    pub fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        match self {
            Self::Managed { file_system, .. } => file_system.to_sandbox_policy(),
            Self::Disabled => FileSystemSandboxPolicy::unrestricted(),
            Self::External { .. } => FileSystemSandboxPolicy::external_sandbox(),
        }
    }

    pub fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        match self {
            Self::Managed { network, .. } | Self::External { network } => *network,
            Self::Disabled => NetworkSandboxPolicy::Enabled,
        }
    }

    pub fn to_legacy_sandbox_policy(&self, cwd: &Path) -> io::Result<SandboxPolicy> {
        match self {
            Self::Managed {
                file_system,
                network,
            } => file_system
                .to_sandbox_policy()
                .to_legacy_sandbox_policy(*network, cwd),
            Self::Disabled => Ok(SandboxPolicy::DangerFullAccess),
            Self::External { network } => Ok(SandboxPolicy::ExternalSandbox {
                network_access: if network.is_enabled() {
                    crate::protocol::NetworkAccess::Enabled
                } else {
                    crate::protocol::NetworkAccess::Restricted
                },
            }),
        }
    }

    pub fn to_runtime_permissions(&self) -> (FileSystemSandboxPolicy, NetworkSandboxPolicy) {
        (
            self.file_system_sandbox_policy(),
            self.network_sandbox_policy(),
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedPermissionProfile {
    #[serde(rename_all = "snake_case")]
    Managed {
        file_system: ManagedFileSystemPermissions,
        network: NetworkSandboxPolicy,
    },
    Disabled,
    #[serde(rename_all = "snake_case")]
    External {
        network: NetworkSandboxPolicy,
    },
}

impl From<TaggedPermissionProfile> for PermissionProfile {
    fn from(value: TaggedPermissionProfile) -> Self {
        match value {
            TaggedPermissionProfile::Managed {
                file_system,
                network,
            } => Self::Managed {
                file_system,
                network,
            },
            TaggedPermissionProfile::Disabled => Self::Disabled,
            TaggedPermissionProfile::External { network } => Self::External { network },
        }
    }
}

/// Pre-tagged shape written to rollout files before `PermissionProfile`
/// represented enforcement explicitly.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPermissionProfile {
    network: Option<NetworkPermissions>,
    file_system: Option<FileSystemPermissions>,
}

impl From<LegacyPermissionProfile> for PermissionProfile {
    fn from(value: LegacyPermissionProfile) -> Self {
        let file_system = value.file_system.map_or_else(
            || ManagedFileSystemPermissions::Restricted {
                entries: Vec::new(),
                glob_scan_max_depth: None,
            },
            |permissions| ManagedFileSystemPermissions::Restricted {
                entries: permissions.entries,
                glob_scan_max_depth: permissions.glob_scan_max_depth,
            },
        );
        let network_sandbox_policy = if value
            .network
            .as_ref()
            .and_then(|network| network.enabled)
            .unwrap_or(false)
        {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        };
        Self::Managed {
            file_system,
            network: network_sandbox_policy,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PermissionProfileDe {
    Tagged(TaggedPermissionProfile),
    Legacy(LegacyPermissionProfile),
}

impl<'de> Deserialize<'de> for PermissionProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match PermissionProfileDe::deserialize(deserializer)? {
            PermissionProfileDe::Tagged(tagged) => tagged.into(),
            PermissionProfileDe::Legacy(legacy) => legacy.into(),
        })
    }
}

impl From<NetworkSandboxPolicy> for NetworkPermissions {
    fn from(value: NetworkSandboxPolicy) -> Self {
        Self {
            enabled: Some(value.is_enabled()),
        }
    }
}

impl From<&FileSystemSandboxPolicy> for FileSystemPermissions {
    fn from(value: &FileSystemSandboxPolicy) -> Self {
        let entries = match value.kind {
            FileSystemSandboxKind::Restricted => value.entries.clone(),
            FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
                vec![FileSystemSandboxEntry::new(
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    FileSystemAccessMode::Write,
                )]
            }
        };
        Self {
            entries,
            glob_scan_max_depth: value.glob_scan_max_depth.and_then(NonZeroUsize::new),
        }
    }
}

impl From<&FileSystemPermissions> for FileSystemSandboxPolicy {
    fn from(value: &FileSystemPermissions) -> Self {
        let mut policy = FileSystemSandboxPolicy::restricted(value.entries.clone());
        policy.glob_scan_max_depth = value.glob_scan_max_depth.map(usize::from);
        policy
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        phase: Option<MessagePhase>,
    },
    FunctionCallOutput {
        call_id: String,
        #[ts(as = "FunctionCallOutputBody")]
        #[schemars(with = "FunctionCallOutputBody")]
        output: FunctionCallOutputPayload,
    },
    McpToolCallOutput {
        call_id: String,
        output: CallToolResult,
    },
    CustomToolCallOutput {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        name: Option<String>,
        #[ts(as = "FunctionCallOutputBody")]
        #[schemars(with = "FunctionCallOutputBody")]
        output: FunctionCallOutputPayload,
    },
    ToolSearchOutput {
        call_id: String,
        status: String,
        execution: String,
        #[ts(type = "unknown[]")]
        tools: Vec<serde_json::Value>,
    },
}

#[derive(derive_more::Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText {
        #[debug("{:?} <{} bytes>", &text[..text.floor_char_boundary(/*index*/ 512)], text.len())]
        text: String,
    },
    InputImage {
        #[serde(flatten)]
        image: ImageReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<ImageDetail>,
    },
    InputAudio {
        #[serde(flatten)]
        audio: AudioReference,
    },
    OutputText {
        text: String,
        // Preserve provider metadata, including empty arrays, for sampled-output replay.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        #[ts(type = "unknown[]")]
        #[debug(skip)]
        annotations: Option<Vec<serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        #[ts(type = "unknown[]")]
        #[debug(skip)]
        logprobs: Option<Vec<serde_json::Value>>,
    },
}

/// Constructs an input-text content item.
pub fn input_text(text: impl Into<String>) -> ContentItem {
    ContentItem::InputText { text: text.into() }
}

impl ContentItem {
    /// Extracts text, returning `None` for non-text content.
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::InputText { text } | Self::OutputText { text, .. } => Some(text),
            Self::InputImage { .. } | Self::InputAudio { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(untagged)]
#[ts(untagged)]
pub enum ImageReference {
    Inline { image_url: String },
    File { file_id: String },
}

/// Inline audio data or an uploaded file loaded by the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(untagged)]
#[ts(untagged)]
pub enum AudioReference {
    Inline { audio_url: String },
    File { file_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessageInputContent {
    InputText { text: String },
    EncryptedContent { encrypted_content: String },
}

/// Returns the locally readable text when an agent message is entirely plaintext.
pub fn plaintext_agent_message_content(content: &[AgentMessageInputContent]) -> Option<String> {
    let mut text_parts = Vec::with_capacity(content.len());
    for part in content {
        match part {
            AgentMessageInputContent::InputText { text } => text_parts.push(text.as_str()),
            AgentMessageInputContent::EncryptedContent { .. } => return None,
        }
    }

    let text = text_parts.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

pub const DEFAULT_IMAGE_DETAIL: ImageDetail = ImageDetail::High;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
/// Classifies assistant text as commentary, a partial answer, or a terminal answer.
///
/// Providers do not emit this consistently, so callers must treat `None` as
/// "phase unknown" and keep compatibility behavior for legacy models.
pub enum MessagePhase {
    /// Mid-turn assistant text (for example preamble/progress narration).
    ///
    /// Additional tool calls or assistant output may follow before turn
    /// completion.
    Commentary,
    /// Stable answer text that may be followed by more assistant output or tools.
    PartialAnswer,
    /// The assistant's declared terminal answer text for the current turn.
    FinalAnswer,
}

/// Internal Responses API passthrough metadata copied into underlying chat messages.
///
/// Responses API strongly types this payload. Do not modify it without first getting API
/// approval and making the corresponding Responses API change.
#[serde_as]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
pub struct InternalChatMessageMetadataPassthrough {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    /// Message creation time in fractional Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub create_time: Option<serde_json::Number>,
    /// Native content-token count for this sampled message, including its sampled footer.
    /// Read from the producer only; do not replay telemetry into subsequent model requests.
    #[serde_as(deserialize_as = "serde_with::DefaultOnError")]
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    #[ts(skip)]
    pub content_token_count: Option<u64>,
    /// Harness-owned classifications aligned with the item's content entries.
    #[serde_as(deserialize_as = "serde_with::DefaultOnError")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub content_item_kinds: Option<Vec<ContentItemKind>>,
    /// Positional source attribution for `content`; missing historical entries are Unknown.
    #[serde_as(deserialize_as = "serde_with::DefaultOnError")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub content_item_metadata: Option<Vec<ContentItemMetadata>>,
    // Ignore input values so requests and rollout reloads cannot fake tool call records.
    // A resumed thread can record new calls, but cannot prove old calls from this payload.
    /// Host-owned Code Mode cell shared by its `exec` and subsequent `wait` outputs.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub cell_id: Option<String>,
    /// Warehouse-only Responses metadata, not part of the public app-server protocol.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub executed_tool_calls: Option<Vec<ExecutedToolCall>>,
    /// Whether the host recorded the complete ordered call inventory without losing calls or names.
    /// Recorded arguments may be truncated independently of this claim.
    /// For a direct tool output this covers its single invocation; with `cell_id`, it covers
    /// the Code Mode cell across its outputs. Neither case describes tool success.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    #[ts(skip)]
    pub tool_calls_complete: Option<bool>,
}

impl InternalChatMessageMetadataPassthrough {
    pub(crate) fn set_turn_id_if_missing(metadata: &mut Option<Self>, turn_id: &str) {
        if turn_id.is_empty()
            || metadata
                .as_ref()
                .and_then(|metadata| metadata.turn_id.as_deref())
                .is_some_and(|turn_id| !turn_id.is_empty())
        {
            return;
        }
        metadata.get_or_insert_with(Self::default).turn_id = Some(turn_id.to_string());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    AdditionalTools {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        role: String,
        #[ts(type = "unknown[]")]
        // Share immutable declarations across history and request copies.
        tools: Arc<[serde_json::Value]>,
    },
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        role: String,
        content: Vec<ContentItem>,
        // Optional output-message phase (for example: "commentary", "final_answer").
        // Availability varies by provider/model, so downstream consumers must
        // preserve fallback behavior when this is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        phase: Option<MessagePhase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    AgentMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        author: String,
        recipient: String,
        content: Vec<AgentMessageInputContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        summary: Vec<ReasoningItemReasoningSummary>,
        #[serde(default, skip_serializing_if = "should_serialize_reasoning_content")]
        #[ts(optional)]
        content: Option<Vec<ReasoningItemContent>>,
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    LocalShellCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        /// Legacy id field retained for compatibility with older payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        /// Set when using the Responses API.
        call_id: Option<String>,
        status: LocalShellStatus,
        action: LocalShellAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        namespace: Option<String>,
        // The Responses API returns the function call arguments as a *string* that contains
        // JSON, not as an already‑parsed object. We keep it as a raw string here and let
        // Session::handle_function_call parse it into a Value.
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_function_args: Option<Vec<String>>,
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    ToolSearchCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        execution: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    // NOTE: The `output` field for `function_call_output` uses a dedicated payload type with
    // custom serialization. On the wire it is either:
    //   - a plain string (`content`)
    //   - an array of structured content items (`content_items`)
    // We keep this behavior centralized in `FunctionCallOutputPayload`.
    FunctionCallOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        namespace: Option<String>,
        #[ts(as = "FunctionCallOutputBody")]
        #[schemars(with = "FunctionCallOutputBody")]
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    CustomToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,

        call_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        namespace: Option<String>,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    // `custom_tool_call_output.output` uses the same wire encoding as
    // `function_call_output.output` so freeform tools can return either plain
    // text or structured content items.
    CustomToolCallOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        name: Option<String>,
        #[ts(as = "FunctionCallOutputBody")]
        #[schemars(with = "FunctionCallOutputBody")]
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    ToolSearchOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        call_id: Option<String>,
        status: String,
        execution: String,
        #[ts(type = "unknown[]")]
        tools: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    // Emitted by the Responses API when the agent triggers a web search.
    // Example payload (from SSE `response.output_item.done`):
    // {
    //   "id":"ws_...",
    //   "type":"web_search_call",
    //   "status":"completed",
    //   "action": {"type":"search","query":"weather: San Francisco, CA"}
    // }
    WebSearchCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        action: Option<WebSearchAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    // Emitted by the Responses API when the agent triggers image generation.
    // Example payload:
    // {
    //   "id":"ig_123",
    //   "type":"image_generation_call",
    //   "status":"completed",
    //   "revised_prompt":"A gray tabby cat hugging an otter...",
    //   "result":"..."
    // }
    ImageGenerationCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        revised_prompt: Option<String>,
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    #[serde(alias = "compaction_summary")]
    Compaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        encrypted_content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    /// A durable input control interpreted by the backend at its position in history.
    ConfigurationUpdate {
        reasoning: ConfigurationReasoning,
    },
    // Compaction triggers are request controls, not durable response items.
    CompactionTrigger {},
    ContextCompaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    },
    #[serde(other)]
    Other,
}

impl ResponseItem {
    /// Clears the top-level `encrypted_content` fields.
    pub fn clear_encrypted_content(&mut self) {
        match self {
            Self::Message {
                encrypted_content, ..
            }
            | Self::Reasoning {
                encrypted_content, ..
            }
            | Self::LocalShellCall {
                encrypted_content, ..
            }
            | Self::FunctionCall {
                encrypted_content, ..
            }
            | Self::ToolSearchCall {
                encrypted_content, ..
            }
            | Self::CustomToolCall {
                encrypted_content, ..
            }
            | Self::WebSearchCall {
                encrypted_content, ..
            }
            | Self::ImageGenerationCall {
                encrypted_content, ..
            }
            | Self::ContextCompaction {
                encrypted_content, ..
            } => *encrypted_content = None,
            Self::Compaction {
                encrypted_content, ..
            } => encrypted_content.clear(),
            Self::AdditionalTools { .. }
            | Self::AgentMessage { .. }
            | Self::FunctionCallOutput { .. }
            | Self::CustomToolCallOutput { .. }
            | Self::ToolSearchOutput { .. }
            | Self::ConfigurationUpdate { .. }
            | Self::CompactionTrigger { .. }
            | Self::Other => {}
        }
    }

    /// Returns the wire type name for this response item.
    pub fn item_type(&self) -> &'static str {
        match self {
            Self::AdditionalTools { .. } => "additional_tools",
            Self::Message { .. } => "message",
            Self::AgentMessage { .. } => "agent_message",
            Self::Reasoning { .. } => "reasoning",
            Self::LocalShellCall { .. } => "local_shell_call",
            Self::FunctionCall { .. } => "function_call",
            Self::ToolSearchCall { .. } => "tool_search_call",
            Self::FunctionCallOutput { .. } => "function_call_output",
            Self::CustomToolCall { .. } => "custom_tool_call",
            Self::CustomToolCallOutput { .. } => "custom_tool_call_output",
            Self::ToolSearchOutput { .. } => "tool_search_output",
            Self::WebSearchCall { .. } => "web_search_call",
            Self::ImageGenerationCall { .. } => "image_generation_call",
            Self::Compaction { .. } => "compaction",
            Self::ConfigurationUpdate { .. } => "configuration_update",
            Self::CompactionTrigger { .. } => "compaction_trigger",
            Self::ContextCompaction { .. } => "context_compaction",
            Self::Other => "other",
        }
    }

    /// Returns whether this item is an ordinary user-role message.
    pub fn is_user_message(&self) -> bool {
        matches!(self, Self::Message { role, .. } if role == "user")
    }

    /// Returns the Responses API item ID, if present.
    pub fn id(&self) -> Option<&ResponseItemId> {
        match self {
            Self::AdditionalTools { id, .. }
            | Self::Message { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::LocalShellCall { id, .. }
            | Self::FunctionCall { id, .. }
            | Self::ToolSearchCall { id, .. }
            | Self::FunctionCallOutput { id, .. }
            | Self::CustomToolCall { id, .. }
            | Self::CustomToolCallOutput { id, .. }
            | Self::ToolSearchOutput { id, .. }
            | Self::WebSearchCall { id, .. }
            | Self::Reasoning { id, .. }
            | Self::ImageGenerationCall { id, .. }
            | Self::Compaction { id, .. }
            | Self::ContextCompaction { id, .. } => id.as_ref(),
            Self::ConfigurationUpdate { .. } | Self::CompactionTrigger { .. } | Self::Other => None,
        }
    }

    /// Sets or clears the Responses API item ID for variants that carry one.
    pub fn set_id(&mut self, new_id: Option<ResponseItemId>) {
        match self {
            Self::AdditionalTools { id, .. }
            | Self::Message { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::LocalShellCall { id, .. }
            | Self::FunctionCall { id, .. }
            | Self::ToolSearchCall { id, .. }
            | Self::FunctionCallOutput { id, .. }
            | Self::CustomToolCall { id, .. }
            | Self::CustomToolCallOutput { id, .. }
            | Self::ToolSearchOutput { id, .. }
            | Self::WebSearchCall { id, .. }
            | Self::Reasoning { id, .. }
            | Self::ImageGenerationCall { id, .. }
            | Self::Compaction { id, .. }
            | Self::ContextCompaction { id, .. } => *id = new_id,
            Self::ConfigurationUpdate { .. } | Self::CompactionTrigger { .. } | Self::Other => {}
        }
    }

    /// Returns the Responses API item ID prefix for variants that carry an ID.
    pub fn id_prefix(&self) -> Option<&'static str> {
        match self {
            Self::AdditionalTools { .. } => Some("at"),
            Self::Message { .. } => Some("msg"),
            Self::AgentMessage { .. } => Some("amsg"),
            Self::Reasoning { .. } => Some("rs"),
            Self::LocalShellCall { .. } => Some("lsh"),
            Self::FunctionCall { .. } => Some("fc"),
            Self::ToolSearchCall { .. } => Some("tsc"),
            Self::FunctionCallOutput { .. } => Some("fco"),
            Self::CustomToolCall { .. } => Some("ctc"),
            Self::CustomToolCallOutput { .. } => Some("ctco"),
            Self::ToolSearchOutput { .. } => Some("tso"),
            Self::WebSearchCall { .. } => Some("ws"),
            Self::ImageGenerationCall { .. } => Some("ig"),
            Self::Compaction { .. } | Self::ContextCompaction { .. } => Some("cmp"),
            Self::ConfigurationUpdate { .. } | Self::CompactionTrigger { .. } | Self::Other => None,
        }
    }

    /// Returns the non-empty turn ID stamped onto this item, if present.
    pub fn turn_id(&self) -> Option<&str> {
        self.internal_chat_message_metadata_passthrough()
            .and_then(|metadata| metadata.turn_id.as_deref())
            .filter(|turn_id| !turn_id.is_empty())
    }

    /// Stamps the item with `turn_id` unless it already has a non-empty turn ID.
    pub fn set_turn_id_if_missing(&mut self, turn_id: &str) {
        let Some(metadata) = self.internal_chat_message_metadata_passthrough_mut() else {
            return;
        };
        InternalChatMessageMetadataPassthrough::set_turn_id_if_missing(metadata, turn_id);
    }

    /// Stamps a harness-authored durable item without replacing its creation time.
    pub fn set_create_time_if_missing(&mut self, create_time: serde_json::Number) {
        let metadata = match self {
            Self::Message {
                role,
                internal_chat_message_metadata_passthrough: metadata,
                ..
            } if matches!(role.as_str(), "user" | "developer") => metadata,
            Self::AgentMessage {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::FunctionCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::CustomToolCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ToolSearchOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            } => metadata,
            _ => return,
        };

        metadata
            .get_or_insert_default()
            .create_time
            .get_or_insert(create_time);
    }

    /// Removes internal chat message metadata passthrough before sending to a provider that does
    /// not accept it.
    pub fn clear_internal_chat_message_metadata_passthrough(&mut self) {
        if let Some(metadata) = self.internal_chat_message_metadata_passthrough_mut() {
            *metadata = None;
        }
    }

    /// Removes content item classifications and attribution, preserving other metadata.
    pub fn clear_content_item_kinds(&mut self) {
        let Some(metadata) = self.internal_chat_message_metadata_passthrough_mut() else {
            return;
        };
        let Some(metadata_value) = metadata else {
            return;
        };

        metadata_value.content_item_kinds = None;
        metadata_value.content_item_metadata = None;
        if metadata_value == &InternalChatMessageMetadataPassthrough::default() {
            *metadata = None;
        }
    }

    /// Removes per-content attribution for endpoints that do not yet accept it.
    pub fn clear_content_item_metadata(&mut self) {
        if let Some(metadata) = self.internal_chat_message_metadata_passthrough_mut()
            && let Some(value) = metadata
        {
            value.content_item_metadata = None;
            if value == &InternalChatMessageMetadataPassthrough::default() {
                *metadata = None;
            }
        }
    }

    fn internal_chat_message_metadata_passthrough(
        &self,
    ) -> Option<&InternalChatMessageMetadataPassthrough> {
        match self {
            Self::Message {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::AgentMessage {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::Reasoning {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::LocalShellCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::FunctionCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ToolSearchCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::FunctionCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::CustomToolCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::CustomToolCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ToolSearchOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::WebSearchCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ImageGenerationCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::Compaction {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ContextCompaction {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            } => metadata.as_ref(),
            Self::ConfigurationUpdate { .. }
            | Self::CompactionTrigger { .. }
            | Self::AdditionalTools { .. }
            | Self::Other => None,
        }
    }

    fn internal_chat_message_metadata_passthrough_mut(
        &mut self,
    ) -> Option<&mut Option<InternalChatMessageMetadataPassthrough>> {
        match self {
            Self::Message {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::AgentMessage {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::Reasoning {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::LocalShellCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::FunctionCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ToolSearchCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::FunctionCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::CustomToolCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::CustomToolCallOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ToolSearchOutput {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::WebSearchCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ImageGenerationCall {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::Compaction {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            }
            | Self::ContextCompaction {
                internal_chat_message_metadata_passthrough: metadata,
                ..
            } => Some(metadata),
            Self::ConfigurationUpdate { .. }
            | Self::CompactionTrigger { .. }
            | Self::AdditionalTools { .. }
            | Self::Other => None,
        }
    }
}

pub const BASE_INSTRUCTIONS_DEFAULT: &str = include_str!("prompts/base_instructions/default.md");

/// Describes whether persisted base instructions were supplied by the user or generated for a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum BaseInstructionsProvenance {
    /// The instructions were explicitly configured and must survive model changes unchanged.
    Custom,
    /// The instructions were generated from this model's instruction template.
    Model { model: String },
}

/// Base instructions for the model in a thread. Corresponds to the `instructions` field in the ResponsesAPI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename = "base_instructions", rename_all = "snake_case")]
pub struct BaseInstructions {
    pub text: String,
    /// Missing on rollouts written before base-instruction provenance was persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provenance: Option<BaseInstructionsProvenance>,
}

impl Default for BaseInstructions {
    fn default() -> Self {
        Self {
            text: BASE_INSTRUCTIONS_DEFAULT.to_string(),
            provenance: None,
        }
    }
}

const MAX_RENDERED_PREFIXES: usize = 100;
const MAX_ALLOW_PREFIX_TEXT_BYTES: usize = 5000;
const TRUNCATED_MARKER: &str = "...\n[Some commands were truncated]";

pub fn format_allow_prefixes(prefixes: Vec<Vec<String>>) -> Option<String> {
    let mut truncated = false;
    if prefixes.len() > MAX_RENDERED_PREFIXES {
        truncated = true;
    }

    let mut prefixes = prefixes;
    prefixes.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| prefix_combined_str_len(a).cmp(&prefix_combined_str_len(b)))
            .then_with(|| a.cmp(b))
    });

    let full_text = prefixes
        .into_iter()
        .take(MAX_RENDERED_PREFIXES)
        .map(|prefix| format!("- {}", render_command_prefix(&prefix)))
        .collect::<Vec<_>>()
        .join("\n");

    // truncate to last UTF8 char
    let mut output = full_text;
    let byte_idx = output
        .char_indices()
        .nth(MAX_ALLOW_PREFIX_TEXT_BYTES)
        .map(|(i, _)| i);
    if let Some(byte_idx) = byte_idx {
        truncated = true;
        output = output[..byte_idx].to_string();
    }

    if truncated {
        Some(format!("{output}{TRUNCATED_MARKER}"))
    } else {
        Some(output)
    }
}

fn prefix_combined_str_len(prefix: &[String]) -> usize {
    prefix.iter().map(String::len).sum()
}

fn render_command_prefix(prefix: &[String]) -> String {
    let tokens = prefix
        .iter()
        .map(|token| serde_json::to_string(token).unwrap_or_else(|_| format!("{token:?}")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{tokens}]")
}

fn should_serialize_reasoning_content(content: &Option<Vec<ReasoningItemContent>>) -> bool {
    match content {
        Some(content) => !content
            .iter()
            .any(|c| matches!(c, ReasoningItemContent::ReasoningText { .. })),
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMediaKind {
    Audio,
    Image,
}

impl LocalMediaKind {
    fn name(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Image => "image",
        }
    }
}

fn local_media_error_placeholder(
    path: &std::path::Path,
    error: impl std::fmt::Display,
    media_kind: LocalMediaKind,
) -> ContentItem {
    let media_name = media_kind.name();
    let path = path.display();
    ContentItem::InputText {
        text: format!("Codex could not read the local {media_name} at `{path}`: {error}"),
    }
}

pub const VIEW_IMAGE_TOOL_NAME: &str = "view_image";

const IMAGE_OPEN_TAG: &str = "<image>";
const IMAGE_CLOSE_TAG: &str = "</image>";
const LOCAL_IMAGE_OPEN_TAG_PREFIX: &str = "<image name=";
const LOCAL_IMAGE_OPEN_TAG_SUFFIX: &str = ">";
const LOCAL_IMAGE_CLOSE_TAG: &str = IMAGE_CLOSE_TAG;
const AUDIO_OPEN_TAG: &str = "<audio>";
const AUDIO_CLOSE_TAG: &str = "</audio>";
const LOCAL_AUDIO_OPEN_TAG_PREFIX: &str = "<audio name=";
const LOCAL_AUDIO_OPEN_TAG_SUFFIX: &str = ">";
const LOCAL_AUDIO_CLOSE_TAG: &str = AUDIO_CLOSE_TAG;

pub fn image_open_tag_text() -> String {
    IMAGE_OPEN_TAG.to_string()
}

pub fn image_close_tag_text() -> String {
    IMAGE_CLOSE_TAG.to_string()
}

pub fn local_image_label_text(label_number: usize) -> String {
    format!("[Image #{label_number}]")
}

pub fn local_image_open_tag_text_with_path(label_number: usize, path: &std::path::Path) -> String {
    let label = local_image_label_text(label_number);
    let path = path.display();
    format!("{LOCAL_IMAGE_OPEN_TAG_PREFIX}{label} path=\"{path}\"{LOCAL_IMAGE_OPEN_TAG_SUFFIX}")
}

pub fn is_local_image_open_tag_text(text: &str) -> bool {
    text.strip_prefix(LOCAL_IMAGE_OPEN_TAG_PREFIX)
        .is_some_and(|rest| rest.ends_with(LOCAL_IMAGE_OPEN_TAG_SUFFIX))
}

pub fn is_local_image_close_tag_text(text: &str) -> bool {
    is_image_close_tag_text(text)
}

pub fn is_image_open_tag_text(text: &str) -> bool {
    text == IMAGE_OPEN_TAG
}

pub fn is_image_close_tag_text(text: &str) -> bool {
    text == IMAGE_CLOSE_TAG
}

pub fn audio_open_tag_text() -> String {
    AUDIO_OPEN_TAG.to_string()
}

pub fn audio_close_tag_text() -> String {
    AUDIO_CLOSE_TAG.to_string()
}

pub fn local_audio_label_text(label_number: usize) -> String {
    format!("[Audio #{label_number}]")
}

pub fn local_audio_open_tag_text_with_path(label_number: usize, path: &std::path::Path) -> String {
    let label = local_audio_label_text(label_number);
    let path = path.display();
    format!("{LOCAL_AUDIO_OPEN_TAG_PREFIX}{label} path=\"{path}\"{LOCAL_AUDIO_OPEN_TAG_SUFFIX}")
}

pub fn is_local_audio_open_tag_text(text: &str) -> bool {
    text.strip_prefix(LOCAL_AUDIO_OPEN_TAG_PREFIX)
        .is_some_and(|rest| rest.ends_with(LOCAL_AUDIO_OPEN_TAG_SUFFIX))
}

pub fn is_local_audio_close_tag_text(text: &str) -> bool {
    is_audio_close_tag_text(text)
}

pub fn is_audio_open_tag_text(text: &str) -> bool {
    text == AUDIO_OPEN_TAG
}

pub fn is_audio_close_tag_text(text: &str) -> bool {
    text == AUDIO_CLOSE_TAG
}

/// Whether this text opens a harness label for the adjacent media part.
pub fn is_opening_media_label(text: &str, media: &ContentItem) -> bool {
    match media {
        ContentItem::InputImage { .. } => {
            is_local_image_open_tag_text(text) || is_image_open_tag_text(text)
        }
        ContentItem::InputAudio { .. } => {
            is_local_audio_open_tag_text(text) || is_audio_open_tag_text(text)
        }
        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => false,
    }
}

/// Whether this text closes a harness label for the adjacent media part.
pub fn is_closing_media_label(text: &str, media: &ContentItem) -> bool {
    match media {
        ContentItem::InputImage { .. } => is_image_close_tag_text(text),
        ContentItem::InputAudio { .. } => is_audio_close_tag_text(text),
        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => false,
    }
}

fn invalid_image_error_placeholder(
    path: &std::path::Path,
    error: impl std::fmt::Display,
) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Image located at `{}` is invalid: {}",
            path.display(),
            error
        ),
    }
}

fn unsupported_image_error_placeholder(path: &std::path::Path, mime: &str) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Codex cannot attach image at `{}`: unsupported image `{}`.",
            path.display(),
            mime
        ),
    }
}

pub fn local_image_content_items_with_label_number(
    path: &std::path::Path,
    file_bytes: Vec<u8>,
    label_number: Option<usize>,
    detail: ImageDetail,
) -> Vec<ContentItem> {
    let mode = match detail {
        ImageDetail::Original => PromptImageMode::Original,
        ImageDetail::Auto | ImageDetail::Low | ImageDetail::High => PromptImageMode::ResizeToFit,
    };

    match load_for_prompt_bytes(path, file_bytes, mode) {
        Ok(image) => local_image_content_items(path, image.into_data_url(), label_number, detail),
        Err(err) => match &err {
            ImageProcessingError::Read { .. }
            | ImageProcessingError::Encode { .. }
            | ImageProcessingError::InvalidDataUrl { .. }
            | ImageProcessingError::ImageTooLarge { .. } => {
                vec![local_media_error_placeholder(
                    path,
                    &err,
                    LocalMediaKind::Image,
                )]
            }
            ImageProcessingError::Decode { .. } if err.is_invalid_image() => {
                vec![invalid_image_error_placeholder(path, &err)]
            }
            ImageProcessingError::Decode { .. } => {
                vec![local_media_error_placeholder(
                    path,
                    &err,
                    LocalMediaKind::Image,
                )]
            }
            ImageProcessingError::UnsupportedImageFormat { mime } => {
                vec![unsupported_image_error_placeholder(path, mime)]
            }
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalImagePreparation {
    Process,
    Defer,
}

fn unsupported_audio_error_placeholder(path: &std::path::Path) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Codex cannot attach audio at `{}`: unsupported audio format; use wav, mp3, m4a, webm, or ogg.",
            path.display()
        ),
    }
}

fn local_audio_content_items(
    path: &std::path::Path,
    file_bytes: &[u8],
    label_number: usize,
) -> Vec<ContentItem> {
    let Some(mime) = audio_mime_for_path(path) else {
        return vec![unsupported_audio_error_placeholder(path)];
    };

    vec![
        ContentItem::InputText {
            text: local_audio_open_tag_text_with_path(label_number, path),
        },
        ContentItem::InputAudio {
            audio: AudioReference::Inline {
                audio_url: data_url_from_bytes(mime, file_bytes),
            },
        },
        ContentItem::InputText {
            text: LOCAL_AUDIO_CLOSE_TAG.to_string(),
        },
    ]
}

fn local_image_content_items(
    path: &std::path::Path,
    image_url: String,
    label_number: Option<usize>,
    detail: ImageDetail,
) -> Vec<ContentItem> {
    let mut items = Vec::with_capacity(3);
    if let Some(label_number) = label_number {
        items.push(ContentItem::InputText {
            text: local_image_open_tag_text_with_path(label_number, path),
        });
    }
    items.push(ContentItem::InputImage {
        image: ImageReference::Inline { image_url },
        detail: Some(detail),
    });
    if label_number.is_some() {
        items.push(ContentItem::InputText {
            text: LOCAL_IMAGE_CLOSE_TAG.to_string(),
        });
    }
    items
}

impl From<ResponseInputItem> for ResponseItem {
    fn from(item: ResponseInputItem) -> Self {
        match item {
            ResponseInputItem::Message {
                role,
                content,
                phase,
            } => Self::Message {
                status: None,
                encrypted_content: None,
                role,
                content,
                id: None,
                phase,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseInputItem::FunctionCallOutput { call_id, output } => Self::FunctionCallOutput {
                id: None,
                call_id: Some(call_id),
                name: None,
                namespace: None,
                output,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseInputItem::McpToolCallOutput { call_id, output } => {
                let output = output.into_function_call_output_payload();
                Self::FunctionCallOutput {
                    id: None,
                    call_id: Some(call_id),
                    name: None,
                    namespace: None,
                    output,
                    internal_chat_message_metadata_passthrough: None,
                }
            }
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                name,
                output,
            } => Self::CustomToolCallOutput {
                id: None,
                call_id,
                name,
                output,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseInputItem::ToolSearchOutput {
                call_id,
                status,
                execution,
                tools,
            } => Self::ToolSearchOutput {
                call_id: Some(call_id),
                status,
                execution,
                tools,
                id: None,
                internal_chat_message_metadata_passthrough: None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum LocalShellStatus {
    Completed,
    InProgress,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalShellAction {
    Exec(LocalShellExecAction),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct LocalShellExecAction {
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub working_directory: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(rename = "ResponsesApiWebSearchAction")]
pub enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        queries: Option<Vec<String>>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
    },
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pattern: Option<String>,
    },

    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemReasoningSummary {
    SummaryText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemContent {
    ReasoningText { text: String },
    Text { text: String },
}

impl From<Vec<UserInput>> for ResponseInputItem {
    fn from(items: Vec<UserInput>) -> Self {
        Self::from_user_input(items, LocalImagePreparation::Process, &mut HashMap::new())
    }
}

impl ResponseInputItem {
    /// Records original user-input indices to image slots in the generated message content.
    /// Inputs that do not produce an image, including failed local reads, have no entry.
    pub fn from_user_input(
        items: Vec<UserInput>,
        local_image_preparation: LocalImagePreparation,
        user_image_content_indices: &mut HashMap<usize, usize>,
    ) -> Self {
        user_image_content_indices.clear();
        let mut image_index = 0;
        let mut audio_index = 0;
        let mut content = Vec::new();
        for (input_index, input) in items.into_iter().enumerate() {
            let generated = match input {
                UserInput::Text { text, .. } => vec![ContentItem::InputText { text }],
                UserInput::Image { image, detail, .. } => {
                    image_index += 1;
                    let detail = detail.unwrap_or(DEFAULT_IMAGE_DETAIL);
                    vec![ContentItem::InputImage {
                        image,
                        detail: Some(detail),
                    }]
                }
                UserInput::LocalImage { path, detail, .. } => {
                    image_index += 1;
                    let detail = detail.unwrap_or(DEFAULT_IMAGE_DETAIL);
                    match std::fs::read(&path) {
                        Ok(file_bytes) => match local_image_preparation {
                            LocalImagePreparation::Process => {
                                local_image_content_items_with_label_number(
                                    &path,
                                    file_bytes,
                                    Some(image_index),
                                    detail,
                                )
                            }
                            LocalImagePreparation::Defer => local_image_content_items(
                                &path,
                                data_url_from_bytes("application/octet-stream", &file_bytes),
                                Some(image_index),
                                detail,
                            ),
                        },
                        Err(err) => vec![local_media_error_placeholder(
                            &path,
                            err,
                            LocalMediaKind::Image,
                        )],
                    }
                }
                UserInput::Audio { audio } => {
                    audio_index += 1;
                    vec![ContentItem::InputAudio { audio }]
                }
                UserInput::LocalAudio { path } => {
                    audio_index += 1;
                    match std::fs::read(&path) {
                        Ok(file_bytes) => {
                            local_audio_content_items(&path, &file_bytes, audio_index)
                        }
                        Err(err) => vec![local_media_error_placeholder(
                            &path,
                            err,
                            LocalMediaKind::Audio,
                        )],
                    }
                }
                UserInput::Skill { .. } | UserInput::Mention { .. } => Vec::new(), // Tool bodies are injected later in core
            };
            // Local images add framing text, while mentions and skills add no content.
            // Capture the image's actual slot before preparation replaces its reference.
            for (offset, item) in generated.iter().enumerate() {
                if matches!(item, ContentItem::InputImage { .. }) {
                    user_image_content_indices.insert(input_index, content.len() + offset);
                }
            }
            content.extend(generated);
        }
        Self::Message {
            role: "user".to_string(),
            content,
            phase: None,
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct SearchToolCallParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<usize>,
}

/// Responses API compatible content items that can be returned by a tool call.
/// This is a subset of ContentItem with the types we support as function call outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    // Do not rename, these are serialized and used directly in the responses API.
    InputText {
        text: String,
    },
    // Do not rename, these are serialized and used directly in the responses API.
    InputImage {
        #[serde(flatten)]
        image: ImageReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<ImageDetail>,
    },
    // Do not rename, these are serialized and used directly in the responses API.
    InputAudio {
        audio_url: String,
    },
    EncryptedContent {
        encrypted_content: String,
    },
}

/// Converts message content to tool output, preserving media and image detail.
/// Output text becomes input text without its sampling annotations or logprobs.
impl From<ContentItem> for FunctionCallOutputContentItem {
    fn from(item: ContentItem) -> Self {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                Self::InputText { text }
            }
            ContentItem::InputImage { image, detail } => Self::InputImage { image, detail },
            ContentItem::InputAudio {
                audio: AudioReference::Inline { audio_url },
            } => Self::InputAudio { audio_url },
            ContentItem::InputAudio {
                audio: AudioReference::File { file_id, .. },
            } => Self::InputText { text: file_id },
        }
    }
}

/// Converts structured function-call output content into plain text for
/// human-readable surfaces.
///
/// This conversion is intentionally lossy:
/// - only `input_text` items are included
/// - image and audio items are ignored
///
/// We use this helper where callers still need a string representation (for
/// example telemetry previews or legacy string-only output paths) while keeping
/// the original multimodal `content_items` as the authoritative payload sent to
/// the model.
pub fn function_call_output_content_items_to_text(
    content_items: &[FunctionCallOutputContentItem],
) -> Option<String> {
    let text_segments = content_items
        .iter()
        .filter_map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } if !text.trim().is_empty() => {
                Some(text.as_str())
            }
            FunctionCallOutputContentItem::InputText { .. }
            | FunctionCallOutputContentItem::InputImage { .. }
            | FunctionCallOutputContentItem::InputAudio { .. }
            | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
        })
        .collect::<Vec<_>>();

    if text_segments.is_empty() {
        None
    } else {
        Some(text_segments.join("\n"))
    }
}

impl From<crate::dynamic_tools::DynamicToolCallOutputContentItem>
    for FunctionCallOutputContentItem
{
    fn from(item: crate::dynamic_tools::DynamicToolCallOutputContentItem) -> Self {
        match item {
            crate::dynamic_tools::DynamicToolCallOutputContentItem::InputText { text } => {
                Self::InputText { text }
            }
            crate::dynamic_tools::DynamicToolCallOutputContentItem::InputImage { image_url } => {
                Self::InputImage {
                    image: ImageReference::Inline { image_url },
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                }
            }
            crate::dynamic_tools::DynamicToolCallOutputContentItem::InputAudio { audio_url } => {
                Self::InputAudio { audio_url }
            }
        }
    }
}

/// The payload we send back to OpenAI when reporting a tool call result.
///
/// `body` serializes directly as the wire value for `function_call_output.output`.
/// `success` remains internal metadata for downstream handling.
#[derive(Debug, Default, Clone, PartialEq, JsonSchema, TS)]
pub struct FunctionCallOutputPayload {
    pub body: FunctionCallOutputBody,
    pub success: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(untagged)]
pub enum FunctionCallOutputBody {
    Text(String),
    ContentItems(Vec<FunctionCallOutputContentItem>),
}

impl FunctionCallOutputBody {
    /// Best-effort conversion of a function-call output body to plain text for
    /// human-readable surfaces.
    ///
    /// This conversion is intentionally lossy when the body contains content
    /// items: image entries are dropped and text entries are joined with
    /// newlines.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Self::Text(content) => Some(content.clone()),
            Self::ContentItems(items) => function_call_output_content_items_to_text(items),
        }
    }
}

impl Default for FunctionCallOutputBody {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl FunctionCallOutputPayload {
    pub fn from_text(content: String) -> Self {
        Self {
            body: FunctionCallOutputBody::Text(content),
            success: None,
        }
    }

    pub fn from_content_items(content_items: Vec<FunctionCallOutputContentItem>) -> Self {
        Self {
            body: FunctionCallOutputBody::ContentItems(content_items),
            success: None,
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.body {
            FunctionCallOutputBody::Text(content) => Some(content),
            FunctionCallOutputBody::ContentItems(_) => None,
        }
    }

    pub fn content_items(&self) -> Option<&[FunctionCallOutputContentItem]> {
        match &self.body {
            FunctionCallOutputBody::Text(_) => None,
            FunctionCallOutputBody::ContentItems(items) => Some(items),
        }
    }

    pub fn content_items_mut(&mut self) -> Option<&mut Vec<FunctionCallOutputContentItem>> {
        match &mut self.body {
            FunctionCallOutputBody::Text(_) => None,
            FunctionCallOutputBody::ContentItems(items) => Some(items),
        }
    }
}

// `function_call_output.output` is encoded as either:
//   - an array of structured content items
//   - a plain string
impl Serialize for FunctionCallOutputPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.body {
            FunctionCallOutputBody::Text(content) => serializer.serialize_str(content),
            FunctionCallOutputBody::ContentItems(items) => items.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FunctionCallOutputPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let body = FunctionCallOutputBody::deserialize(deserializer)?;
        Ok(FunctionCallOutputPayload {
            body,
            success: None,
        })
    }
}

impl CallToolResult {
    pub fn from_result(result: Result<Self, String>) -> Self {
        match result {
            Ok(result) => result,
            Err(error) => Self::from_error_text(error),
        }
    }

    pub fn from_error_text(text: String) -> Self {
        Self {
            content: vec![serde_json::json!({
                "type": "text",
                "text": text,
            })],
            structured_content: None,
            is_error: Some(true),
            meta: None,
        }
    }

    pub fn success(&self) -> bool {
        self.is_error != Some(true)
    }

    pub fn as_function_call_output_payload(&self) -> FunctionCallOutputPayload {
        let content_items = convert_mcp_content_to_items(&self.content);
        if content_items
            .iter()
            .any(|item| matches!(item, FunctionCallOutputContentItem::EncryptedContent { .. }))
        {
            return FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(content_items),
                success: Some(self.success()),
            };
        }

        if let Some(structured_content) = &self.structured_content
            && !structured_content.is_null()
        {
            match serde_json::to_string(structured_content) {
                Ok(serialized_structured_content) => {
                    return FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text(serialized_structured_content),
                        success: Some(self.success()),
                    };
                }
                Err(err) => {
                    return FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text(err.to_string()),
                        success: Some(false),
                    };
                }
            }
        }

        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(content_items),
            success: Some(self.success()),
        }
    }

    pub fn into_function_call_output_payload(self) -> FunctionCallOutputPayload {
        self.as_function_call_output_payload()
    }
}

fn convert_mcp_content_to_items(
    contents: &[serde_json::Value],
) -> Vec<FunctionCallOutputContentItem> {
    const CODEX_ENCRYPTED_CONTENT_META_KEY: &str = "codex/encryptedContent";
    const CODEX_IMAGE_DETAIL_META_KEY: &str = "codex/imageDetail";

    #[derive(serde::Deserialize)]
    #[serde(tag = "type")]
    enum McpContent {
        #[serde(rename = "text")]
        Text {
            text: String,
            #[serde(rename = "_meta", default)]
            meta: Option<serde_json::Value>,
        },
        #[serde(rename = "image")]
        Image {
            data: String,
            #[serde(rename = "mimeType", alias = "mime_type")]
            mime_type: Option<String>,
            #[serde(rename = "_meta", default)]
            meta: Option<serde_json::Value>,
        },
        #[serde(rename = "audio")]
        Audio {
            data: String,
            #[serde(rename = "mimeType", alias = "mime_type")]
            mime_type: Option<String>,
            #[serde(rename = "_meta", default)]
            _meta: Option<serde_json::Value>,
        },
        #[serde(other)]
        Unknown,
    }

    let mut items = Vec::with_capacity(contents.len());

    for content in contents {
        let item = match serde_json::from_value::<McpContent>(content.clone()) {
            Ok(McpContent::Text { text, meta }) => {
                if meta
                    .as_ref()
                    .and_then(|meta| meta.get(CODEX_ENCRYPTED_CONTENT_META_KEY))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    FunctionCallOutputContentItem::EncryptedContent {
                        encrypted_content: text,
                    }
                } else {
                    FunctionCallOutputContentItem::InputText { text }
                }
            }
            Ok(McpContent::Image {
                data,
                mime_type,
                meta,
            }) => {
                let image_url = if data.starts_with("data:") {
                    data
                } else {
                    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".into());
                    format!("data:{mime_type};base64,{data}")
                };
                FunctionCallOutputContentItem::InputImage {
                    image: ImageReference::Inline { image_url },
                    detail: meta
                        .as_ref()
                        .and_then(serde_json::Value::as_object)
                        .and_then(|meta| meta.get(CODEX_IMAGE_DETAIL_META_KEY))
                        .and_then(serde_json::Value::as_str)
                        .and_then(|detail| match detail {
                            "auto" => Some(ImageDetail::Auto),
                            "low" => Some(ImageDetail::Low),
                            "high" => Some(ImageDetail::High),
                            "original" => Some(ImageDetail::Original),
                            _ => None,
                        })
                        .or(Some(DEFAULT_IMAGE_DETAIL)),
                }
            }
            Ok(McpContent::Audio {
                data, mime_type, ..
            }) => {
                let audio_url = if data.starts_with("data:") {
                    data
                } else {
                    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".into());
                    format!("data:{mime_type};base64,{data}")
                };
                FunctionCallOutputContentItem::InputAudio { audio_url }
            }
            Ok(McpContent::Unknown) | Err(_) => FunctionCallOutputContentItem::InputText {
                text: serde_json::to_string(content).unwrap_or_else(|_| "<content>".to_string()),
            },
        };
        items.push(item);
    }

    items
}

// Implement Display so callers can treat the payload like a plain string when logging or doing
// trivial substring checks in tests (existing tests call `.contains()` on the output). For
// `ContentItems`, Display emits a JSON representation.

impl std::fmt::Display for FunctionCallOutputPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.body {
            FunctionCallOutputBody::Text(content) => f.write_str(content),
            FunctionCallOutputBody::ContentItems(items) => {
                let content = serde_json::to_string(items).unwrap_or_default();
                f.write_str(content.as_str())
            }
        }
    }
}

// (Moved event mapping logic into codex-core to avoid coupling protocol to UI-facing events.)

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
