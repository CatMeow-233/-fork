//! Installation check: current executable, install context, and package-manager provenance.

use super::*;

pub(super) fn installation_check(show_details: bool) -> DoctorCheck {
    let mut details = Vec::new();
    let current_exe = env::current_exe().ok();
    push_path_detail(&mut details, "current executable", current_exe.as_deref());
    let inherited_managed_env = inherited_managed_env_for_cargo_binary(current_exe.as_deref());
    let install_context = doctor_install_context(current_exe.as_deref());
    details.push(format!(
        "install context: {}",
        describe_install_context(&install_context)
    ));
    if inherited_managed_env {
        details.push(
            "ignored inherited package-manager launch env for cargo-built binary".to_string(),
        );
    }
    details.push(format!(
        "managed by npm: {}",
        doctor_managed_by_npm(current_exe.as_deref())
    ));
    details.push(format!(
        "managed by bun: {}",
        env::var_os("CODEX_MANAGED_BY_BUN").is_some()
    ));
    details.push(format!(
        "managed by Vite+: {}",
        env::var_os("CODEX_MANAGED_BY_VITE_PLUS").is_some()
    ));
    details.push(format!(
        "managed by pnpm: {}",
        env::var_os("CODEX_MANAGED_BY_PNPM").is_some()
    ));
    push_env_path_detail(
        &mut details,
        "managed package root",
        "CODEX_MANAGED_PACKAGE_ROOT",
    );

    let path_entries = codex_path_entries();
    let mut status = CheckStatus::Ok;
    let mut summary = "installation looks consistent".to_string();
    let mut remediation = None;

    if path_entries.len() > 1 {
        details.push(format!("PATH codex entries: {}", path_entries.len()));
    }
    if show_details || path_entries.len() > 1 {
        details.extend(
            path_entries
                .iter()
                .enumerate()
                .map(|(index, path)| format!("PATH codex #{}: {path}", index + 1)),
        );
    }

    if doctor_managed_by_npm(current_exe.as_deref()) {
        details
            .push("npm update target: not inspected (PATH helpers are not executed)".to_string());
        if env::var_os("CODEX_MANAGED_PACKAGE_ROOT").is_none() {
            status = status.max(CheckStatus::Warning);
            summary = "npm-managed launch is missing package-root provenance".to_string();
            remediation = Some(
                "Reinstall or update Codex so the JS shim provides CODEX_MANAGED_PACKAGE_ROOT."
                    .to_string(),
            );
        }
    }

    let mut check = DoctorCheck::new("installation", "install", status, summary).details(details);
    if let Some(remediation) = remediation {
        check = check.remediation(remediation);
    }
    check
}

pub(super) fn doctor_install_context(current_exe: Option<&Path>) -> InstallContext {
    if inherited_managed_env_for_cargo_binary(current_exe) {
        InstallContext {
            method: InstallMethod::Other,
            package_layout: None,
        }
    } else {
        InstallContext::current().clone()
    }
}

pub(super) fn doctor_managed_by_npm(current_exe: Option<&Path>) -> bool {
    env::var_os("CODEX_MANAGED_BY_NPM").is_some()
        && !inherited_managed_env_for_cargo_binary(current_exe)
}

fn inherited_managed_env_for_cargo_binary(current_exe: Option<&Path>) -> bool {
    if env::var_os("CODEX_MANAGED_BY_NPM").is_none()
        && env::var_os("CODEX_MANAGED_BY_BUN").is_none()
        && env::var_os("CODEX_MANAGED_BY_VITE_PLUS").is_none()
        && env::var_os("CODEX_MANAGED_BY_PNPM").is_none()
    {
        return false;
    }

    let Some(current_exe) = current_exe else {
        return false;
    };
    let components = current_exe
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|window| window[0] == "target" && matches!(window[1].as_ref(), "debug" | "release"))
}

pub(super) fn describe_install_context(context: &InstallContext) -> String {
    match &context.method {
        InstallMethod::Standalone {
            release_dir,
            resources_dir,
            platform,
        } => {
            let platform = match platform {
                StandalonePlatform::Unix => "unix",
                StandalonePlatform::Windows => "windows",
            };
            match &context.package_layout {
                Some(package_layout) => {
                    let resources = display_optional_path(package_layout.resources_dir.as_deref());
                    let path = display_optional_path(package_layout.path_dir.as_deref());
                    format!(
                        "standalone ({platform}, package {}, bin {}, resources {resources}, path {path})",
                        package_layout.package_dir.display(),
                        package_layout.bin_dir.display()
                    )
                }
                None => {
                    let resources = display_optional_path(resources_dir.as_deref());
                    format!(
                        "standalone ({platform}, release {}, resources {resources})",
                        release_dir.display()
                    )
                }
            }
        }
        InstallMethod::Npm => {
            describe_method_with_package_layout("npm", context.package_layout.as_ref())
        }
        InstallMethod::Bun => {
            describe_method_with_package_layout("bun", context.package_layout.as_ref())
        }
        InstallMethod::VitePlus => {
            describe_method_with_package_layout("vite+", context.package_layout.as_ref())
        }
        InstallMethod::Pnpm => {
            describe_method_with_package_layout("pnpm", context.package_layout.as_ref())
        }
        InstallMethod::Brew => {
            describe_method_with_package_layout("brew", context.package_layout.as_ref())
        }
        InstallMethod::Other => {
            describe_method_with_package_layout("other", context.package_layout.as_ref())
        }
    }
}

fn describe_method_with_package_layout(
    method: &str,
    package_layout: Option<&CodexPackageLayout>,
) -> String {
    match package_layout {
        Some(package_layout) => {
            let resources = display_optional_path(package_layout.resources_dir.as_deref());
            let path = display_optional_path(package_layout.path_dir.as_deref());
            format!(
                "{method} (package {}, bin {}, resources {resources}, path {path})",
                package_layout.package_dir.display(),
                package_layout.bin_dir.display()
            )
        }
        None => method.to_string(),
    }
}

fn display_optional_path(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn codex_path_entries() -> Vec<String> {
    let Ok(candidates) = which::which_all("codex") else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    candidates
        .filter(|candidate| seen.insert(candidate.clone()))
        .map(|path| path.display().to_string())
        .collect()
}

fn push_env_path_detail(details: &mut Vec<String>, label: &str, name: &str) {
    match env::var_os(name) {
        Some(path) => details.push(format!("{label}: {}", PathBuf::from(path).display())),
        None => details.push(format!("{label}: not set")),
    }
}
