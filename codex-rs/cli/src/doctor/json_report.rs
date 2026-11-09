//! Converts a `DoctorReport` into the redacted, check-id keyed `--json` support report.

use super::*;

impl JsonDetailValue {
    fn push(&mut self, value: String) {
        match self {
            JsonDetailValue::One(previous) => {
                *self = JsonDetailValue::Many(vec![std::mem::take(previous), value]);
            }
            JsonDetailValue::Many(values) => values.push(value),
        }
    }
}

pub(super) fn redacted_json_report(report: &DoctorReport) -> JsonDoctorReport {
    let checks = report
        .checks
        .iter()
        .map(|check| {
            let json_check = redacted_json_check(check);
            (check.id.clone(), json_check)
        })
        .collect();
    JsonDoctorReport {
        schema_version: report.schema_version,
        generated_at: report.generated_at.clone(),
        overall_status: report.overall_status,
        codex_version: report.codex_version.clone(),
        checks,
    }
}

pub(super) fn redacted_json_check(check: &DoctorCheck) -> JsonDoctorCheck {
    let (details, notes) = structured_json_details(&check.details);
    JsonDoctorCheck {
        id: check.id.clone(),
        category: check.category.clone(),
        status: check.status,
        summary: check.summary.clone(),
        details,
        issues: check.issues.iter().map(redacted_json_issue).collect(),
        notes,
        remediation: check.remediation.as_deref().map(redact_detail),
        duration_ms: check.duration_ms,
    }
}

fn redacted_json_issue(issue: &DoctorIssue) -> JsonDoctorIssue {
    JsonDoctorIssue {
        severity: issue.severity,
        cause: redact_detail(&issue.cause),
        measured: issue.measured.as_deref().map(redact_detail),
        expected: issue.expected.as_deref().map(redact_detail),
        remedy: issue.remedy.as_deref().map(redact_detail),
        fields: issue
            .fields
            .iter()
            .map(|field| redact_detail(field))
            .collect(),
    }
}

/// Converts redacted `label: value` detail strings into JSON key/value fields.
///
/// Detail strings that do not follow the doctor detail convention are preserved
/// as notes instead of being dropped. Repeated labels become arrays so callers
/// can still retrieve the common scalar case directly while keeping all values.
fn structured_json_details(details: &[String]) -> (BTreeMap<String, JsonDetailValue>, Vec<String>) {
    let mut structured: BTreeMap<String, JsonDetailValue> = BTreeMap::new();
    let mut notes = Vec::new();
    for detail in details {
        let redacted = redact_detail(detail);
        let Some((key, value)) = redacted.split_once(": ") else {
            notes.push(redacted);
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            notes.push(redacted);
            continue;
        }
        let value = json_detail_value(key, value);
        match structured.get_mut(key) {
            Some(existing) => existing.push(value),
            None => {
                structured.insert(key.to_string(), JsonDetailValue::One(value));
            }
        }
    }
    (structured, notes)
}

fn json_detail_value(key: &str, value: &str) -> String {
    if matches!(
        key,
        "VISUAL" | "EDITOR" | "PAGER" | "GIT_PAGER" | "GH_PAGER" | "LESS"
    ) && !value.eq_ignore_ascii_case("not set")
    {
        // Editor and pager configuration can contain arbitrary arguments or
        // inline environment assignments. Keep full values local to human output
        // because the JSON report may be attached to feedback.
        "set".to_string()
    } else {
        value.to_string()
    }
}
