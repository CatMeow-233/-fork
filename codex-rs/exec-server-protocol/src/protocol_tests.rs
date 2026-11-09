use super::CapabilityRootDiscoverRequest;
use super::WireExecParams;
#[test]
fn discovery_v2_support_defaults_off_for_older_executors() -> serde_json::Result<()> {
    let legacy: super::EnvironmentCapabilities = serde_json::from_value(serde_json::json!({}))?;
    assert!(!legacy.capability_discovery_v2);
    assert!(!legacy.fs_watch_missing);
    let capabilities = super::EnvironmentInfo::local().capabilities;
    assert_eq!(
        serde_json::from_value::<super::EnvironmentCapabilities>(serde_json::to_value(
            &capabilities
        )?)?,
        capabilities
    );
    Ok(())
}

use super::EnvironmentCapabilities;
use super::EnvironmentInfo;
use super::ExecExitedNotification;
use super::ExecMetadata;
use super::ExecParams;
use super::ExecResponse;
use super::FsOpenMode;
use super::FsOpenParams;
use super::FsReadFileParams;
use super::HttpRequestParams;
use super::NetworkProxyCapabilities;
use super::ProcessId;
use super::ProcessSandboxType;
use super::ShellInfo;
use super::WireFsOpenParams;
use super::WireFsReadFileParams;
use codex_file_system::FileSystemSandboxContext;
use codex_file_system::WindowsSandboxSelection;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_network_proxy::NetworkProxyAuditMetadata;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::RemoteCredentialMasking;
use codex_network_proxy::RemoteNetworkProxyConfig;
use codex_network_proxy::RemoteNetworkProxyLaunchConfig;
use codex_protocol::config_types::WindowsSandboxProxySettingsMode;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::sandbox::SandboxOverride;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

#[test]
fn shared_byte_chunks_preserve_base64_and_owned_bytes() {
    let chunk = super::ByteChunk::from(vec![0, 255, 10]);
    let shared = chunk.clone();
    assert_eq!(serde_json::to_string(&shared).unwrap(), r#""AP8K""#);
    assert_eq!(
        serde_json::from_str::<super::ByteChunk>(r#""AP8K""#).unwrap(),
        chunk,
    );

    let mut owned = shared.into_inner();
    owned[0] = 1;
    assert_eq!(
        (owned, chunk.into_inner()),
        (vec![1, 255, 10], vec![0, 255, 10])
    );
}

#[test]
fn exec_params_keeps_proxy_launch_separate_from_sandbox_facts() {
    let cwd = PathUri::from_host_native_path(std::env::current_dir().expect("current directory"))
        .expect("cwd URI");
    let params = ExecParams {
        process_id: ProcessId::from("managed-network"),
        metadata: Some(ExecMetadata {
            thread_id: Some(codex_protocol::ThreadId::new()),
            tool_call_id: Some("call-1".to_string()),
        }),
        argv: vec!["true".to_string()],
        cwd,
        env_policy: None,
        shell_snapshot: None,
        env: HashMap::new(),
        tty: false,
        pipe_stdin: false,
        arg0: None,
        sandbox: None,
        enforce_managed_network: true,
        managed_network: Some(ManagedNetworkSandboxContext {
            loopback_ports: vec![43123, 48081],
            allow_local_binding: false,
            allow_unix_sockets: vec!["/tmp/allowed.sock".to_string()],
            dangerously_allow_all_unix_sockets: true,
        }),
        network_proxy: Some(
            RemoteNetworkProxyLaunchConfig::new(
                RemoteNetworkProxyConfig::from_effective_config(&NetworkProxyConfig::default())
                    .expect("supported remote config"),
            )
            .with_audit_metadata(NetworkProxyAuditMetadata {
                conversation_id: Some("conversation-1".to_string()),
                ..NetworkProxyAuditMetadata::default()
            })
            .for_execution("remote".to_string(), "execution-1".to_string()),
        ),
    };

    let mut serialized = serde_json::to_value(&params).expect("serialize exec params");
    assert_eq!(
        (
            serialized.get("threadId").cloned(),
            serialized.get("toolCallId").cloned(),
            serialized.get("metadata").cloned(),
        ),
        (None, None, Some(serde_json::json!(params.metadata)),)
    );
    assert_eq!(
        serialized["managedNetwork"],
        serde_json::json!({
            "loopbackPorts": [43123, 48081],
            "allowLocalBinding": false,
            "allowUnixSockets": ["/tmp/allowed.sock"],
            "dangerouslyAllowAllUnixSockets": true,
        })
    );
    assert_eq!(
        serialized["networkProxy"]["auditMetadata"]["conversationId"],
        "conversation-1"
    );
    let round_trip: ExecParams =
        serde_json::from_value(serialized.clone()).expect("deserialize exec params");
    assert_eq!(round_trip, params);

    serialized
        .as_object_mut()
        .expect("exec params object")
        .remove("managedNetwork");
    serialized
        .as_object_mut()
        .expect("exec params object")
        .remove("networkProxy");
    serialized.as_object_mut().unwrap().remove("metadata");
    let legacy: ExecParams =
        serde_json::from_value(serialized).expect("deserialize legacy exec params");
    assert!(legacy.enforce_managed_network);
    assert_eq!(legacy.managed_network, None);
    assert_eq!(legacy.network_proxy, None);
    assert_eq!(legacy.metadata, None);
    let legacy_serialized =
        serde_json::to_value(&legacy).expect("serialize exec params without proxy launch");
    assert!(legacy_serialized.get("networkProxy").is_none());
    assert!(legacy_serialized.get("threadId").is_none());
    assert!(legacy_serialized.get("toolCallId").is_none());
    assert!(legacy_serialized.get("metadata").is_none());

    for sandbox_override in [
        SandboxOverride::NoOverride,
        SandboxOverride::EscalatedSandboxWithRestrictions,
        SandboxOverride::BypassSandboxFirstAttempt,
    ] {
        let mut params = params.clone();
        let mut sandbox = FileSystemSandboxContext::from_permission_profile(
            PermissionProfile::read_only(),
            params.cwd.clone(),
        );
        sandbox.sandbox_override = sandbox_override;
        params.sandbox = Some(sandbox);
        let mut json = serde_json::to_value(&params).expect("serialize sandboxed exec");
        assert_eq!(
            json["sandbox"].get("sandboxOverride").is_none(),
            sandbox_override.is_no_override(),
        );
        let wire: WireExecParams = serde_json::from_value(json.clone()).expect("wire exec");
        assert_eq!(ExecParams::from(wire), params);

        json["sandbox"]
            .as_object_mut()
            .unwrap()
            .remove("sandboxOverride");
        params.sandbox.as_mut().unwrap().sandbox_override = SandboxOverride::NoOverride;
        let legacy: WireExecParams = serde_json::from_value(json).expect("legacy wire exec");
        assert_eq!(ExecParams::from(legacy), params);
    }
}

#[test]
fn exec_params_defaults_legacy_managed_network_unix_socket_policy() {
    let cwd = PathUri::from_host_native_path(std::env::current_dir().expect("current directory"))
        .expect("cwd URI");
    let legacy: ExecParams = serde_json::from_value(serde_json::json!({
        "processId": "legacy-managed-network",
        "argv": ["true"],
        "cwd": cwd,
        "env": {},
        "tty": false,
        "arg0": null,
        "enforceManagedNetwork": true,
        "managedNetwork": {
            "loopbackPorts": [43123],
            "allowLocalBinding": true,
        },
    }))
    .expect("deserialize legacy managed network context");

    assert_eq!(
        legacy,
        ExecParams {
            process_id: ProcessId::from("legacy-managed-network"),
            metadata: None,
            argv: vec!["true".to_string()],
            cwd,
            env_policy: None,
            shell_snapshot: None,
            env: HashMap::new(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: true,
            managed_network: Some(ManagedNetworkSandboxContext {
                loopback_ports: vec![43123],
                allow_local_binding: true,
                allow_unix_sockets: Vec::new(),
                dangerously_allow_all_unix_sockets: false,
            }),
            network_proxy: None,
        }
    );
}

#[test]
fn environment_info_accepts_legacy_response_without_cwd() {
    let info: EnvironmentInfo = serde_json::from_value(serde_json::json!({
        "shell": { "name": "zsh", "path": "/bin/zsh" }
    }))
    .expect("legacy environment info should deserialize");

    assert_eq!(
        info,
        EnvironmentInfo {
            shell: ShellInfo {
                name: "zsh".to_string(),
                path: "/bin/zsh".to_string(),
            },
            executor_version: "0.0.0".to_string(),
            provider_id: None,
            cwd: None,
            user_home_dir: None,
            platform_os: None,
            prepend_path_dirs: Vec::new(),
            temporary_directories: None,
            temp_dir: None,
            capabilities: EnvironmentCapabilities::default(),
        }
    );
}

#[test]
fn environment_capabilities_accept_legacy_response_without_environment_config_read() {
    let capabilities: EnvironmentCapabilities = serde_json::from_value(serde_json::json!({
        "networkProxyLaunch": true,
        "capabilityDiscoverySandbox": true,
    }))
    .expect("legacy environment capabilities should deserialize");

    assert_eq!(
        capabilities,
        EnvironmentCapabilities {
            network_proxy_launch: true,
            network_proxy_review_domains: false,
            credential_masking: false,
            credential_masking_custom_providers: false,
            capability_discovery_sandbox: true,
            fs_watch_missing: false,
            capability_discovery_v2: false,
            environment_config_read: false,
            http_header_env_vars: false,
            sandboxed_file_streaming: false,
            file_write_streaming: false,
            shell_snapshot_v2: false,
            windows_mxc: false,
            linux_root_write_preserves_devices: false,
            linux_approved_root_write_preserves_restrictions: false,
        }
    );

    for (input_masking, expected_masking, output_masking) in [
        (
            serde_json::json!({}),
            RemoteCredentialMasking::Disabled,
            serde_json::json!({}),
        ),
        (
            serde_json::json!({ "credentialMasking": true }),
            RemoteCredentialMasking::BuiltInOnly,
            serde_json::json!({ "credentialMasking": true }),
        ),
        (
            serde_json::json!({
                "credentialMasking": true,
                "credentialMaskingCustomProviders": false,
            }),
            RemoteCredentialMasking::BuiltInOnly,
            serde_json::json!({ "credentialMasking": true }),
        ),
        (
            serde_json::json!({
                "credentialMasking": true,
                "credentialMaskingCustomProviders": true,
            }),
            RemoteCredentialMasking::WithConfiguredProviders,
            serde_json::json!({
                "credentialMasking": true,
                "credentialMaskingCustomProviders": true,
            }),
        ),
        (
            serde_json::json!({ "credentialMaskingCustomProviders": true }),
            RemoteCredentialMasking::Disabled,
            serde_json::json!({ "credentialMaskingCustomProviders": true }),
        ),
        (
            serde_json::json!({
                "credentialMasking": false,
                "credentialMaskingCustomProviders": true,
            }),
            RemoteCredentialMasking::Disabled,
            serde_json::json!({ "credentialMaskingCustomProviders": true }),
        ),
    ] {
        for launch in [None, Some(false), Some(true)] {
            let mut input = serde_json::json!({
                "capabilityDiscoverySandbox": true,
            });
            if let Some(launch) = launch {
                input["networkProxyLaunch"] = launch.into();
            }
            input
                .as_object_mut()
                .unwrap()
                .extend(input_masking.as_object().unwrap().clone());
            let decoded: EnvironmentCapabilities = serde_json::from_value(input).unwrap();
            assert_eq!(
                decoded.network_proxy(),
                NetworkProxyCapabilities {
                    launch: launch.unwrap_or_default(),
                    credential_masking: expected_masking,
                }
            );
            let mut expected_wire = serde_json::json!({
                "networkProxyLaunch": launch.unwrap_or_default(),
                "capabilityDiscoverySandbox": true,
                "capabilityDiscoveryV2": false,
                "environmentConfigRead": false,
                "httpHeaderEnvVars": false,
                "sandboxedFileStreaming": false,
                "fileWriteStreaming": false,
                "shellSnapshotV2": false,
                "windowsMxc": false,
            });
            expected_wire
                .as_object_mut()
                .unwrap()
                .extend(output_masking.as_object().unwrap().clone());
            assert_eq!(serde_json::to_value(decoded).unwrap(), expected_wire);
        }
    }
}

#[test]
fn linux_approved_root_write_support_is_opt_in_on_the_wire() {
    let mut capabilities = EnvironmentCapabilities::default();
    let legacy = serde_json::to_value(&capabilities).unwrap();
    assert!(legacy.get("linuxRootWritePreservesDevices").is_none());
    assert!(
        legacy
            .get("linuxApprovedRootWritePreservesRestrictions")
            .is_none()
    );
    capabilities.linux_root_write_preserves_devices = true;
    let device_only = serde_json::from_value::<EnvironmentCapabilities>(
        serde_json::to_value(&capabilities).unwrap(),
    )
    .unwrap();
    assert!(!device_only.linux_approved_root_write_preserves_restrictions);
    capabilities.linux_approved_root_write_preserves_restrictions = true;
    assert_eq!(
        serde_json::from_value::<EnvironmentCapabilities>(
            serde_json::to_value(&capabilities).unwrap()
        )
        .unwrap(),
        capabilities
    );
    let local = EnvironmentInfo::local().capabilities;
    assert_eq!(
        (
            local.linux_root_write_preserves_devices,
            local.linux_approved_root_write_preserves_restrictions,
        ),
        (cfg!(target_os = "linux"), cfg!(target_os = "linux"))
    );
}

#[test]
fn environment_info_preserves_executor_metadata() {
    let expected = serde_json::json!({
        "shell": { "name": "powershell", "path": "powershell.exe" },
        "executorVersion": "1.2.3-alpha.4",
        "providerId": "sha256:e0a0cebe63ab8189ffe3eed378ccf6aa89ef15bc75e39dbbf1fc55951ec6888b",
        "cwd": null,
        "userHomeDir": "file:///C:/Users/remote",
        "platformOs": "windows",
        "prependPathDirs": ["file:///C:/tools/bin", "file:///D:/tools/bin"],
        "temporaryDirectories": ["file:///C:/Temp", "file:///D:/Temp"],
        "capabilities": {
            "networkProxyLaunch": false,
            "capabilityDiscoverySandbox": false,
            "capabilityDiscoveryV2": false,
            "environmentConfigRead": false,
            "httpHeaderEnvVars": false,
            "sandboxedFileStreaming": false,
            "fileWriteStreaming": false,
            "shellSnapshotV2": false,
            "windowsMxc": false,
        },
    });
    let info: EnvironmentInfo = serde_json::from_value(expected.clone())
        .expect("environment info with executor metadata should deserialize");

    assert_eq!(
        serde_json::to_value(info).expect("environment info should serialize"),
        expected,
    );
}

#[test]
fn local_environment_info_reads_platform_temporary_directories() {
    let cwd = std::env::current_dir().expect("current directory");
    let names: &[&str] = if cfg!(windows) {
        &["TEMP", "TMP"]
    } else {
        &["TMPDIR"]
    };
    let mut expected = names
        .iter()
        .filter_map(std::env::var_os)
        .filter(|path| !path.is_empty())
        .filter(|path| cfg!(unix) || std::path::Path::new(path).is_absolute())
        .filter_map(|path| {
            PathUri::from_host_native_path(&path).ok().or_else(|| {
                if cfg!(unix) {
                    PathUri::from_host_native_path(cwd.join(path)).ok()
                } else {
                    None
                }
            })
        })
        .collect::<Vec<_>>();
    expected.dedup();

    let info = EnvironmentInfo::local();
    assert_eq!(info.temporary_directories, Some(expected));
    assert_eq!(info.user_home_dir, PathUri::from_host_native_path("~").ok());
}

#[cfg(unix)]
#[test]
fn local_environment_info_resolves_relative_temporary_directory() {
    if std::env::var_os("CODEX_TEST_RELATIVE_TMPDIR").is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--exact")
            .arg("protocol::tests::local_environment_info_resolves_relative_temporary_directory")
            .env("CODEX_TEST_RELATIVE_TMPDIR", "1")
            .env("TMPDIR", "relative-temp")
            .status()
            .expect("run relative TMPDIR subprocess");
        assert!(status.success(), "relative TMPDIR subprocess failed");
        return;
    }

    let expected = PathUri::from_host_native_path(
        std::env::current_dir()
            .expect("current directory")
            .join("relative-temp"),
    )
    .expect("absolute temporary directory URI");
    let info = EnvironmentInfo::local();
    assert_eq!(info.temporary_directories, Some(vec![expected.clone()]));
    assert_eq!(info.temp_dir, Some(expected));
}

#[test]
fn filesystem_protocol_rejects_native_absolute_paths() {
    let native_path = std::env::current_dir()
        .expect("current directory")
        .join("native-file.txt");
    let native_cwd = std::env::current_dir().expect("current directory");

    serde_json::from_value::<FsReadFileParams>(serde_json::json!({
        "path": native_path.to_string_lossy(),
        "sandbox": null,
    }))
    .expect_err("native absolute path should not deserialize as a URI");

    let sandbox = FileSystemSandboxContext::from_permission_profile(
        PermissionProfile::default(),
        PathUri::from_host_native_path(&native_cwd).expect("cwd URI"),
    );
    let mut native_path_sandbox = serde_json::to_value(sandbox).expect("sandbox should serialize");
    native_path_sandbox["cwd"] = serde_json::json!(native_cwd.to_string_lossy());

    serde_json::from_value::<FsReadFileParams>(serde_json::json!({
        "path": PathUri::from_host_native_path(native_path)
            .expect("path URI")
            .to_string(),
        "sandbox": native_path_sandbox,
    }))
    .expect_err("native absolute sandbox cwd should not deserialize as a URI");
}

#[test]
fn filesystem_protocol_round_trips_permission_entries() {
    let native_cwd = std::env::current_dir().expect("current directory");
    let cwd = PathUri::from_host_native_path(&native_cwd).expect("cwd URI");
    let file_system = ManagedFileSystemPermissions::Restricted {
        entries: vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: cwd.clone() },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry::skip_missing_path(
                FileSystemPath::Path {
                    path: PathUri::from_host_native_path(native_cwd.join(".git"))
                        .expect("absolute path"),
                },
                FileSystemAccessMode::Read,
            ),
            FileSystemSandboxEntry::skip_missing_path(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::ProjectRoots {
                        subpath: Some(".codex".into()),
                    },
                },
                FileSystemAccessMode::Read,
            ),
        ],
        glob_scan_max_depth: Some(2.try_into().expect("non-zero depth")),
    };
    let permissions = PermissionProfile::Managed {
        file_system,
        network: NetworkSandboxPolicy::Restricted,
    };
    let mut sandbox = FileSystemSandboxContext::from_permission_profile(permissions, cwd.clone());
    sandbox.user_home_dir = Some(cwd.clone());
    sandbox.windows_sandbox_selection = WindowsSandboxSelection::Mxc;

    let serialized = serde_json::to_value(&sandbox).expect("serialize sandbox");

    assert_eq!(serialized["windowsSandboxLevel"], "mxc");
    assert_eq!(serialized["cwd"], serde_json::json!(cwd.to_string()));
    assert_eq!(
        serialized["userHomeDir"],
        serde_json::json!(cwd.to_string())
    );
    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][0]["path"]["path"],
        serde_json::json!(cwd.to_string())
    );
    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][1]["path"]["type"],
        serde_json::json!("path")
    );
    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][1]["missing_path_behavior"],
        serde_json::json!("skip")
    );
    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][2]["path"]["type"],
        serde_json::json!("special")
    );
    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][2]["missing_path_behavior"],
        serde_json::json!("skip")
    );
    assert!(!serialized.to_string().contains("generated_default_path"));
    assert!(!serialized.to_string().contains("generated_default_special"));
    assert_eq!(
        serde_json::from_value::<FileSystemSandboxContext>(serialized)
            .expect("deserialize sandbox"),
        sandbox
    );
    let preserve = FileSystemSandboxContext {
        windows_sandbox_proxy_settings_mode: Some(WindowsSandboxProxySettingsMode::Preserve),
        ..sandbox
    };
    let serialized = serde_json::to_value(&preserve).expect("serialize preserve mode");
    assert_eq!(serialized["windowsSandboxProxySettingsMode"], "preserve");
    assert_eq!(
        serde_json::from_value::<FileSystemSandboxContext>(serialized)
            .expect("deserialize preserve mode"),
        preserve
    );
}

/// The filesystem RPC preserves Windows drive and UNC permission URIs on any host.
#[test]
fn filesystem_protocol_preserves_foreign_permission_path_json() {
    let cwd = PathUri::parse("file:///C:/a%20b").expect("drive cwd URI");
    let unc = PathUri::parse("file://host/s/a%20b").expect("UNC path URI");
    let permissions = PermissionProfile::Managed {
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: [cwd.clone(), unc.clone()]
                .into_iter()
                .map(|path| FileSystemSandboxEntry::new(path.into(), FileSystemAccessMode::Read))
                .collect(),
            glob_scan_max_depth: None,
        },
        network: NetworkSandboxPolicy::Restricted,
    };
    let params = FsReadFileParams {
        path: unc,
        follow_symlinks: None,
        sandbox: Some(FileSystemSandboxContext::from_permission_profile(
            permissions,
            cwd,
        )),
    };
    let serialized = serde_json::to_value(WireFsReadFileParams::from(params.clone()))
        .expect("serialize filesystem wire request");
    assert_eq!(
        serialized["sandbox"]["permissions"],
        serde_json::json!({
            "type": "managed",
            "file_system": {
                "type": "restricted",
                "entries": [
                    {"path": {"type": "path", "path": "file:///C:/a%20b"}, "access": "read"},
                    {"path": {"type": "path", "path": "file://host/s/a%20b"}, "access": "read"}
                ]
            },
            "network": "restricted"
        }),
    );
    assert_eq!(
        serde_json::from_value::<WireFsReadFileParams>(serialized)
            .expect("deserialize filesystem wire request")
            .try_into_request(|wire| {
                let cwd = wire.cwd().expect("explicit client policy cwd").clone();
                Ok(wire.into_context(cwd))
            })
            .expect("resolve filesystem request"),
        params,
    );
}

/// Executor ingress must use the full policy context instead of interpreting legacy helper fields.
#[test]
fn filesystem_protocol_prefers_explicit_policy_paths_over_legacy_fields() {
    let cwd = PathUri::parse("file:///workspace/selected").expect("selected policy cwd");
    let workspace = PathUri::parse("file:///workspace/other").expect("selected workspace");
    let path = cwd.join("note.txt").expect("read path");
    let sandbox = FileSystemSandboxContext {
        sandbox_override: SandboxOverride::EscalatedSandboxWithRestrictions,
        workspace_roots: vec![workspace],
        ..FileSystemSandboxContext::from_permission_profile(PermissionProfile::Disabled, cwd)
    };
    let params = FsReadFileParams {
        path,
        follow_symlinks: None,
        sandbox: Some(sandbox),
    };
    let mut json = serde_json::to_value(WireFsReadFileParams::from(params.clone()))
        .expect("serialize wire read");
    json["sandbox"]["cwd"] = serde_json::json!("file:///legacy");
    json["sandbox"]["workspaceRoots"] = serde_json::json!(["file:///legacy"]);
    let wire: WireFsReadFileParams = serde_json::from_value(json).expect("wire read");
    let request = wire
        .try_into_request(|wire| {
            let cwd = wire.cwd().expect("explicit policy cwd").clone();
            Ok(wire.into_context(cwd))
        })
        .expect("strict executor read");
    assert_eq!(request, params);
}

/// Legacy `fs/open` callers continue to open existing files for reading only.
#[test]
fn filesystem_open_accepts_legacy_request_without_mode() {
    let wire: WireFsOpenParams = serde_json::from_value(serde_json::json!({
        "handleId": "legacy-handle",
        "path": "file:///tmp/existing.txt",
    }))
    .expect("legacy open should deserialize");
    assert_eq!(
        wire.try_into_request(|_| panic!("no sandbox to resolve"))
            .expect("legacy open should resolve"),
        FsOpenParams {
            handle_id: "legacy-handle".to_string(),
            path: PathUri::parse("file:///tmp/existing.txt").expect("file URI"),
            mode: FsOpenMode::Read,
            sandbox: None,
        }
    );
}

/// Only filesystem RPCs need the additive policy field; capability discovery keeps its original wire cwd.
#[test]
fn filesystem_open_and_capability_discovery_keep_their_existing_cwd_contracts() {
    let cwd = PathUri::parse("file:///D:/selected").expect("Windows policy cwd");
    let path = cwd.join("note.txt").expect("read path");
    let mut sandbox =
        FileSystemSandboxContext::from_permission_profile(PermissionProfile::Disabled, cwd.clone());
    sandbox.windows_sandbox_selection = WindowsSandboxSelection::Mxc;
    let open = FsOpenParams {
        handle_id: "handle".to_owned(),
        path: path.clone(),
        mode: FsOpenMode::Read,
        sandbox: Some(sandbox.clone()),
    };
    let capability = CapabilityRootDiscoverRequest {
        id: "root".to_owned(),
        path,
        sandbox: Some(sandbox),
    };
    let open_wire =
        serde_json::to_value(WireFsOpenParams::from(open.clone())).expect("serialize open");
    let capability_wire = serde_json::to_value(&capability).expect("serialize capability");
    assert_eq!(
        (
            open_wire["sandbox"].get("cwd"),
            open_wire["sandbox"].get("workspaceRoots"),
            open_wire["sandbox"].get("policyContext"),
            open_wire["sandbox"].get("windowsSandboxLevel"),
            capability_wire["sandbox"].get("cwd"),
            capability_wire["sandbox"].get("workspaceRoots"),
            capability_wire["sandbox"].get("policyContext"),
            capability_wire["sandbox"].get("windowsSandboxLevel"),
        ),
        (
            None,
            None,
            Some(&serde_json::json!({"cwd": cwd, "workspaceRoots": [cwd]})),
            Some(&serde_json::json!("mxc")),
            Some(&serde_json::json!(cwd)),
            Some(&serde_json::json!([cwd])),
            None,
            Some(&serde_json::json!("mxc")),
        )
    );
    assert_eq!(
        serde_json::from_value::<WireFsOpenParams>(open_wire)
            .expect("deserialize wire open")
            .try_into_request(|wire| {
                let cwd = wire.cwd().expect("explicit client policy cwd").clone();
                Ok(wire.into_context(cwd))
            })
            .expect("resolve open"),
        open
    );
    assert_eq!(
        serde_json::from_value::<CapabilityRootDiscoverRequest>(capability_wire)
            .expect("deserialize capability"),
        capability
    );
}

#[test]
fn filesystem_protocol_round_trips_legacy_policy_paths_as_uris() {
    let native_cwd = std::env::current_dir().expect("current directory");
    let cwd = PathUri::from_host_native_path(&native_cwd).expect("cwd URI");
    let mut file_system_policy =
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: cwd.clone() },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        }]);
    file_system_policy.glob_scan_max_depth = Some(2);
    let permissions = PermissionProfile::from_runtime_permissions(
        &file_system_policy,
        NetworkSandboxPolicy::Restricted,
    );
    let sandbox = FileSystemSandboxContext::from_permission_profile(permissions, cwd.clone());

    let serialized = serde_json::to_value(&sandbox).expect("serialize sandbox");

    assert_eq!(
        serialized["permissions"]["file_system"]["entries"][0]["path"]["path"],
        serde_json::json!(cwd.to_string())
    );
    assert_eq!(
        serde_json::from_value::<FileSystemSandboxContext>(serialized)
            .expect("deserialize sandbox"),
        sandbox
    );
}

#[test]
fn http_request_timeout_treats_omitted_and_null_as_no_timeout() {
    let omitted: HttpRequestParams = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "url": "https://example.test",
        "requestId": "req-omitted-timeout",
    }))
    .expect("omitted timeout should deserialize");
    let null_timeout: HttpRequestParams = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "url": "https://example.test",
        "requestId": "req-null-timeout",
        "timeoutMs": null,
    }))
    .expect("null timeout should deserialize");
    let explicit_timeout: HttpRequestParams = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "url": "https://example.test",
        "requestId": "req-explicit-timeout",
        "timeoutMs": 1234,
    }))
    .expect("numeric timeout should deserialize");

    assert_eq!(
        (omitted.request_id.as_str(), omitted.timeout_ms),
        ("req-omitted-timeout", None)
    );
    assert_eq!(
        (null_timeout.request_id.as_str(), null_timeout.timeout_ms),
        ("req-null-timeout", None)
    );
    assert_eq!(
        (
            explicit_timeout.request_id.as_str(),
            explicit_timeout.timeout_ms
        ),
        ("req-explicit-timeout", Some(1234))
    );
}

#[test]
fn exited_notification_accepts_legacy_payload_without_sandbox_denied() {
    let notification: ExecExitedNotification = serde_json::from_value(serde_json::json!({
        "processId": "proc-1",
        "seq": 3,
        "exitCode": 1,
    }))
    .expect("legacy exited notification should deserialize");

    assert_eq!(notification.sandbox_denied, None);
}

#[test]
fn exec_response_distinguishes_unknown_from_explicitly_unsandboxed() {
    let unknown: ExecResponse = serde_json::from_value(serde_json::json!({
        "processId": "legacy",
    }))
    .expect("legacy response should deserialize");
    let unsandboxed: ExecResponse = serde_json::from_value(serde_json::json!({
        "processId": "current",
        "sandboxType": "none",
    }))
    .expect("explicitly unsandboxed response should deserialize");

    assert_eq!(
        (unknown.sandbox_type, unsandboxed.sandbox_type),
        (None, Some(ProcessSandboxType::None))
    );
}
