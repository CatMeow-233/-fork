#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use crate::JSONRPCNotification;
use crate::JSONRPCRequest;
use crate::JsonSchema;
use crate::RequestId;
use crate::TS;
#[cfg(test)]
use crate::export::GeneratedSchema;
#[cfg(test)]
use crate::export::write_json_schema;
use crate::protocol::v1;
use crate::protocol::v2;
use codex_experimental_api_macros::ExperimentalApi;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;

/// Authentication mode for OpenAI-backed providers.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// OpenAI API key provided by the caller and stored by Codex.
    ApiKey,
    /// ChatGPT OAuth managed by Codex (tokens persisted and refreshed by Codex).
    Chatgpt,
    /// [UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.
    ///
    /// ChatGPT auth tokens are supplied by an external host app and are only
    /// stored in memory. Token refresh must be handled by the external host app.
    #[serde(rename = "chatgptAuthTokens")]
    #[ts(rename = "chatgptAuthTokens")]
    #[strum(serialize = "chatgptAuthTokens")]
    ChatgptAuthTokens,
    /// Backend auth supplied as request headers.
    #[serde(rename = "headers")]
    #[ts(rename = "headers")]
    #[strum(serialize = "headers")]
    Headers,
    /// Programmatic Codex auth backed by a registered Agent Identity.
    #[serde(rename = "agentIdentity")]
    #[ts(rename = "agentIdentity")]
    #[strum(serialize = "agentIdentity")]
    AgentIdentity,
    /// Programmatic Codex auth backed by a personal access token.
    #[serde(rename = "personalAccessToken")]
    #[ts(rename = "personalAccessToken")]
    #[strum(serialize = "personalAccessToken")]
    PersonalAccessToken,
    /// Amazon Bedrock bearer token managed by Codex.
    #[serde(rename = "bedrockApiKey")]
    #[ts(rename = "bedrockApiKey")]
    #[strum(serialize = "bedrockApiKey")]
    BedrockApiKey,
    /// Amazon Bedrock AWS access keys managed by Codex.
    #[serde(rename = "bedrockAccessKeys")]
    #[ts(rename = "bedrockAccessKeys")]
    #[strum(serialize = "bedrockAccessKeys")]
    BedrockAccessKeys,
}

impl AuthMode {
    /// Returns whether this mode represents an authenticated human ChatGPT account.
    pub fn has_chatgpt_account(self) -> bool {
        match self {
            Self::Chatgpt | Self::ChatgptAuthTokens | Self::PersonalAccessToken => true,
            Self::ApiKey
            | Self::Headers
            | Self::AgentIdentity
            | Self::BedrockApiKey
            | Self::BedrockAccessKeys => false,
        }
    }

    /// Returns whether this mode is backed by Codex services rather than a direct model API.
    pub fn uses_codex_backend(self) -> bool {
        match self {
            Self::Chatgpt
            | Self::ChatgptAuthTokens
            | Self::Headers
            | Self::AgentIdentity
            | Self::PersonalAccessToken => true,
            Self::ApiKey | Self::BedrockApiKey | Self::BedrockAccessKeys => false,
        }
    }
}

macro_rules! experimental_reason_expr {
    // If a request variant is explicitly marked experimental, that reason wins.
    (variant $variant:ident, #[experimental($reason:expr)] $params:ident $(, $inspect_params:tt)?) => {
        Some($reason)
    };
    // `inspect_params: true` is used when a method is mostly stable but needs
    // field-level gating from its params type (for example, ThreadStart).
    (variant $variant:ident, $params:ident, true) => {
        crate::experimental_api::ExperimentalApi::experimental_reason($params)
    };
    (variant $variant:ident, $params:ident $(, $inspect_params:tt)?) => {
        None
    };
}

#[cfg(test)]
macro_rules! experimental_method_entry {
    (#[experimental($reason:expr)] => $wire:literal) => {
        $wire
    };
    (#[experimental($reason:expr)]) => {
        $reason
    };
    ($($tt:tt)*) => {
        ""
    };
}

#[cfg(test)]
macro_rules! experimental_type_entry {
    (#[experimental($reason:expr)] $ty:ty) => {
        stringify!($ty)
    };
    ($ty:ty) => {
        ""
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRequestSerializationScope {
    Global(&'static str),
    GlobalSharedRead(&'static str),
    Thread { thread_id: String },
    ThreadPath { path: PathBuf },
    CommandExecProcess { process_id: String },
    Process { process_handle: String },
    FuzzyFileSearchSession { session_id: String },
    FsWatch { watch_id: String },
    McpOauth { server_name: String },
}

macro_rules! serialization_scope_expr {
    ($actual_params:ident, None) => {
        None
    };
    ($actual_params:ident, global($key:literal)) => {
        Some(ClientRequestSerializationScope::Global($key))
    };
    ($actual_params:ident, global_shared_read($key:literal)) => {
        Some(ClientRequestSerializationScope::GlobalSharedRead($key))
    };
    // Match one or more dot-separated fields, allowing both `params.thread_id`
    // and nested paths like `params.start.thread_id`. The resolved ID selects
    // the per-thread request queue, preserving ordering with other thread operations.
    ($actual_params:ident, thread_id($params:ident . $($field:ident).+)) => {
        Some(ClientRequestSerializationScope::Thread {
            thread_id: $actual_params.$($field).+.clone(),
        })
    };
    ($actual_params:ident, optional_thread_id($params:ident . $field:ident)) => {
        $actual_params
            .$field
            .clone()
            .map(|thread_id| ClientRequestSerializationScope::Thread { thread_id })
    };
    ($actual_params:ident, thread_or_path($params:ident . $thread_field:ident, $params2:ident . $path_field:ident)) => {
        if !$actual_params.$thread_field.is_empty() {
            Some(ClientRequestSerializationScope::Thread {
                thread_id: $actual_params.$thread_field.clone(),
            })
        } else if let Some(path) = $actual_params.$path_field.clone() {
            Some(ClientRequestSerializationScope::ThreadPath { path })
        } else {
            Some(ClientRequestSerializationScope::Thread {
                thread_id: $actual_params.$thread_field.clone(),
            })
        }
    };
    ($actual_params:ident, optional_command_process_id($params:ident . $field:ident)) => {
        $actual_params
            .$field
            .clone()
            .map(|process_id| ClientRequestSerializationScope::CommandExecProcess { process_id })
    };
    ($actual_params:ident, command_process_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::CommandExecProcess {
            process_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, process_handle($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::Process {
            process_handle: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, fuzzy_session_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::FuzzyFileSearchSession {
            session_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, fs_watch_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::FsWatch {
            watch_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, mcp_oauth_server($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::McpOauth {
            server_name: $actual_params.$field.clone(),
        })
    };
}

/// Generates an `enum ClientRequest` where each variant is a request that the
/// client can send to the server. Each variant has associated `params` and
/// `response` types. Also generates a `export_client_responses()` function to
/// export all response types to TypeScript.
macro_rules! client_request_definitions {
    (
        $(
            $(#[experimental($reason:expr)])?
            $(#[doc = $variant_doc:literal])*
            $variant:ident => $wire:literal {
                params: $(#[$params_meta:meta])* $params:ty,
                $(inspect_params: $inspect_params:tt,)?
                serialization: $serialization:ident $( ( $($serialization_args:tt)* ) )?,
                $(manual_payload_conversion: $manual_payload_conversion:ident,)?
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request from the client to the server.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ClientRequest {
            $(
                $(#[doc = $variant_doc])*
                #[serde(rename = $wire)]
                #[ts(rename = $wire)]
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    $(#[$params_meta])*
                    params: $params,
                },
            )*
        }

        impl ClientRequest {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub const fn method_name(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $wire,)*
                }
            }

            pub fn serialization_scope(&self) -> Option<ClientRequestSerializationScope> {
                match self {
                    $(
                        Self::$variant { params, .. } => {
                            let _ = params;
                            serialization_scope_expr!(
                                params, $serialization $( ( $($serialization_args)* ) )?
                            )
                        }
                    )*
                }
            }
        }

        impl TryFrom<JSONRPCRequest> for ClientRequest {
            type Error = serde_json::Error;

            fn try_from(request: JSONRPCRequest) -> Result<Self, Self::Error> {
                let JSONRPCRequest {
                    id: request_id,
                    method,
                    params,
                    trace: _,
                } = request;
                let mut request = serde_json::Map::new();
                request.insert("id".to_string(), serde_json::to_value(request_id)?);
                request.insert("method".to_string(), serde_json::Value::String(method));
                if let Some(params) = params {
                    request.insert("params".to_string(), params);
                }
                serde_json::from_value(serde_json::Value::Object(request))
            }
        }

        /// Typed response from the server to the client.
        #[derive(Serialize, Deserialize, Debug, Clone)]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ClientResponse {
            $(
                $(#[doc = $variant_doc])*
                #[serde(rename = $wire)]
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    response: $response,
                },
            )*
        }

        impl ClientResponse {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn method(&self) -> String {
                match self {
                    $(Self::$variant { .. } => $wire.to_string(),)*
                }
            }

            pub fn into_jsonrpc_parts(
                self,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                let (request_id, response) = match self {
                    $(
                        Self::$variant { request_id, response } => {
                            (request_id, ClientResponsePayload::$variant(response))
                        }
                    )*
                };
                serde_json::to_value(response).map(|result| (request_id, result))
            }
        }

        #[derive(Debug, Clone, Serialize)]
        #[serde(untagged)]
        #[allow(clippy::large_enum_variant)]
        pub enum ClientResponsePayload {
            $( $variant($response), )*
            InterruptConversation(v1::InterruptConversationResponse),
        }

        impl ClientResponsePayload {
            pub fn into_client_response(self, request_id: RequestId) -> Option<ClientResponse> {
                match self {
                    $(
                        Self::$variant(response) => {
                            Some(ClientResponse::$variant {
                                request_id,
                                response,
                            })
                        }
                    )*
                    Self::InterruptConversation(_) => None,
                }
            }

            pub fn into_jsonrpc_parts(
                self,
                request_id: RequestId,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                self.to_jsonrpc_parts(request_id)
            }

            pub fn to_jsonrpc_parts(
                &self,
                request_id: RequestId,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                serde_json::to_value(self).map(|result| (request_id, result))
            }
        }

        impl From<v1::InterruptConversationResponse> for ClientResponsePayload {
            fn from(response: v1::InterruptConversationResponse) -> Self {
                Self::InterruptConversation(response)
            }
        }

        $(
            client_response_payload_from_impl!(
                $variant,
                $response
                $(, $manual_payload_conversion)?
            );
        )*

        impl crate::experimental_api::ExperimentalApi for ClientRequest {
            fn experimental_reason(&self) -> Option<&'static str> {
                match self {
                    $(
                        Self::$variant { params: _params, .. } => {
                            experimental_reason_expr!(
                                variant $variant,
                                $(#[experimental($reason)])?
                                _params
                                $(, $inspect_params)?
                            )
                        }
                    )*
                }
            }
        }

        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_CLIENT_METHODS: &[&str] = &[
            $(
                experimental_method_entry!($(#[experimental($reason)])? => $wire),
            )*
        ];
        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_CLIENT_METHOD_PARAM_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $params),
            )*
        ];
        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_CLIENT_METHOD_RESPONSE_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $response),
            )*
        ];

        #[cfg(test)]
        pub fn export_client_responses(
            out_dir: &::std::path::Path,
        ) -> ::std::result::Result<(), ::ts_rs::ExportError> {
            $(
                <$response as ::ts_rs::TS>::export_all_to(out_dir)?;
            )*
            Ok(())
        }

        #[cfg(test)]
        pub(crate) fn visit_client_response_types(v: &mut impl ::ts_rs::TypeVisitor) {
            $(
                v.visit::<$response>();
            )*
        }

        #[cfg(test)]
        #[allow(clippy::vec_init_then_push)]
        pub fn export_client_response_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(write_json_schema::<$response>(out_dir, stringify!($response))?);
            )*
            Ok(schemas)
        }

        #[cfg(test)]
        #[allow(clippy::vec_init_then_push)]
        pub fn export_client_param_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(write_json_schema::<$params>(out_dir, stringify!($params))?);
            )*
            Ok(schemas)
        }
    };
}

macro_rules! client_response_payload_from_impl {
    ($variant:ident, $response:ty) => {
        impl From<$response> for ClientResponsePayload {
            fn from(response: $response) -> Self {
                Self::$variant(response)
            }
        }
    };
    ($variant:ident, $response:ty, manual) => {};
}

/// Preserve explicit `undefined` accepted by the original stable usage request.
///
/// A Rust-based TypeScript proxy retains dependency discovery; a raw `#[ts(type = ...)]`
/// override would silently omit the generated params import and schema fixture.
#[allow(dead_code)]
#[derive(TS)]
#[ts(untagged)]
enum GetAccountTokenUsageParamsTypeScript {
    Params(v2::GetAccountTokenUsageParams),
    #[ts(type = "undefined")]
    Undefined,
}

/// Preserve omitted/undefined params while exporting the new usage capability type.
#[allow(dead_code)]
#[derive(TS)]
#[ts(untagged)]
enum GetAccountRateLimitsParamsTypeScript {
    Params(v2::GetAccountRateLimitsParams),
    #[ts(type = "undefined")]
    Undefined,
}

/// Preserve null and explicit undefined params while adding provider selection to gateway reads.
#[allow(dead_code)]
#[derive(TS)]
#[ts(untagged)]
enum GatewayOAuthReadParamsTypeScript {
    Params(Option<v2::GatewayOAuthReadParams>),
    #[ts(type = "undefined")]
    Undefined,
}

/// Preserve null and explicit undefined params while adding provider selection to gateway login.
#[allow(dead_code)]
#[derive(TS)]
#[ts(untagged)]
enum GatewayOAuthLoginParamsTypeScript {
    Params(Option<v2::GatewayOAuthLoginParams>),
    #[ts(type = "undefined")]
    Undefined,
}

client_request_definitions! {
    Initialize => "initialize" {
        params: v1::InitializeParams,
        serialization: None,
        response: v1::InitializeResponse,
    },

    #[experimental("server/diagnostics")]
    /// Read content-free, process-local diagnostics.
    ServerDiagnostics => "server/diagnostics" {
        params: v2::ServerDiagnosticsParams,
        serialization: None,
        response: v2::ServerDiagnosticsResponse,
    },

    #[experimental("userVerification/status")]
    UserVerificationStatus => "userVerification/status" {
        params: v2::UserVerificationStatusParams,
        serialization: None,
        response: v2::UserVerificationStatusResponse,
    },

    #[experimental("userVerification/enroll")]
    UserVerificationEnroll => "userVerification/enroll" {
        params: v2::UserVerificationEnrollParams,
        serialization: None,
        response: v2::UserVerificationEnrollResponse,
    },

    #[experimental("userVerification/delete")]
    UserVerificationDelete => "userVerification/delete" {
        params: v2::UserVerificationDeleteParams,
        serialization: None,
        response: v2::UserVerificationDeleteResponse,
    },

    #[experimental("userVerification/verify")]
    UserVerificationVerify => "userVerification/verify" {
        params: v2::UserVerificationVerifyParams,
        serialization: None,
        response: v2::UserVerificationVerifyResponse,
    },

    #[experimental("userVerification/cancel")]
    UserVerificationCancel => "userVerification/cancel" {
        params: v2::UserVerificationCancelParams,
        serialization: None,
        response: v2::UserVerificationCancelResponse,
    },

    /// NEW APIs
    // Thread lifecycle
    // Uses `inspect_params` because only some fields are experimental.
    ThreadStart => "thread/start" {
        params: v2::ThreadStartParams,
        inspect_params: true,
        serialization: None,
        response: v2::ThreadStartResponse,
    },
    ThreadResume => "thread/resume" {
        params: v2::ThreadResumeParams,
        inspect_params: true,
        serialization: thread_or_path(params.thread_id, params.path),
        response: v2::ThreadResumeResponse,
    },
    ThreadFork => "thread/fork" {
        params: v2::ThreadForkParams,
        inspect_params: true,
        serialization: thread_or_path(params.thread_id, params.path),
        response: v2::ThreadForkResponse,
    },
    #[experimental("thread/sample")]
    ThreadSample => "thread/sample" {
        params: v2::ThreadSampleParams,
        // Waiting for a sample must not block requests on its parent thread.
        serialization: None,
        response: v2::ThreadSampleResponse,
    },
    #[experimental("thread/sample/cancel")]
    ThreadSampleCancel => "thread/sample/cancel" {
        params: v2::ThreadSampleCancelParams,
        serialization: None,
        response: v2::ThreadSampleCancelResponse,
    },
    ThreadArchive => "thread/archive" {
        params: v2::ThreadArchiveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadArchiveResponse,
    },
    ThreadDelete => "thread/delete" {
        params: v2::ThreadDeleteParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadDeleteResponse,
    },
    ThreadUnsubscribe => "thread/unsubscribe" {
        params: v2::ThreadUnsubscribeParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadUnsubscribeResponse,
    },
    #[experimental("thread/increment_elicitation")]
    /// Increment the thread-local out-of-band elicitation counter.
    ///
    /// This is used by external helpers to pause timeout accounting while a user
    /// approval or other elicitation is pending outside the app-server request flow.
    ThreadIncrementElicitation => "thread/increment_elicitation" {
        params: v2::ThreadIncrementElicitationParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadIncrementElicitationResponse,
    },
    #[experimental("thread/decrement_elicitation")]
    /// Decrement the thread-local out-of-band elicitation counter.
    ///
    /// When the count reaches zero, timeout accounting resumes for the thread.
    ThreadDecrementElicitation => "thread/decrement_elicitation" {
        params: v2::ThreadDecrementElicitationParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadDecrementElicitationResponse,
    },
    ThreadSetName => "thread/name/set" {
        params: v2::ThreadSetNameParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadSetNameResponse,
    },
    #[experimental("thread/prediction/request")]
    ThreadPredictionRequest => "thread/prediction/request" {
        params: v2::ThreadPredictionRequestParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadPredictionRequestResponse,
    },
    ThreadGoalSet => "thread/goal/set" {
        params: v2::ThreadGoalSetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalSetResponse,
    },
    ThreadGoalGet => "thread/goal/get" {
        params: v2::ThreadGoalGetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalGetResponse,
    },
    ThreadGoalClear => "thread/goal/clear" {
        params: v2::ThreadGoalClearParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalClearResponse,
    },
    #[experimental("thread/queue/add")]
    ThreadQueueAdd => "thread/queue/add" {
        params: v2::ThreadQueueAddParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueAddResponse,
    },
    #[experimental("thread/queue/list")]
    ThreadQueueList => "thread/queue/list" {
        params: v2::ThreadQueueListParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueListResponse,
    },
    #[experimental("thread/queue/update")]
    ThreadQueueUpdate => "thread/queue/update" {
        params: v2::ThreadQueueUpdateParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueUpdateResponse,
    },
    #[experimental("thread/queue/delete")]
    ThreadQueueDelete => "thread/queue/delete" {
        params: v2::ThreadQueueDeleteParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueDeleteResponse,
    },
    #[experimental("thread/queue/reorder")]
    ThreadQueueReorder => "thread/queue/reorder" {
        params: v2::ThreadQueueReorderParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueReorderResponse,
    },
    #[experimental("thread/queue/start")]
    ThreadQueueStart => "thread/queue/start" {
        params: v2::ThreadQueueStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadQueueStartResponse,
    },
    ThreadMetadataUpdate => "thread/metadata/update" {
        params: v2::ThreadMetadataUpdateParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadMetadataUpdateResponse,
    },
    ThreadAttachmentAdd => "thread/attachment/add" {
        params: v2::ThreadAttachmentAddParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadAttachmentAddResponse,
    },
    ThreadAttachmentList => "thread/attachment/list" {
        params: v2::ThreadAttachmentListParams,
        serialization: None,
        response: v2::ThreadAttachmentListResponse,
    },
    ThreadAttachmentOwnerList => "thread/attachmentOwner/list" {
        params: v2::ThreadAttachmentOwnerListParams,
        serialization: None,
        response: v2::ThreadAttachmentOwnerListResponse,
    },
    ThreadAttachmentRemove => "thread/attachment/remove" {
        params: v2::ThreadAttachmentRemoveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadAttachmentRemoveResponse,
    },
    ThreadSectionMove => "thread/section/move" {
        params: v2::ThreadSectionMoveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadSectionMoveResponse,
    },
    #[experimental("thread/settings/update")]
    ThreadSettingsUpdate => "thread/settings/update" {
        params: v2::ThreadSettingsUpdateParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadSettingsUpdateResponse,
    },
    #[experimental("thread/memoryMode/set")]
    ThreadMemoryModeSet => "thread/memoryMode/set" {
        params: v2::ThreadMemoryModeSetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadMemoryModeSetResponse,
    },
    #[experimental("thread/memory/read")]
    ThreadMemoryRead => "thread/memory/read" {
        params: v2::ThreadMemoryReadParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadMemoryReadResponse,
    },
    #[experimental("memory/status")]
    MemoryStatus => "memory/status" {
        params: v2::MemoryStatusParams,
        serialization: global("memory"),
        response: v2::MemoryStatusResponse,
    },
    #[experimental("memory/reset")]
    MemoryReset => "memory/reset" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("memory"),
        response: v2::MemoryResetResponse,
    },
    #[experimental("rollout/compress")]
    /// Start a best-effort background compression pass for cold local rollouts.
    RolloutCompress => "rollout/compress" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::RolloutCompressResponse,
    },
    ThreadUnarchive => "thread/unarchive" {
        params: v2::ThreadUnarchiveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadUnarchiveResponse,
    },
    ThreadCompactStart => "thread/compact/start" {
        params: v2::ThreadCompactStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadCompactStartResponse,
    },
    ThreadShellCommand => "thread/shellCommand" {
        params: v2::ThreadShellCommandParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadShellCommandResponse,
    },
    ThreadApproveGuardianDeniedAction => "thread/approveGuardianDeniedAction" {
        params: v2::ThreadApproveGuardianDeniedActionParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadApproveGuardianDeniedActionResponse,
    },
    #[experimental("thread/backgroundTerminals/clean")]
    ThreadBackgroundTerminalsClean => "thread/backgroundTerminals/clean" {
        params: v2::ThreadBackgroundTerminalsCleanParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadBackgroundTerminalsCleanResponse,
    },
    #[experimental("thread/backgroundTerminals/list")]
    ThreadBackgroundTerminalsList => "thread/backgroundTerminals/list" {
        params: v2::ThreadBackgroundTerminalsListParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadBackgroundTerminalsListResponse,
    },
    #[experimental("thread/backgroundTerminals/terminate")]
    ThreadBackgroundTerminalsTerminate => "thread/backgroundTerminals/terminate" {
        params: v2::ThreadBackgroundTerminalsTerminateParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadBackgroundTerminalsTerminateResponse,
    },
    ThreadRevert => "thread/revert" {
        params: v2::ThreadRevertParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRevertResponse,
    },
    ThreadList => "thread/list" {
        params: v2::ThreadListParams,
        inspect_params: true,
        serialization: None,
        response: v2::ThreadListResponse,
    },
    #[experimental("project/list")]
    ProjectList => "project/list" {
        params: v2::ProjectListParams,
        serialization: global_shared_read("projects"),
        response: v2::ProjectListResponse,
    },
    #[experimental("project/read")]
    ProjectRead => "project/read" {
        params: v2::ProjectReadParams,
        serialization: global_shared_read("projects"),
        response: v2::ProjectReadResponse,
    },
    #[experimental("project/create")]
    ProjectCreate => "project/create" {
        params: v2::ProjectCreateParams,
        serialization: global("projects"),
        response: v2::ProjectCreateResponse,
    },
    #[experimental("project/import")]
    ProjectImport => "project/import" {
        params: v2::ProjectImportParams,
        serialization: global("projects"),
        response: v2::ProjectImportResponse,
    },
    #[experimental("project/update")]
    ProjectUpdate => "project/update" {
        params: v2::ProjectUpdateParams,
        serialization: global("projects"),
        response: v2::ProjectUpdateResponse,
    },
    #[experimental("project/move")]
    ProjectMove => "project/move" {
        params: v2::ProjectMoveParams,
        serialization: global("projects"),
        response: v2::ProjectMoveResponse,
    },
    #[experimental("project/delete")]
    ProjectDelete => "project/delete" {
        params: v2::ProjectDeleteParams,
        serialization: global("projects"),
        response: v2::ProjectDeleteResponse,
    },
    ThreadSectionList => "threadSection/list" {
        params: v2::ThreadSectionListParams,
        serialization: global_shared_read("thread-sections"),
        response: v2::ThreadSectionListResponse,
    },
    ThreadSectionCreate => "threadSection/create" {
        params: v2::ThreadSectionCreateParams,
        serialization: global("thread-sections"),
        response: v2::ThreadSectionCreateResponse,
    },
    ThreadSectionUpdate => "threadSection/update" {
        params: v2::ThreadSectionUpdateParams,
        serialization: global("thread-sections"),
        response: v2::ThreadSectionUpdateResponse,
    },
    ThreadSectionDelete => "threadSection/delete" {
        params: v2::ThreadSectionDeleteParams,
        serialization: global("thread-sections"),
        response: v2::ThreadSectionDeleteResponse,
    },
    #[experimental("thread/search")]
    ThreadSearch => "thread/search" {
        params: v2::ThreadSearchParams,
        serialization: None,
        response: v2::ThreadSearchResponse,
    },
    #[experimental("thread/searchOccurrences")]
    ThreadSearchOccurrences => "thread/searchOccurrences" {
        params: v2::ThreadSearchOccurrencesParams,
        // Explicitly concurrent: this reads persisted paginated history.
        serialization: None,
        response: v2::ThreadSearchOccurrencesResponse,
    },
    ThreadLoadedList => "thread/loaded/list" {
        params: v2::ThreadLoadedListParams,
        serialization: None,
        response: v2::ThreadLoadedListResponse,
    },
    ThreadRead => "thread/read" {
        params: v2::ThreadReadParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadReadResponse,
    },
    #[experimental("thread/readState/update")]
    ThreadReadStateUpdate => "thread/readState/update" {
        params: v2::ThreadReadStateUpdateParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadReadStateUpdateResponse,
    },
    ThreadTurnsList => "thread/turns/list" {
        params: v2::ThreadTurnsListParams,
        // Explicitly concurrent: this primarily reads append-only rollout storage.
        serialization: None,
        response: v2::ThreadTurnsListResponse,
    },
    ThreadItemsList => "thread/items/list" {
        params: v2::ThreadItemsListParams,
        inspect_params: true,
        // Explicitly concurrent: this primarily reads append-only rollout storage.
        serialization: None,
        response: v2::ThreadItemsListResponse,
    },
    #[experimental("thread/items/read")]
    ThreadItemsRead => "thread/items/read" {
        params: v2::ThreadItemsReadParams,
        // Explicitly concurrent: this primarily reads append-only rollout storage.
        serialization: None,
        response: v2::ThreadItemsReadResponse,
    },
    #[experimental("thread/guardianReview")]
    /// Review the idle thread's final injected exec_command without executing it.
    ThreadGuardianReview => "thread/guardianReview" {
        params: v2::ThreadGuardianReviewParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGuardianReviewResponse,
    },
    /// Append raw Responses API items to the thread history without starting a user turn.
    ThreadInjectItems => "thread/inject_items" {
        params: v2::ThreadInjectItemsParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadInjectItemsResponse,
    },
    SkillsList => "skills/list" {
        params: v2::SkillsListParams,
        serialization: global_shared_read("config"),
        response: v2::SkillsListResponse,
    },
    SkillsExtraRootsSet => "skills/extraRoots/set" {
        params: v2::SkillsExtraRootsSetParams,
        serialization: global("config"),
        response: v2::SkillsExtraRootsSetResponse,
    },
    HooksList => "hooks/list" {
        params: v2::HooksListParams,
        serialization: global_shared_read("config"),
        response: v2::HooksListResponse,
    },
    MarketplaceAdd => "marketplace/add" {
        params: v2::MarketplaceAddParams,
        serialization: global("config"),
        response: v2::MarketplaceAddResponse,
    },
    MarketplaceRemove => "marketplace/remove" {
        params: v2::MarketplaceRemoveParams,
        serialization: global("config"),
        response: v2::MarketplaceRemoveResponse,
    },
    MarketplaceUpgrade => "marketplace/upgrade" {
        params: v2::MarketplaceUpgradeParams,
        serialization: global("config"),
        response: v2::MarketplaceUpgradeResponse,
    },
    PluginList => "plugin/list" {
        params: v2::PluginListParams,
        serialization: None,
        response: v2::PluginListResponse,
    },
    #[experimental("plugin/search")]
    PluginSearch => "plugin/search" {
        params: v2::PluginSearchParams,
        serialization: None,
        response: v2::PluginSearchResponse,
    },
    PluginInstalled => "plugin/installed" {
        params: v2::PluginInstalledParams,
        serialization: None,
        response: v2::PluginInstalledResponse,
    },
    PluginReconcile => "plugin/reconcile" {
        params: v2::PluginReconcileParams,
        serialization: None,
        response: v2::PluginReconcileResponse,
    },
    PluginRead => "plugin/read" {
        params: v2::PluginReadParams,
        serialization: None,
        response: v2::PluginReadResponse,
    },
    PluginSkillRead => "plugin/skill/read" {
        params: v2::PluginSkillReadParams,
        serialization: global("config"),
        response: v2::PluginSkillReadResponse,
    },
    PluginShareSave => "plugin/share/save" {
        params: v2::PluginShareSaveParams,
        serialization: global("config"),
        response: v2::PluginShareSaveResponse,
    },
    PluginShareUpdateTargets => "plugin/share/updateTargets" {
        params: v2::PluginShareUpdateTargetsParams,
        serialization: global("config"),
        response: v2::PluginShareUpdateTargetsResponse,
    },
    PluginShareList => "plugin/share/list" {
        params: v2::PluginShareListParams,
        serialization: global("config"),
        response: v2::PluginShareListResponse,
    },
    PluginShareCheckout => "plugin/share/checkout" {
        params: v2::PluginShareCheckoutParams,
        serialization: global("config"),
        response: v2::PluginShareCheckoutResponse,
    },
    PluginShareDelete => "plugin/share/delete" {
        params: v2::PluginShareDeleteParams,
        serialization: global("config"),
        response: v2::PluginShareDeleteResponse,
    },
    AppsRead => "app/read" {
        params: v2::AppsReadParams,
        serialization: None,
        response: v2::AppsReadResponse,
    },
    AppsList => "app/list" {
        params: v2::AppsListParams,
        serialization: None,
        response: v2::AppsListResponse,
    },
    AppsInstalled => "app/installed" {
        params: v2::AppsInstalledParams,
        serialization: None,
        response: v2::AppsInstalledResponse,
    },
    // File system requests are intentionally concurrent. Desktop already treats local
    // file system operations as concurrent, and app-server remote fs mirrors that model.
    FsReadFile => "fs/readFile" {
        params: v2::FsReadFileParams,
        serialization: None,
        response: v2::FsReadFileResponse,
    },
    FsWriteFile => "fs/writeFile" {
        params: v2::FsWriteFileParams,
        serialization: None,
        response: v2::FsWriteFileResponse,
    },
    FsCreateDirectory => "fs/createDirectory" {
        params: v2::FsCreateDirectoryParams,
        serialization: None,
        response: v2::FsCreateDirectoryResponse,
    },
    FsGetMetadata => "fs/getMetadata" {
        params: v2::FsGetMetadataParams,
        serialization: None,
        response: v2::FsGetMetadataResponse,
    },
    FsReadDirectory => "fs/readDirectory" {
        params: v2::FsReadDirectoryParams,
        serialization: None,
        response: v2::FsReadDirectoryResponse,
    },
    FsRemove => "fs/remove" {
        params: v2::FsRemoveParams,
        serialization: None,
        response: v2::FsRemoveResponse,
    },
    FsCopy => "fs/copy" {
        params: v2::FsCopyParams,
        serialization: None,
        response: v2::FsCopyResponse,
    },
    FsWatch => "fs/watch" {
        params: v2::FsWatchParams,
        serialization: fs_watch_id(params.watch_id),
        response: v2::FsWatchResponse,
    },
    FsUnwatch => "fs/unwatch" {
        params: v2::FsUnwatchParams,
        serialization: fs_watch_id(params.watch_id),
        response: v2::FsUnwatchResponse,
    },
    SkillsConfigWrite => "skills/config/write" {
        params: v2::SkillsConfigWriteParams,
        serialization: global("config"),
        response: v2::SkillsConfigWriteResponse,
    },
    PluginInstall => "plugin/install" {
        params: v2::PluginInstallParams,
        serialization: global("config"),
        response: v2::PluginInstallResponse,
    },
    PluginUninstall => "plugin/uninstall" {
        params: v2::PluginUninstallParams,
        serialization: global("config"),
        response: v2::PluginUninstallResponse,
    },
    TurnStart => "turn/start" {
        params: v2::TurnStartParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::TurnStartResponse,
    },
    #[experimental("turn/settings/update")]
    TurnSettingsUpdate => "turn/settings/update" {
        params: v2::TurnSettingsUpdateParams,
        serialization: thread_id(params.thread_id),
        response: v2::TurnSettingsUpdateResponse,
    },
    TurnSteer => "turn/steer" {
        params: v2::TurnSteerParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::TurnSteerResponse,
    },
    TurnInterrupt => "turn/interrupt" {
        params: v2::TurnInterruptParams,
        serialization: thread_id(params.thread_id),
        response: v2::TurnInterruptResponse,
    },
    #[experimental("thread/realtime/start")]
    ThreadRealtimeStart => "thread/realtime/start" {
        params: v2::ThreadRealtimeStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeStartResponse,
    },
    #[experimental("thread/realtime/appendAudio")]
    ThreadRealtimeAppendAudio => "thread/realtime/appendAudio" {
        params: v2::ThreadRealtimeAppendAudioParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeAppendAudioResponse,
    },
    #[experimental("thread/realtime/appendText")]
    ThreadRealtimeAppendText => "thread/realtime/appendText" {
        params: v2::ThreadRealtimeAppendTextParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeAppendTextResponse,
    },
    #[experimental("thread/realtime/appendSpeech")]
    ThreadRealtimeAppendSpeech => "thread/realtime/appendSpeech" {
        params: v2::ThreadRealtimeAppendSpeechParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeAppendSpeechResponse,
    },
    #[experimental("thread/realtime/stop")]
    ThreadRealtimeStop => "thread/realtime/stop" {
        params: v2::ThreadRealtimeStopParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeStopResponse,
    },
    #[experimental("thread/timeline/list")]
    ThreadTimelineList => "thread/timeline/list" {
        params: v2::ThreadTimelineListParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadTimelineListResponse,
    },
    #[experimental("thread/realtime/listVoices")]
    ThreadRealtimeListVoices => "thread/realtime/listVoices" {
        params: v2::ThreadRealtimeListVoicesParams,
        serialization: None,
        response: v2::ThreadRealtimeListVoicesResponse,
    },
    ReviewStart => "review/start" {
        params: v2::ReviewStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ReviewStartResponse,
    },

    ModelList => "model/list" {
        params: v2::ModelListParams,
        serialization: None,
        response: v2::ModelListResponse,
    },
    GatewayOAuthRead => "account/gatewayOAuth/read" {
        params: #[ts(optional, as = "Option<GatewayOAuthReadParamsTypeScript>", inline)] #[serde(default, skip_serializing_if = "Option::is_none")] v2::NullableGatewayOAuthReadParams,
        serialization: None,
        response: v2::GatewayOAuthReadResponse,
    },
    GatewayOAuthLogin => "account/gatewayOAuth/login" {
        params: #[ts(optional, as = "Option<GatewayOAuthLoginParamsTypeScript>", inline)] #[serde(default, skip_serializing_if = "Option::is_none")] v2::NullableGatewayOAuthLoginParams,
        serialization: None,
        response: v2::GatewayOAuthLoginResponse,
    },
    GatewayOAuthCancel => "account/gatewayOAuth/cancel" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GatewayOAuthCancelResponse,
    },
    ModelProviderList => "modelProvider/list" {
        params: v2::ModelProviderListParams,
        serialization: global_shared_read("config"),
        response: v2::ModelProviderListResponse,
    },
    ModelProviderConfigWrite => "modelProvider/config/write" {
        params: v2::ModelProviderConfigWriteParams,
        serialization: global("config"),
        response: v2::ModelProviderConfigWriteResponse,
    },
    ModelProviderCapabilitiesRead => "modelProvider/capabilities/read" {
        params: v2::ModelProviderCapabilitiesReadParams,
        serialization: None,
        response: v2::ModelProviderCapabilitiesReadResponse,
    },
    ExperimentalFeatureList => "experimentalFeature/list" {
        params: v2::ExperimentalFeatureListParams,
        serialization: global("config"),
        response: v2::ExperimentalFeatureListResponse,
    },
    PermissionProfileList => "permissionProfile/list" {
        params: v2::PermissionProfileListParams,
        serialization: global_shared_read("config"),
        response: v2::PermissionProfileListResponse,
    },
    ExperimentalFeatureEnablementSet => "experimentalFeature/enablement/set" {
        params: v2::ExperimentalFeatureEnablementSetParams,
        serialization: global("config"),
        response: v2::ExperimentalFeatureEnablementSetResponse,
    },
    #[experimental("remoteControl/enable")]
    RemoteControlEnable => "remoteControl/enable" {
        params: #[serde(skip_serializing_if = "Option::is_none")] v2::NullableRemoteControlEnableParams,
        serialization: global("remote-control"),
        response: v2::RemoteControlEnableResponse,
    },
    #[experimental("remoteControl/disable")]
    RemoteControlDisable => "remoteControl/disable" {
        params: #[serde(skip_serializing_if = "Option::is_none")] v2::NullableRemoteControlDisableParams,
        serialization: global("remote-control"),
        response: v2::RemoteControlDisableResponse,
    },
    #[experimental("remoteControl/status/read")]
    RemoteControlStatusRead => "remoteControl/status/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global_shared_read("remote-control"),
        response: v2::RemoteControlStatusReadResponse,
    },
    #[experimental("remoteControl/pairing/start")]
    RemoteControlPairingStart => "remoteControl/pairing/start" {
        params: v2::RemoteControlPairingStartParams,
        serialization: global("remote-control-pairing"),
        response: v2::RemoteControlPairingStartResponse,
    },
    #[experimental("remoteControl/pairing/status")]
    RemoteControlPairingStatus => "remoteControl/pairing/status" {
        params: v2::RemoteControlPairingStatusParams,
        serialization: global_shared_read("remote-control-pairing"),
        response: v2::RemoteControlPairingStatusResponse,
    },
    #[experimental("remoteControl/client/list")]
    RemoteControlClientsList => "remoteControl/client/list" {
        params: v2::RemoteControlClientsListParams,
        serialization: global_shared_read("remote-control-clients"),
        response: v2::RemoteControlClientsListResponse,
    },
    #[experimental("remoteControl/client/revoke")]
    RemoteControlClientsRevoke => "remoteControl/client/revoke" {
        params: v2::RemoteControlClientsRevokeParams,
        serialization: global("remote-control-clients"),
        response: v2::RemoteControlClientsRevokeResponse,
    },
    #[experimental("collaborationMode/list")]
    /// Lists collaboration mode presets.
    CollaborationModeList => "collaborationMode/list" {
        params: v2::CollaborationModeListParams,
        serialization: None,
        response: v2::CollaborationModeListResponse,
    },
    #[experimental("mock/experimentalMethod")]
    /// Test-only method used to validate experimental gating.
    MockExperimentalMethod => "mock/experimentalMethod" {
        params: v2::MockExperimentalMethodParams,
        serialization: None,
        response: v2::MockExperimentalMethodResponse,
    },
    #[experimental("environment/add")]
    /// Adds or replaces a remote environment by id for later selection.
    EnvironmentAdd => "environment/add" {
        params: v2::EnvironmentAddParams,
        serialization: global("environment"),
        response: v2::EnvironmentAddResponse,
    },
    #[experimental("environment/info")]
    /// Reads information from a configured execution environment.
    EnvironmentInfo => "environment/info" {
        params: v2::EnvironmentInfoParams,
        serialization: global_shared_read("environment"),
        response: v2::EnvironmentInfoResponse,
    },
    #[experimental("environment/status")]
    /// Reads the current status of a configured execution environment.
    EnvironmentStatus => "environment/status" {
        params: v2::EnvironmentStatusParams,
        serialization: global_shared_read("environment"),
        response: v2::EnvironmentStatusResponse,
    },

    McpServerOauthLogin => "mcpServer/oauth/login" {
        params: v2::McpServerOauthLoginParams,
        serialization: mcp_oauth_server(params.name),
        response: v2::McpServerOauthLoginResponse,
    },

    McpServerRefresh => "config/mcpServer/reload" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("mcp-registry"),
        response: v2::McpServerRefreshResponse,
    },

    McpServerStatusList => "mcpServerStatus/list" {
        params: v2::ListMcpServerStatusParams,
        serialization: global("mcp-registry"),
        response: v2::ListMcpServerStatusResponse,
    },

    McpResourceRead => "mcpServer/resource/read" {
        params: v2::McpResourceReadParams,
        serialization: optional_thread_id(params.thread_id),
        response: v2::McpResourceReadResponse,
    },

    #[experimental("mcpServer/event/stream/start")]
    McpServerEventStreamStart => "mcpServer/event/stream/start" {
        params: v2::McpServerEventStreamStartParams,
        serialization: None,
        response: v2::McpServerEventStreamStartResponse,
    },

    #[experimental("mcpServer/event/stream/stop")]
    McpServerEventStreamStop => "mcpServer/event/stream/stop" {
        params: v2::McpServerEventStreamStopParams,
        serialization: None,
        response: v2::McpServerEventStreamStopResponse,
    },

    McpServerToolCall => "mcpServer/tool/call" {
        params: v2::McpServerToolCallParams,
        serialization: thread_id(params.thread_id),
        response: v2::McpServerToolCallResponse,
    },

    WindowsSandboxSetupStart => "windowsSandbox/setupStart" {
        params: v2::WindowsSandboxSetupStartParams,
        serialization: global("windows-sandbox-setup"),
        response: v2::WindowsSandboxSetupStartResponse,
    },
    WindowsSandboxReadiness => "windowsSandbox/readiness" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("config"),
        response: v2::WindowsSandboxReadinessResponse,
    },

    LoginAccount => "account/login/start" {
        params: v2::LoginAccountParams,
        inspect_params: true,
        serialization: global("account-auth"),
        response: v2::LoginAccountResponse,
    },

    #[experimental("account/bedrock/discover")]
    BedrockDiscover => "account/bedrock/discover" {
        params: v2::BedrockDiscoverParams,
        serialization: global_shared_read("account-auth"),
        response: v2::BedrockDiscoverResponse,
    },

    #[experimental("account/bedrock/setup")]
    BedrockSetup => "account/bedrock/setup" {
        params: v2::BedrockSetupParams,
        serialization: global("account-auth"),
        response: v2::BedrockSetupResponse,
    },

    #[experimental("account/gcp/setup")]
    GcpSetup => "account/gcp/setup" {
        params: v2::GcpSetupParams,
        serialization: global("account-auth"),
        response: v2::GcpSetupResponse,
    },

    #[experimental("account/bedrock/checkGovCloudRequirements")]
    BedrockCheckGovCloudRequirements => "account/bedrock/checkGovCloudRequirements" {
        params: v2::BedrockCheckGovCloudRequirementsParams,
        serialization: global("account-auth"),
        response: v2::BedrockCheckGovCloudRequirementsResponse,
    },

    CancelLoginAccount => "account/login/cancel" {
        params: v2::CancelLoginAccountParams,
        serialization: global("account-auth"),
        response: v2::CancelLoginAccountResponse,
    },

    #[experimental("accountSession/list")]
    AccountSessionsList => "accountSession/list" {
        params: v2::AccountSessionsListParams,
        serialization: global("account-auth"),
        manual_payload_conversion: manual,
        response: v2::AccountSessionsResponse,
    },

    #[experimental("accountSession/switch")]
    AccountSessionsSwitch => "accountSession/switch" {
        params: v2::AccountSessionsSwitchParams,
        serialization: global("account-auth"),
        manual_payload_conversion: manual,
        response: v2::AccountSessionsSwitchResponse,
    },

    LogoutAccount => "account/logout" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("account-auth"),
        response: v2::LogoutAccountResponse,
    },

    GetAccountSecuritySetup => "account/securitySetup/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GetAccountSecuritySetupResponse,
    },

    GetAccountRateLimits => "account/rateLimits/read" {
        params: #[ts(optional, as = "Option<GetAccountRateLimitsParamsTypeScript>", inline)] #[serde(default, skip_serializing_if = "Option::is_none")] v2::NullableGetAccountRateLimitsParams,
        serialization: None,
        response: v2::GetAccountRateLimitsResponse,
    },

    ConsumeAccountRateLimitResetCredit => "account/rateLimitResetCredit/consume" {
        params: v2::ConsumeAccountRateLimitResetCreditParams,
        serialization: global("account-auth"),
        response: v2::ConsumeAccountRateLimitResetCreditResponse,
    },

    GetAccountTokenUsage => "account/usage/read" {
        params: #[ts(optional, as = "Option<GetAccountTokenUsageParamsTypeScript>", inline)] #[serde(default, skip_serializing_if = "Option::is_none")] v2::NullableGetAccountTokenUsageParams,
        serialization: None,
        response: v2::GetAccountTokenUsageResponse,
    },

    GetWorkspaceMessages => "account/workspaceMessages/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GetWorkspaceMessagesResponse,
    },

    SendAddCreditsNudgeEmail => "account/sendAddCreditsNudgeEmail" {
        params: v2::SendAddCreditsNudgeEmailParams,
        serialization: global("account-auth"),
        response: v2::SendAddCreditsNudgeEmailResponse,
    },

    FeedbackUpload => "feedback/upload" {
        params: v2::FeedbackUploadParams,
        serialization: None,
        response: v2::FeedbackUploadResponse,
    },

    FeedbackSave => "feedback/save" {
        params: v2::FeedbackSaveParams,
        serialization: None,
        response: v2::FeedbackSaveResponse,
    },

    /// Execute a standalone command (argv vector) under the server's sandbox.
    OneOffCommandExec => "command/exec" {
        params: v2::CommandExecParams,
        inspect_params: true,
        serialization: optional_command_process_id(params.process_id),
        response: v2::CommandExecResponse,
    },
    /// Write stdin bytes to a running `command/exec` session or close stdin.
    CommandExecWrite => "command/exec/write" {
        params: v2::CommandExecWriteParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecWriteResponse,
    },
    /// Terminate a running `command/exec` session by client-supplied `processId`.
    CommandExecTerminate => "command/exec/terminate" {
        params: v2::CommandExecTerminateParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecTerminateResponse,
    },
    /// Resize a running PTY-backed `command/exec` session by client-supplied `processId`.
    CommandExecResize => "command/exec/resize" {
        params: v2::CommandExecResizeParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecResizeResponse,
    },
    #[experimental("process/spawn")]
    /// Spawn a standalone process (argv vector) without a Codex sandbox.
    ProcessSpawn => "process/spawn" {
        params: v2::ProcessSpawnParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessSpawnResponse,
    },
    #[experimental("process/writeStdin")]
    /// Write stdin bytes to a running `process/spawn` session or close stdin.
    ProcessWriteStdin => "process/writeStdin" {
        params: v2::ProcessWriteStdinParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessWriteStdinResponse,
    },
    #[experimental("process/kill")]
    /// Terminate a running `process/spawn` session by client-supplied `processHandle`.
    ProcessKill => "process/kill" {
        params: v2::ProcessKillParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessKillResponse,
    },
    #[experimental("process/resizePty")]
    /// Resize a running PTY-backed `process/spawn` session by client-supplied `processHandle`.
    ProcessResizePty => "process/resizePty" {
        params: v2::ProcessResizePtyParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessResizePtyResponse,
    },

    ConfigRead => "config/read" {
        params: v2::ConfigReadParams,
        serialization: global_shared_read("config"),
        response: v2::ConfigReadResponse,
    },
    ExternalAgentConfigDetect => "externalAgentConfig/detect" {
        params: v2::ExternalAgentConfigDetectParams,
        serialization: global("external-agent-detect"),
        response: v2::ExternalAgentConfigDetectResponse,
    },
    ExternalAgentConfigImport => "externalAgentConfig/import" {
        params: v2::ExternalAgentConfigImportParams,
        serialization: global("config"),
        response: v2::ExternalAgentConfigImportResponse,
    },
    ExternalAgentConfigImportHistoryRecord => "externalAgentConfig/import/recordHistory" {
        params: v2::ExternalAgentConfigImportHistoryRecordParams,
        serialization: global("config"),
        response: v2::ExternalAgentConfigImportHistoryRecordResponse,
    },
    ExternalAgentConfigImportHistoriesRead => "externalAgentConfig/import/readHistories" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global_shared_read("config"),
        response: v2::ExternalAgentConfigImportHistoriesReadResponse,
    },
    ConfigValueWrite => "config/value/write" {
        params: v2::ConfigValueWriteParams,
        serialization: global("config"),
        manual_payload_conversion: manual,
        response: v2::ConfigWriteResponse,
    },
    ConfigBatchWrite => "config/batchWrite" {
        params: v2::ConfigBatchWriteParams,
        serialization: global("config"),
        manual_payload_conversion: manual,
        response: v2::ConfigWriteResponse,
    },

    ConfigRequirementsRead => "configRequirements/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global_shared_read("config"),
        response: v2::ConfigRequirementsReadResponse,
    },

    GetAccount => "account/read" {
        params: v2::GetAccountParams,
        serialization: global("account-auth"),
        response: v2::GetAccountResponse,
    },

    /// DEPRECATED APIs below
    GetConversationSummary => "getConversationSummary" {
        params: v1::GetConversationSummaryParams,
        serialization: None,
        response: v1::GetConversationSummaryResponse,
    },
    GitDiffToRemote => "gitDiffToRemote" {
        params: v1::GitDiffToRemoteParams,
        serialization: None,
        response: v1::GitDiffToRemoteResponse,
    },
    /// DEPRECATED in favor of GetAccount
    GetAuthStatus => "getAuthStatus" {
        params: v1::GetAuthStatusParams,
        serialization: global("account-auth"),
        response: v1::GetAuthStatusResponse,
    },
    // Legacy fuzzy search cancellation is intentionally concurrent: clients reuse a
    // cancellation token so a newer request can cancel an older in-flight search.
    FuzzyFileSearch => "fuzzyFileSearch" {
        params: FuzzyFileSearchParams,
        serialization: None,
        response: FuzzyFileSearchResponse,
    },
    #[experimental("fuzzyFileSearch/sessionStart")]
    FuzzyFileSearchSessionStart => "fuzzyFileSearch/sessionStart" {
        params: FuzzyFileSearchSessionStartParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionStartResponse,
    },
    #[experimental("fuzzyFileSearch/sessionUpdate")]
    FuzzyFileSearchSessionUpdate => "fuzzyFileSearch/sessionUpdate" {
        params: FuzzyFileSearchSessionUpdateParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionUpdateResponse,
    },
    #[experimental("fuzzyFileSearch/sessionStop")]
    FuzzyFileSearchSessionStop => "fuzzyFileSearch/sessionStop" {
        params: FuzzyFileSearchSessionStopParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionStopResponse,
    },
}

/// Generates an `enum ServerRequest` where each variant is a request that the
/// server can send to the client along with the corresponding params and
/// response types. It also generates helper types used by the app/server
/// infrastructure (payload enum, request constructor, and export helpers).
macro_rules! server_request_definitions {
    (
        $(
            $(#[experimental($reason:expr)])?
            $(#[doc = $variant_doc:literal])*
            $variant:ident $(=> $wire:literal)? {
                params: $params:ty,
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request initiated from the server and sent to the client.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ServerRequest {
            $(
                $(#[doc = $variant_doc])*
                $(#[serde(rename = $wire)] #[ts(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    params: $params,
                },
            )*
        }

        impl ServerRequest {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn response_from_result(
                &self,
                result: crate::Result,
            ) -> serde_json::Result<ServerResponse> {
                match self {
                    $(
                        Self::$variant { request_id, .. } => {
                            let response = serde_json::from_value::<$response>(result)?;
                            Ok(ServerResponse::$variant {
                                request_id: request_id.clone(),
                                response,
                            })
                        }
                    )*
                }
            }
        }

        /// Typed response from the client to the server.
        #[derive(Serialize, Deserialize, Debug, Clone)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ServerResponse {
            $(
                $(#[doc = $variant_doc])*
                $(#[serde(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    response: $response,
                },
            )*
        }

        impl ServerResponse {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn method(&self) -> String {
                serde_json::to_value(self)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string())
            }
        }

        #[derive(Debug, Clone, PartialEq, JsonSchema)]
        #[allow(clippy::large_enum_variant)]
        pub enum ServerRequestPayload {
            $( $variant($params), )*
        }

        impl ServerRequestPayload {
            pub fn request_with_id(self, request_id: RequestId) -> ServerRequest {
                match self {
                    $(Self::$variant(params) => ServerRequest::$variant { request_id, params },)*
                }
            }
        }

        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_SERVER_METHODS: &[&str] = &[
            $(
                experimental_method_entry!($(#[experimental($reason)])? $(=> $wire)?),
            )*
        ];
        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_SERVER_METHOD_PARAM_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $params),
            )*
        ];
        #[cfg(test)]
        pub(crate) const EXPERIMENTAL_SERVER_METHOD_RESPONSE_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $response),
            )*
        ];

        #[cfg(test)]
        pub fn export_server_responses(
            out_dir: &::std::path::Path,
        ) -> ::std::result::Result<(), ::ts_rs::ExportError> {
            $(
                <$response as ::ts_rs::TS>::export_all_to(out_dir)?;
            )*
            Ok(())
        }

        #[cfg(test)]
        pub(crate) fn visit_server_response_types(v: &mut impl ::ts_rs::TypeVisitor) {
            $(
                v.visit::<$response>();
            )*
        }

        #[cfg(test)]
        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_response_schemas(
            out_dir: &Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(crate::export::write_json_schema::<$response>(
                    out_dir,
                    concat!(stringify!($variant), "Response"),
                )?);
            )*
            Ok(schemas)
        }

        #[cfg(test)]
        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_param_schemas(
            out_dir: &Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(crate::export::write_json_schema::<$params>(
                    out_dir,
                    concat!(stringify!($variant), "Params"),
                )?);
            )*
            Ok(schemas)
        }
    };
}

/// Generates `ServerNotification` enum and helpers, including a JSON Schema
/// exporter for each notification.
macro_rules! server_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $(=> $wire:literal)? ( $payload:ty )
        ),* $(,)?
    ) => {
        /// Notification sent from the server to the client.
        #[derive(
            Serialize,
            Deserialize,
            Debug,
            Clone,
            JsonSchema,
            TS,
            Display,
            ExperimentalApi,
        )]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ServerNotification {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)] #[ts(rename = $wire)] #[strum(serialize = $wire)])?
                $variant($payload),
            )*
        }

        impl ServerNotification {
            pub fn to_params(self) -> Result<serde_json::Value, serde_json::Error> {
                match self {
                    $(Self::$variant(params) => serde_json::to_value(params),)*
                }
            }
        }

        impl TryFrom<JSONRPCNotification> for ServerNotification {
            type Error = serde_json::Error;

            fn try_from(value: JSONRPCNotification) -> Result<Self, serde_json::Error> {
                serde_json::from_value(serde_json::to_value(value)?)
            }
        }

        #[cfg(test)]
        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_notification_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(schemas.push(crate::export::write_json_schema::<$payload>(out_dir, stringify!($payload))?);)*
            Ok(schemas)
        }
    };
}
/// Notifications sent from the client to the server.
macro_rules! client_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $( ( $payload:ty ) )?
        ),* $(,)?
    ) => {
        #[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS, Display)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ClientNotification {
            $(
                $(#[$variant_meta])*
                $variant $( ( $payload ) )?,
            )*
        }

        #[cfg(test)]
        pub fn export_client_notification_schemas(
            _out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let schemas = Vec::new();
            $( $(schemas.push(crate::export::write_json_schema::<$payload>(_out_dir, stringify!($payload))?);)? )*
            Ok(schemas)
        }
    };
}

impl TryFrom<JSONRPCRequest> for ServerRequest {
    type Error = serde_json::Error;

    fn try_from(value: JSONRPCRequest) -> Result<Self, Self::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }
}

server_request_definitions! {
    /// NEW APIs
    /// Sent when approval is requested for a specific command execution.
    /// This request is used for Turns started via turn/start.
    CommandExecutionRequestApproval => "item/commandExecution/requestApproval" {
        params: v2::CommandExecutionRequestApprovalParams,
        response: v2::CommandExecutionRequestApprovalResponse,
    },

    /// Sent when approval is requested for a specific file change.
    /// This request is used for Turns started via turn/start.
    FileChangeRequestApproval => "item/fileChange/requestApproval" {
        params: v2::FileChangeRequestApprovalParams,
        response: v2::FileChangeRequestApprovalResponse,
    },

    /// EXPERIMENTAL - Request input from the user for a tool call.
    ToolRequestUserInput => "item/tool/requestUserInput" {
        params: v2::ToolRequestUserInputParams,
        response: v2::ToolRequestUserInputResponse,
    },

    /// Request input for an MCP server elicitation.
    McpServerElicitationRequest => "mcpServer/elicitation/request" {
        params: v2::McpServerElicitationRequestParams,
        response: v2::McpServerElicitationRequestResponse,
    },

    /// Request approval for additional permissions from the user.
    PermissionsRequestApproval => "item/permissions/requestApproval" {
        params: v2::PermissionsRequestApprovalParams,
        response: v2::PermissionsRequestApprovalResponse,
    },

    /// Execute a dynamic tool call on the client.
    DynamicToolCall => "item/tool/call" {
        params: v2::DynamicToolCallParams,
        response: v2::DynamicToolCallResponse,
    },

    ChatgptAuthTokensRefresh => "account/chatgptAuthTokens/refresh" {
        params: v2::ChatgptAuthTokensRefreshParams,
        response: v2::ChatgptAuthTokensRefreshResponse,
    },

    /// Generate a fresh upstream attestation result on demand.
    AttestationGenerate => "attestation/generate" {
        params: v2::AttestationGenerateParams,
        response: v2::AttestationGenerateResponse,
    },

    #[experimental("currentTime/read")]
    /// Read the current time from an external clock owned by the client.
    CurrentTimeRead => "currentTime/read" {
        params: v2::CurrentTimeReadParams,
        response: v2::CurrentTimeReadResponse,
    },

    /// DEPRECATED APIs below
    /// Request to approve a patch.
    /// This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).
    ApplyPatchApproval {
        params: v1::ApplyPatchApprovalParams,
        response: v1::ApplyPatchApprovalResponse,
    },
    /// Request to exec a command.
    /// This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).
    ExecCommandApproval {
        params: v1::ExecCommandApprovalParams,
        response: v1::ExecCommandApprovalResponse,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchParams {
    pub query: String,
    pub roots: Vec<String>,
    // if provided, will cancel any previous request that used the same value
    pub cancellation_token: Option<String>,
}

/// Superset of [`codex_file_search::FileMatch`]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct FuzzyFileSearchResult {
    pub root: String,
    pub path: String,
    pub match_type: FuzzyFileSearchMatchType,
    pub file_name: String,
    pub score: u32,
    pub indices: Option<Vec<u32>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FuzzyFileSearchMatchType {
    File,
    Directory,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct FuzzyFileSearchResponse {
    pub files: Vec<FuzzyFileSearchResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionStartParams {
    pub session_id: String,
    pub roots: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, Default)]
pub struct FuzzyFileSearchSessionStartResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionUpdateParams {
    pub session_id: String,
    pub query: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, Default)]
pub struct FuzzyFileSearchSessionUpdateResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionStopParams {
    pub session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, Default)]
pub struct FuzzyFileSearchSessionStopResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionUpdatedNotification {
    pub session_id: String,
    pub query: String,
    pub files: Vec<FuzzyFileSearchResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionCompletedNotification {
    pub session_id: String,
}

server_notification_definitions! {
    /// NEW NOTIFICATIONS
    Error => "error" (v2::ErrorNotification),
    ThreadStarted => "thread/started" (v2::ThreadStartedNotification),
    ThreadStatusChanged => "thread/status/changed" (v2::ThreadStatusChangedNotification),
    #[experimental("thread/readState/changed")]
    ThreadReadStateChanged => "thread/readState/changed" (v2::ThreadReadStateChangedNotification),
    ThreadArchived => "thread/archived" (v2::ThreadArchivedNotification),
    ThreadDeleted => "thread/deleted" (v2::ThreadDeletedNotification),
    ThreadUnarchived => "thread/unarchived" (v2::ThreadUnarchivedNotification),
    ThreadClosed => "thread/closed" (v2::ThreadClosedNotification),
    ThreadReverted => "thread/reverted" (v2::ThreadRevertedNotification),
    SkillsChanged => "skills/changed" (v2::SkillsChangedNotification),
    ThreadNameUpdated => "thread/name/updated" (v2::ThreadNameUpdatedNotification),
    ThreadSectionChanged => "thread/section/changed" (v2::ThreadSectionChangedNotification),
    ThreadAttachmentUpdated => "thread/attachment/updated" (v2::ThreadAttachmentUpdatedNotification),
    ThreadGoalUpdated => "thread/goal/updated" (v2::ThreadGoalUpdatedNotification),
    #[experimental("thread/prediction/updated")]
    ThreadPredictionUpdated => "thread/prediction/updated" (v2::ThreadPredictionUpdatedNotification),
    ThreadGoalCleared => "thread/goal/cleared" (v2::ThreadGoalClearedNotification),
    #[experimental("thread/queue/changed")]
    ThreadQueueChanged => "thread/queue/changed" (v2::ThreadQueueChangedNotification),
    #[experimental("project/changed")]
    ProjectChanged => "project/changed" (v2::ProjectChangedNotification),
    #[experimental("thread/project/updated")]
    ThreadProjectUpdated => "thread/project/updated" (v2::ThreadProjectUpdatedNotification),
    #[experimental("thread/environment/connected")]
    EnvironmentConnected => "thread/environment/connected" (v2::EnvironmentConnectionNotification),
    #[experimental("thread/environment/disconnected")]
    EnvironmentDisconnected => "thread/environment/disconnected" (v2::EnvironmentConnectionNotification),
    #[experimental("thread/settings/updated")]
    ThreadSettingsUpdated => "thread/settings/updated" (v2::ThreadSettingsUpdatedNotification),
    ThreadTokenUsageUpdated => "thread/tokenUsage/updated" (v2::ThreadTokenUsageUpdatedNotification),
    TurnStarted => "turn/started" (v2::TurnStartedNotification),
    HookStarted => "hook/started" (v2::HookStartedNotification),
    TurnCompleted => "turn/completed" (v2::TurnCompletedNotification),
    HookCompleted => "hook/completed" (v2::HookCompletedNotification),
    TurnDiffUpdated => "turn/diff/updated" (v2::TurnDiffUpdatedNotification),
    TurnPlanUpdated => "turn/plan/updated" (v2::TurnPlanUpdatedNotification),
    ItemStarted => "item/started" (v2::ItemStartedNotification),
    ItemGuardianApprovalReviewStarted => "item/autoApprovalReview/started" (v2::ItemGuardianApprovalReviewStartedNotification),
    ItemGuardianApprovalReviewCompleted => "item/autoApprovalReview/completed" (v2::ItemGuardianApprovalReviewCompletedNotification),
    #[experimental("autoApprovalReview/strictReviewRequired")]
    StrictReviewRequired => "autoApprovalReview/strictReviewRequired" (v2::StrictReviewRequiredNotification),
    ItemCompleted => "item/completed" (v2::ItemCompletedNotification),
    /// This event is internal-only. Used by Codex Cloud.
    RawResponseItemCompleted => "rawResponseItem/completed" (v2::RawResponseItemCompletedNotification),
    /// This event is internal-only. Used by clients that need exact upstream usage.
    RawResponseCompleted => "rawResponse/completed" (v2::RawResponseCompletedNotification),
    AgentMessageDelta => "item/agentMessage/delta" (v2::AgentMessageDeltaNotification),
    /// EXPERIMENTAL - proposed plan streaming deltas for plan items.
    PlanDelta => "item/plan/delta" (v2::PlanDeltaNotification),
    /// Stream base64-encoded stdout/stderr chunks for a running `command/exec` session.
    CommandExecOutputDelta => "command/exec/outputDelta" (v2::CommandExecOutputDeltaNotification),
    /// Stream base64-encoded stdout/stderr chunks for a running `process/spawn` session.
    #[experimental("process/outputDelta")]
    ProcessOutputDelta => "process/outputDelta" (v2::ProcessOutputDeltaNotification),
    /// Final exit notification for a `process/spawn` session.
    #[experimental("process/exited")]
    ProcessExited => "process/exited" (v2::ProcessExitedNotification),
    CommandExecutionOutputDelta => "item/commandExecution/outputDelta" (v2::CommandExecutionOutputDeltaNotification),
    TerminalInteraction => "item/commandExecution/terminalInteraction" (v2::TerminalInteractionNotification),
    /// Deprecated legacy apply_patch output stream notification.
    FileChangeOutputDelta => "item/fileChange/outputDelta" (v2::FileChangeOutputDeltaNotification),
    FileChangePatchUpdated => "item/fileChange/patchUpdated" (v2::FileChangePatchUpdatedNotification),
    ServerRequestResolved => "serverRequest/resolved" (v2::ServerRequestResolvedNotification),
    McpToolCallProgress => "item/mcpToolCall/progress" (v2::McpToolCallProgressNotification),
    McpServerOauthLoginCompleted => "mcpServer/oauthLogin/completed" (v2::McpServerOauthLoginCompletedNotification),
    McpServerStatusUpdated => "mcpServer/startupStatus/updated" (v2::McpServerStatusUpdatedNotification),
    #[experimental("mcpServer/event/stream/notification")]
    McpServerEventStream => "mcpServer/event/stream/notification" (v2::McpServerEventStreamNotification),
    AccountUpdated => "account/updated" (v2::AccountUpdatedNotification),
    GatewayOAuthChanged => "account/gatewayOAuth/changed" (v2::GatewayOAuthChangedNotification),
    AccountRateLimitsUpdated => "account/rateLimits/updated" (v2::AccountRateLimitsUpdatedNotification),
    AppListUpdated => "app/list/updated" (v2::AppListUpdatedNotification),
    RemoteControlStatusChanged => "remoteControl/status/changed" (v2::RemoteControlStatusChangedNotification),
    ExternalAgentConfigImportProgress => "externalAgentConfig/import/progress" (v2::ExternalAgentConfigImportProgressNotification),
    ExternalAgentConfigImportCompleted => "externalAgentConfig/import/completed" (v2::ExternalAgentConfigImportCompletedNotification),
    FsChanged => "fs/changed" (v2::FsChangedNotification),
    ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta" (v2::ReasoningSummaryTextDeltaNotification),
    ReasoningSummaryPartAdded => "item/reasoning/summaryPartAdded" (v2::ReasoningSummaryPartAddedNotification),
    ReasoningTextDelta => "item/reasoning/textDelta" (v2::ReasoningTextDeltaNotification),
    /// Deprecated: Use `ContextCompaction` item type instead.
    ContextCompacted => "thread/compacted" (v2::ContextCompactedNotification),
    ModelRerouted => "model/rerouted" (v2::ModelReroutedNotification),
    ModelVerification => "model/verification" (v2::ModelVerificationNotification),
    AuthRecoveryStarted => "modelProvider/authRecoveryStarted" (v2::AuthRecoveryNotification),
    AuthRecoveryCompleted => "modelProvider/authRecoveryCompleted" (v2::AuthRecoveryNotification),
    #[experimental("turn/moderationMetadata")]
    TurnModerationMetadata => "turn/moderationMetadata" (v2::TurnModerationMetadataNotification),
    ModelSafetyBufferingUpdated => "model/safetyBuffering/updated" (v2::ModelSafetyBufferingUpdatedNotification),
    Warning => "warning" (v2::WarningNotification),
    GuardianWarning => "guardianWarning" (v2::GuardianWarningNotification),
    DeprecationNotice => "deprecationNotice" (v2::DeprecationNoticeNotification),
    ConfigWarning => "configWarning" (v2::ConfigWarningNotification),
    FuzzyFileSearchSessionUpdated => "fuzzyFileSearch/sessionUpdated" (FuzzyFileSearchSessionUpdatedNotification),
    FuzzyFileSearchSessionCompleted => "fuzzyFileSearch/sessionCompleted" (FuzzyFileSearchSessionCompletedNotification),
    #[experimental("thread/realtime/started")]
    ThreadRealtimeStarted => "thread/realtime/started" (v2::ThreadRealtimeStartedNotification),
    #[experimental("thread/realtime/itemAdded")]
    ThreadRealtimeItemAdded => "thread/realtime/itemAdded" (v2::ThreadRealtimeItemAddedNotification),
    #[experimental("thread/realtime/item/started")]
    ThreadRealtimeItemStarted => "thread/realtime/item/started" (v2::ThreadRealtimeItemStartedNotification),
    #[experimental("thread/realtime/item/transcript/delta")]
    ThreadRealtimeItemTranscriptDelta => "thread/realtime/item/transcript/delta" (v2::ThreadRealtimeItemTranscriptDeltaNotification),
    #[experimental("thread/realtime/item/completed")]
    ThreadRealtimeItemCompleted => "thread/realtime/item/completed" (v2::ThreadRealtimeItemCompletedNotification),
    #[experimental("thread/realtime/transcript/delta")]
    ThreadRealtimeTranscriptDelta => "thread/realtime/transcript/delta" (v2::ThreadRealtimeTranscriptDeltaNotification),
    #[experimental("thread/realtime/transcript/done")]
    ThreadRealtimeTranscriptDone => "thread/realtime/transcript/done" (v2::ThreadRealtimeTranscriptDoneNotification),
    #[experimental("thread/realtime/outputAudio/delta")]
    ThreadRealtimeOutputAudioDelta => "thread/realtime/outputAudio/delta" (v2::ThreadRealtimeOutputAudioDeltaNotification),
    #[experimental("thread/realtime/sdp")]
    ThreadRealtimeSdp => "thread/realtime/sdp" (v2::ThreadRealtimeSdpNotification),
    #[experimental("thread/realtime/error")]
    ThreadRealtimeError => "thread/realtime/error" (v2::ThreadRealtimeErrorNotification),
    #[experimental("thread/realtime/closed")]
    ThreadRealtimeClosed => "thread/realtime/closed" (v2::ThreadRealtimeClosedNotification),

    /// Notifies the user of world-writable directories on Windows, which cannot be protected by the sandbox.
    WindowsWorldWritableWarning => "windows/worldWritableWarning" (v2::WindowsWorldWritableWarningNotification),
    WindowsSandboxSetupCompleted => "windowsSandbox/setupCompleted" (v2::WindowsSandboxSetupCompletedNotification),

    #[serde(rename = "account/login/completed")]
    #[ts(rename = "account/login/completed")]
    #[strum(serialize = "account/login/completed")]
    AccountLoginCompleted(v2::AccountLoginCompletedNotification),

}

/// Server notification envelope sent over app-server transports.
///
/// `emitted_at_ms` records when app-server emitted the notification, before it
/// is fanned out to individual connections.
#[derive(Serialize, Deserialize, Debug, Clone, TS)]
#[serde(rename_all = "camelCase")]
pub struct ServerNotificationEnvelope {
    #[serde(flatten)]
    pub notification: ServerNotification,
    /// Unix timestamp (in milliseconds) when app-server emitted this notification.
    ///
    /// Optional so clients can decode notifications from older app-server
    /// versions. Current app-server versions always populate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub emitted_at_ms: Option<i64>,
}

client_notification_definitions! {
    Initialized,
}

#[cfg(test)]
#[path = "common_unit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "common_tests.rs"]
mod common_tests;
