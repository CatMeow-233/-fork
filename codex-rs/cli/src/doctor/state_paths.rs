//! State check: CODEX_HOME paths, SQLite integrity, rollout file statistics, and release cache.

use super::*;

pub(super) async fn state_check(config: &Config, command: &DoctorCommand) -> DoctorCheck {
    let mut details = Vec::new();
    path_readiness(&mut details, "CODEX_HOME", &config.codex_home);
    path_readiness(&mut details, "log dir", &config.log_dir);
    path_readiness(&mut details, "sqlite home", config.sqlite_config().home());

    let mut status = CheckStatus::Ok;
    let mut failed_databases = Vec::new();
    for db in config.sqlite_config().runtime_db_paths() {
        path_readiness(&mut details, db.label, &db.path);
        // Feedback collection gives each database its own budget; direct runs scan fully.
        let deadline = command
            .feedback
            .then(|| Instant::now() + Duration::from_secs(1));
        let db_status = sqlite_integrity_detail(
            config.sqlite_config(),
            &mut details,
            db.label,
            &db.path,
            deadline,
        )
        .await;
        if db_status == CheckStatus::Fail {
            failed_databases.push(db.path.display().to_string());
        }
        status = status.max(db_status);
    }
    rollout_stats_details(&mut details, &config.codex_home);
    standalone_release_cache_details(&mut details);

    let summary = match status {
        CheckStatus::Ok => "state paths and databases are inspectable".to_string(),
        CheckStatus::Warning => {
            "some database integrity checks exceeded their time limit".to_string()
        }
        CheckStatus::Fail => "state database integrity check failed".to_string(),
    };
    let mut check = DoctorCheck::new("state.paths", "state", status, summary).details(details);
    if status == CheckStatus::Fail {
        let noun = if failed_databases.len() == 1 {
            "database"
        } else {
            "databases"
        };
        check = check.issue(
            DoctorIssue::new(
                CheckStatus::Fail,
                format!(
                    "{} {noun} failed integrity check",
                    failed_databases.join(", ")
                ),
            )
            .remedy(
                "Move the damaged SQLite database aside, then restart the interactive CLI or app server so it can rebuild that runtime database from saved data. Other entry points may not rebuild automatically.",
            ),
        );
    }
    check
}

async fn sqlite_integrity_detail(
    sqlite: &codex_state::SqliteConfig,
    details: &mut Vec<String>,
    label: &str,
    path: &Path,
    deadline: Option<Instant>,
) -> CheckStatus {
    if !path.is_file() {
        details.push(format!("{label} integrity: skipped (missing)"));
        return CheckStatus::Ok;
    }

    let (rows, timed_out) = match codex_state::sqlite_integrity_check(sqlite, path, deadline).await
    {
        Ok(codex_state::SqliteIntegrityCheck::Complete(rows)) => (rows, false),
        Ok(codex_state::SqliteIntegrityCheck::TimedOut(rows)) => (rows, true),
        Err(err) => {
            details.push(format!("{label} integrity: {err}"));
            return CheckStatus::Fail;
        }
    };
    if timed_out {
        details.push(format!(
            "{label} integrity: incomplete (1 second time limit)"
        ));
    }
    if rows.iter().any(|row| row != "ok") {
        details.push(format!("{label} integrity: {}", rows.join("; ")));
        CheckStatus::Fail
    } else if timed_out {
        CheckStatus::Warning
    } else {
        details.push(format!("{label} integrity: ok"));
        CheckStatus::Ok
    }
}

pub(super) fn rollout_stats_details(details: &mut Vec<String>, codex_home: &Path) {
    let active = collect_rollout_stats(&codex_home.join("sessions"));
    let archived = collect_rollout_stats(&codex_home.join("archived_sessions"));
    push_rollout_stats_detail(details, "active rollout files", active);
    push_rollout_stats_detail(details, "archived rollout files", archived);
}

fn push_rollout_stats_detail(details: &mut Vec<String>, label: &str, stats: RolloutStats) {
    match stats.error {
        Some(error) => details.push(format!("{label}: scan failed ({error})")),
        None => details.push(format!(
            "{label}: {} files, {} total bytes, {} average bytes",
            stats.files,
            stats.total_bytes,
            stats.average_bytes()
        )),
    }
}

impl RolloutStats {
    fn average_bytes(&self) -> u64 {
        self.total_bytes.checked_div(self.files).unwrap_or(0)
    }
}

fn collect_rollout_stats(root: &Path) -> RolloutStats {
    let mut stats = RolloutStats::default();
    collect_rollout_stats_inner(root, &mut stats);
    stats
}

fn collect_rollout_stats_inner(path: &Path, stats: &mut RolloutStats) {
    if stats.error.is_some() {
        return;
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            stats.error = Some(err.to_string());
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                stats.error = Some(err.to_string());
                return;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                stats.error = Some(err.to_string());
                return;
            }
        };
        if metadata.is_dir() {
            collect_rollout_stats_inner(&path, stats);
        } else if metadata.is_file() && is_rollout_file(&path) {
            stats.files += 1;
            stats.total_bytes = stats.total_bytes.saturating_add(metadata.len());
        }
    }
}

fn is_rollout_file(path: &Path) -> bool {
    let path = codex_rollout::plain_rollout_path(path);
    path.extension() == Some(OsStr::new("jsonl"))
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("rollout-"))
}

pub(super) fn fallback_state_check() -> DoctorCheck {
    let codex_home = find_codex_home();
    match codex_home {
        Ok(path) => DoctorCheck::new(
            "state.paths",
            "state",
            CheckStatus::Ok,
            "CODEX_HOME was resolved without config",
        )
        .detail(format!("CODEX_HOME: {}", path.display())),
        Err(err) => DoctorCheck::new(
            "state.paths",
            "state",
            CheckStatus::Warning,
            "CODEX_HOME could not be resolved",
        )
        .detail(err.to_string()),
    }
}

fn path_readiness(details: &mut Vec<String>, label: &str, path: &Path) {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let kind = if metadata.is_dir() {
                "dir"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            details.push(format!("{label}: {} ({kind})", path.display()));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            details.push(format!("{label}: {} (missing)", path.display()));
        }
        Err(err) => details.push(format!("{label}: {} ({err})", path.display())),
    }
}

fn standalone_release_cache_details(details: &mut Vec<String>) {
    let context = InstallContext::current();
    let InstallMethod::Standalone { release_dir, .. } = &context.method else {
        return;
    };
    let Some(releases_dir) = release_dir.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&releases_dir) else {
        return;
    };
    let release_count = entries.filter_map(Result::ok).count();
    details.push(format!(
        "standalone release cache: {release_count} entries in {}",
        releases_dir.display()
    ));
}
