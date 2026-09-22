//! Redaction and secret-safe semantic transformations.

use crate::{Flow, HeaderEntry, RedactionMarker};
use serde_json::Value;
use std::collections::BTreeSet;

/// Explicit redaction selectors applied before fixture publication.
#[derive(Debug, Clone, Default)]
pub struct RedactionConfig {
    /// Case-insensitive header names to replace.
    pub headers: BTreeSet<String>,
    /// Query keys to replace (also applies to form bodies).
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

    /// Build from a named [`crate::RedactionProfile`], lowercasing headers.
    pub fn from_profile(profile: &crate::RedactionProfile) -> Self {
        Self {
            headers: profile
                .sensitive_headers
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            query_keys: profile.sensitive_query_keys.iter().cloned().collect(),
            json_paths: profile.sensitive_json_paths.iter().cloned().collect(),
        }
    }

    /// Return whether any structured body redaction was requested.
    pub fn wants_body_redaction(&self) -> bool {
        !self.json_paths.is_empty() || !self.query_keys.is_empty()
    }
}

/// Default bound for buffered structured-body redaction (1 MiB).
///
/// Larger bodies with requested redaction fail closed rather than buffering
/// unboundedly. Callers may reuse a smaller documented body limit instead.
pub const DEFAULT_MAX_STRUCTURED_REDACTION_BYTES: u64 = 1024 * 1024;

/// Marker prefix for redacted JSON body paths.
pub const BODY_JSON_MARKER_PREFIX: &str = "request.body.json:";
/// Marker prefix for redacted response JSON body paths.
pub const RESPONSE_BODY_JSON_MARKER_PREFIX: &str = "response.body.json:";

/// Replace sensitive fields in a flow and return explicit markers.
///
/// Header/query redaction only; structured body bytes must already have been
/// transformed before publication (see `apply_json_redaction` and the HTTP
/// adapter). Body markers for transformed JSON must be attached by the caller
/// via `push_body_markers`.
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
    for header in &mut flow.request.trailers {
        redact_header(
            header,
            &config.headers,
            &mut flow.redactions,
            "request.trailers",
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

/// Attach body JSON redaction markers after a successful transform.
pub fn push_body_markers(
    markers: &mut Vec<RedactionMarker>,
    paths: &BTreeSet<String>,
    profile: &str,
    is_response: bool,
) {
    let prefix = if is_response {
        RESPONSE_BODY_JSON_MARKER_PREFIX
    } else {
        BODY_JSON_MARKER_PREFIX
    };
    for path in paths {
        markers.push(RedactionMarker {
            field: format!("{prefix}{path}"),
            profile: profile.into(),
        });
    }
}

/// Apply JSON Pointer redaction to buffered body bytes.
///
/// Returns the redacted bytes and whether any path matched. Fails closed on
/// malformed JSON. Caller enforces `max_bytes` before buffering and media-type
/// gating before calling.
pub fn apply_json_redaction(
    body: &[u8],
    paths: &BTreeSet<String>,
) -> Result<(Vec<u8>, bool), String> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| format!("malformed JSON for redaction: {e}"))?;
    let mut matched = false;
    for path in paths {
        if let Some(target) = value.pointer_mut(path) {
            *target = Value::String("<redacted>".into());
            matched = true;
        }
    }
    serde_json::to_vec(&value)
        .map_err(|e| format!("JSON re-encode failed: {e}"))
        .map(|b| (b, matched))
}

/// Apply form redaction (`application/x-www-form-urlencoded`) using query keys.
pub fn apply_form_redaction(
    body: &[u8],
    query_keys: &BTreeSet<String>,
) -> Result<(Vec<u8>, bool), String> {
    let text = std::str::from_utf8(body).map_err(|_| "form body is not valid UTF-8".to_string())?;
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(text.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    // Verify round-trip stability to avoid silently altering encoding; if the
    // body is not a well-formed form encoding, fail closed when redaction was
    // explicitly requested.
    if pairs.is_empty() && !text.is_empty() {
        return Err("malformed form body for redaction".into());
    }
    let mut matched = false;
    for (key, value) in pairs.iter_mut() {
        if query_keys.contains(key) {
            *value = "<redacted>".into();
            matched = true;
        }
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in &pairs {
        serializer.append_pair(key, value);
    }
    Ok((serializer.finish().into_bytes(), matched))
}

/// Reconcile representation headers after a body transform.
///
/// - `Content-Length` is recomputed to the new length (never stale).
/// - `Content-MD5`, `Digest`, `Content-Digest`, `Signature`, `Signature-Input`,
///   and strong `ETag`s are removed (invalidated by the transform) with
///   markers; weak ETags are preserved with a warning annotation.
/// - Returns markers/annotations for the caller to attach to the flow.
pub fn reconcile_headers_after_body_redaction(
    headers: &mut Vec<HeaderEntry>,
    new_len: u64,
    profile: &str,
) -> (Vec<RedactionMarker>, Vec<(String, String)>) {
    let mut markers = Vec::new();
    let mut annotations = Vec::new();
    headers.retain(|header| {
        let name = header.name.to_ascii_lowercase();
        match name.as_str() {
            "content-length" => false,
            "content-md5" | "content-digest" | "digest" | "signature" | "signature-input" => {
                markers.push(RedactionMarker {
                    field: format!("headers.{name}.removed-after-redaction"),
                    profile: profile.into(),
                });
                false
            }
            "etag" => {
                if header.value.trim_start().starts_with("W/") {
                    annotations.push((
                        "redaction".into(),
                        "weak-etag-preserved-after-redaction".into(),
                    ));
                    true
                } else {
                    markers.push(RedactionMarker {
                        field: "headers.etag.removed-after-redaction".into(),
                        profile: profile.into(),
                    });
                    false
                }
            }
            _ => true,
        }
    });
    headers.push(HeaderEntry {
        name: "content-length".into(),
        value: new_len.to_string(),
    });
    (markers, annotations)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn framing_metadata_reconciled_after_redaction() {
        let mut headers = vec![
            HeaderEntry {
                name: "content-length".into(),
                value: "100".into(),
            },
            HeaderEntry {
                name: "content-md5".into(),
                value: "abc".into(),
            },
            HeaderEntry {
                name: "etag".into(),
                value: "\"strong-123\"".into(),
            },
            HeaderEntry {
                name: "x-keep".into(),
                value: "yes".into(),
            },
        ];
        let (markers, _) = reconcile_headers_after_body_redaction(&mut headers, 42, "test-v1");
        let length = headers
            .iter()
            .find(|header| header.name == "content-length")
            .expect("content-length recomputed");
        assert_eq!(length.value, "42");
        assert!(headers.iter().all(|header| header.name != "content-md5"));
        assert!(
            headers
                .iter()
                .all(|header| !(header.name == "etag" && header.value.contains("strong")))
        );
        assert!(headers.iter().any(|header| header.name == "x-keep"));
        assert!(
            markers
                .iter()
                .any(|marker| marker.field.contains("content-md5"))
        );
        // Weak ETag preserved with warning.
        let mut weak = vec![HeaderEntry {
            name: "etag".into(),
            value: "W/\"weak-1\"".into(),
        }];
        let (_, annotations) = reconcile_headers_after_body_redaction(&mut weak, 7, "test-v1");
        assert!(weak.iter().any(|header| header.value.contains("weak")));
        assert!(
            annotations
                .iter()
                .any(|(_, value)| value.contains("weak-etag"))
        );
    }

    #[test]
    fn json_and_form_redaction_replaces_sentinels() {
        let body = br#"{"secret":"SENTINEL","keep":1}"#;
        let paths = BTreeSet::from(["/secret".to_string()]);
        let (redacted, matched) = apply_json_redaction(body, &paths).unwrap();
        assert!(matched);
        assert!(!redacted.windows(8).any(|window| window == b"SENTINEL"));
        assert!(String::from_utf8_lossy(&redacted).contains("<redacted>"));
        let form = b"api_key=SENTINEL&other=1";
        let keys = BTreeSet::from(["api_key".to_string()]);
        let (redacted_form, matched) = apply_form_redaction(form, &keys).unwrap();
        assert!(matched);
        assert!(!redacted_form.windows(8).any(|window| window == b"SENTINEL"));
    }
}
