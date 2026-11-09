//! Applies the selected server's socket policy on initial connections and reconnects.

use crate::AppServerTarget;
#[cfg(windows)]
use crate::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use crate::connect_remote_app_server;
use codex_app_server_client::AppServerClient;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerClient;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerConnectArgs;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerEndpoint;
#[cfg(windows)]
use codex_utils_absolute_path::AbsolutePathBuf;
use std::time::Duration;
use std::time::Instant;

/// A longer gap means an interactive screen ran in between, during which the daemon may
/// have restarted; redialing then costs little compared with the wait.
const PREPARED_CONNECTION_MAX_AGE: Duration = Duration::from_secs(2);

/// An initialized connection that startup checks already opened, kept for the first
/// session connect so it does not repeat the socket and `initialize` handshakes.
pub(crate) struct PreparedConnection {
    target: AppServerTarget,
    // Boxed to keep the startup future and `Compatibility` variants small.
    client: Box<AppServerClient>,
    opened_at: Instant,
}

impl PreparedConnection {
    /// `client` must come from [`connect`] for `target`, so it matches a fresh connection.
    pub(crate) fn new(target: AppServerTarget, client: AppServerClient) -> Self {
        Self {
            target,
            client: Box::new(client),
            opened_at: Instant::now(),
        }
    }

    /// Returns the client if it was opened recently for `target`; otherwise closes it.
    pub(crate) async fn take_for(self, target: &AppServerTarget) -> Option<AppServerClient> {
        self.take_for_at(target, Instant::now()).await
    }

    pub(crate) async fn take_for_at(
        self,
        target: &AppServerTarget,
        now: Instant,
    ) -> Option<AppServerClient> {
        if self.target == *target
            && now.saturating_duration_since(self.opened_at) <= PREPARED_CONNECTION_MAX_AGE
        {
            return Some(*self.client);
        }
        let _ = self.client.shutdown().await;
        None
    }
}

impl std::fmt::Debug for PreparedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedConnection")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn connect(target: &AppServerTarget) -> color_eyre::Result<AppServerClient> {
    match target {
        AppServerTarget::Cloud => {
            color_eyre::eyre::bail!("Reopen Codex to reconnect the cloud backend.");
        }
        AppServerTarget::Embedded => {
            color_eyre::eyre::bail!("embedded sessions have no remote connection")
        }
        #[cfg(windows)]
        AppServerTarget::LocalDaemon {
            endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
            ..
        } => {
            // Revalidate at the real connection, not just the earlier discovery probe.
            let (socket_path, _directory) =
                codex_uds::validate_private_socket_path(socket_path.as_path())?;
            let app_server =
                RemoteAppServerClient::connect_local_daemon(RemoteAppServerConnectArgs {
                    required_human_approval: true,
                    endpoint: RemoteAppServerEndpoint::UnixSocket {
                        socket_path: AbsolutePathBuf::from_absolute_path_checked(socket_path)?,
                    },
                    client_name: "codex-tui".to_string(),
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                    experimental_api: true,
                    mcp_server_openai_form_elicitation: false,
                    opt_out_notification_methods: Vec::new(),
                    channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
                })
                .await?;
            Ok(AppServerClient::Remote(app_server))
        }
        AppServerTarget::LocalDaemon { endpoint, .. } | AppServerTarget::Remote { endpoint } => {
            connect_remote_app_server(endpoint.clone()).await
        }
    }
}
