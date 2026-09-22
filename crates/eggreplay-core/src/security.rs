//! Redaction and secret-safe semantic transformations.

use crate::{Flow, HeaderEntry, RedactionMarker};
use serde_json::Value;
use std::collections::BTreeSet;

/// Explicit redaction selectors applied before fixture publication.
#[derive(Debug, Clone, Default)]
pub struct RedactionConfig {
    /// Case-insensitive header names to replace.
    pub headers: BTreeSet<String>,
    /// Query keys to replace.
    pub query_keys: BTreeSet<String>,
    /// JSON pointers to replace with a marker.
    pub json_paths: BTreeSet<String>,
}

impl RedactionConfig {
    /// Construct the secure default profile.
    pub fn default_secure() -> Self {
        Self {
            headers: [
                "authorization",
                "proxy-authorization",
                "cookie",
                "set-cookie",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            query_keys: BTreeSet::new(),
            json_paths: BTreeSet::new(),
        }
    }
}

/// Replace sensitive fields in a flow and return explicit markers.
pub fn redact_flow(flow: &mut Flow, config: &RedactionConfig, profile: &str) {
    for header in &mut flow.request.headers {
        redact_header(
            header,
            &config.headers,
            &mut flow.redactions,
            "request.headers",
            profile,
        );
    }
    if let crate::FlowOutcome::Response(response) = &mut flow.outcome {
        for header in &mut response.headers {
            redact_header(
                header,
                &config.headers,
                &mut flow.redactions,
                "response.headers",
                profile,
            );
        }
        for header in &mut response.trailers {
            redact_header(
                header,
                &config.headers,
                &mut flow.redactions,
                "response.trailers",
                profile,
            );
        }
    }
    for pair in &mut flow.request.query {
        if config.query_keys.contains(&pair.key) {
            pair.value = "<redacted>".into();
            flow.redactions.push(RedactionMarker {
                field: format!("request.query.{}", pair.key),
                profile: profile.into(),
            });
        }
    }
}

/// Redact configured JSON pointer paths in a structured body.
pub fn redact_json(value: &mut Value, paths: &BTreeSet<String>) {
    for path in paths {
        if let Some(target) = value.pointer_mut(path) {
            *target = Value::String("<redacted>".into());
        }
    }
}

/// Remove userinfo and query/fragment material from a URL used in diagnostics.
pub fn redact_url(input: &str) -> String {
    input
        .parse::<url::Url>()
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|_| "<invalid-url>".into())
}

fn redact_header(
    header: &mut HeaderEntry,
    names: &BTreeSet<String>,
    markers: &mut Vec<RedactionMarker>,
    prefix: &str,
    profile: &str,
) {
    if names.contains(&header.name.to_ascii_lowercase()) {
        header.value = "<redacted>".into();
        markers.push(RedactionMarker {
            field: format!("{prefix}.{}", header.name),
            profile: profile.into(),
        });
    }
}
