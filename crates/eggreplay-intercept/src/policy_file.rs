//! Versioned declarative target-policy file for the M013E operator surface.
//!
//! The file is JSON (no new dependencies: the CLI already uses `serde_json`)
//! with an explicit version field. There is no scripting, no regex, and no
//! allow-all default: unmatched targets deny, deny wins ties, and parsing is
//! bounded and fails closed on unknown fields, versions, or actions.
//!
//! # File format (`eggreplay-intercept-policy/v1`)
//!
//! ```json
//! {
//!   "version": "eggreplay-intercept-policy/v1",
//!   "default_connect_action": "deny",
//!   "rules": [
//!     {
//!       "host": "example.test",
//!       "ports": "any",
//!       "kind": "any",
//!       "action": "intercept"
//!     }
//!   ]
//! }
//! ```
//!
//! - `host`: an exact DNS name (`example.test`), a safe suffix parent with a
//!   `*.` (or leading `.`) prefix (`*.example.test`, matched on a label
//!   boundary only), or an exact IP literal (`192.0.2.1`, `[::1]`).
//! - `ports`: `"any"`, a single port (`443` or `"443"`), a bounded inclusive
//!   range (`"8000-8100"`), or a bounded array of ports (`[80, 443]`).
//! - `kind`: `"plain"` (absolute-form HTTP), `"connect"` (`CONNECT`), or
//!   `"any"` (default).
//! - `action`: `"deny"`, `"tunnel"`, or `"intercept"`. `tunnel` and
//!   `intercept` both mean *allow* at the [`TargetPolicy`] layer; the
//!   distinction selects the `CONNECT` behavior. Rules whose kind covers
//!   `CONNECT` and that are not `deny` must agree with
//!   `default_connect_action` (mixed tunnel/intercept listeners are rejected
//!   as a configuration error; run separate listeners instead). Plain-only
//!   rules treat `tunnel`/`intercept` as allow.
//! - `default_connect_action`: `"deny"` (default when omitted), `"tunnel"`,
//!   or `"intercept"`.
//!
//! [`inspect`](FilePolicy::normalized_summary)/`validate` output echoes only
//! normalized public facts (hosts, ports, counts). Policy files never carry
//! CA key material, and this module never logs credentials.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::policy::{
    ConnectAction, HostMatch, MAX_POLICY_RULES, PortMatch, RequestKind, Rule, RuleAction,
    TargetPolicy,
};

/// Version string every M013E policy file must carry.
pub const INTERCEPT_POLICY_VERSION: &str = "eggreplay-intercept-policy/v1";
/// Maximum accepted policy-file size in bytes (bounded parsing).
pub const MAX_POLICY_FILE_BYTES: usize = 64 * 1024;
/// Bound applied to error detail strings (never credentials; policies carry none).
const DETAIL_LEN: usize = 200;

/// Fail-closed policy-file errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyFileError {
    /// The file exceeds [`MAX_POLICY_FILE_BYTES`].
    #[error("policy file exceeds the size bound")]
    TooLarge,
    /// A filesystem operation failed (paths are deliberately omitted).
    #[error("policy file I/O failed during {0}")]
    Io(&'static str),
    /// The file is not valid JSON or violates the schema.
    #[error("invalid policy file: {0}")]
    Invalid(String),
    /// The `version` field is missing or not supported.
    #[error("unsupported policy version: {0}")]
    UnsupportedVersion(String),
    /// The rule list exceeds [`MAX_POLICY_RULES`].
    #[error("too many policy rules: {0}")]
    TooManyRules(usize),
    /// Non-deny `CONNECT`-covering rules disagree with the default action.
    #[error(
        "policy mixes tunnel and intercept actions; align every non-deny rule with the default"
    )]
    MixedActions,
}

fn bound_detail(input: &str) -> String {
    if input.len() <= DETAIL_LEN {
        return input.to_owned();
    }
    let mut out: String = input.chars().take(DETAIL_LEN).collect();
    out.push_str("...");
    out
}

/// Per-rule action in the file vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Refuse the target.
    Deny,
    /// Allow; `CONNECT` targets relay opaquely.
    Tunnel,
    /// Allow; `CONNECT` targets terminate TLS and record.
    Intercept,
}

impl FileAction {
    fn parse(input: &str) -> Result<Self, PolicyFileError> {
        match input.trim().to_ascii_lowercase().as_str() {
            "deny" => Ok(Self::Deny),
            "tunnel" => Ok(Self::Tunnel),
            "intercept" => Ok(Self::Intercept),
            other => Err(PolicyFileError::Invalid(format!(
                "unknown rule action: {}",
                bound_detail(other)
            ))),
        }
    }

    /// The [`ConnectAction`] this rule selects for `CONNECT` targets.
    #[must_use]
    pub fn connect_action(self) -> ConnectAction {
        match self {
            Self::Deny => ConnectAction::Deny,
            Self::Tunnel => ConnectAction::Tunnel,
            Self::Intercept => ConnectAction::Intercept,
        }
    }

    /// The [`RuleAction`] this rule contributes to the [`TargetPolicy`] layer.
    #[must_use]
    pub fn rule_action(self) -> RuleAction {
        match self {
            Self::Deny => RuleAction::Deny,
            Self::Tunnel | Self::Intercept => RuleAction::Allow,
        }
    }

    #[must_use]
    /// Render the action in file vocabulary (`deny`, `tunnel`, `intercept`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Tunnel => "tunnel",
            Self::Intercept => "intercept",
        }
    }
}

/// One validated file rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRule {
    /// Host matcher.
    pub host: HostMatch,
    /// Port matcher.
    pub ports: PortMatch,
    /// Request-kind matcher.
    pub kind: RequestKind,
    /// Explicit file action.
    pub action: FileAction,
    /// Normalized host pattern as written (for normalized summaries).
    pub host_display: String,
}

/// A validated file policy: default action plus bounded explicit rules.
#[derive(Debug, Clone)]
pub struct FilePolicy {
    default_connect_action: ConnectAction,
    rules: Vec<FileRule>,
}

impl FilePolicy {
    /// Parse and validate policy-file JSON bytes (bounded, fail-closed).
    ///
    /// # Errors
    ///
    /// Returns [`PolicyFileError`] for oversized input, malformed JSON,
    /// unknown fields/versions/actions, unbounded rule lists, malformed
    /// hosts/ports, or mixed tunnel/intercept actions.
    pub fn parse_json(bytes: &[u8]) -> Result<Self, PolicyFileError> {
        if bytes.len() > MAX_POLICY_FILE_BYTES {
            return Err(PolicyFileError::TooLarge);
        }
        let document: PolicyDocument = serde_json::from_slice(bytes)
            .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())))?;
        Self::from_document(&document)
    }

    /// Read (bounded) and parse a policy file from disk.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyFileError`] for oversized/unreadable files or any
    /// [`parse_json`](Self::parse_json) failure.
    pub fn load_file(path: &Path) -> Result<Self, PolicyFileError> {
        use std::io::Read as _;
        let len = std::fs::metadata(path)
            .map_err(|_| PolicyFileError::Io("stat"))
            .map(|meta| meta.len())?;
        if len > MAX_POLICY_FILE_BYTES as u64 {
            return Err(PolicyFileError::TooLarge);
        }
        let file = std::fs::File::open(path).map_err(|_| PolicyFileError::Io("open"))?;
        let mut bytes = Vec::new();
        file.take(MAX_POLICY_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PolicyFileError::Io("read"))?;
        Self::parse_json(&bytes)
    }

    fn from_document(document: &PolicyDocument) -> Result<Self, PolicyFileError> {
        if document.version != INTERCEPT_POLICY_VERSION {
            return Err(PolicyFileError::UnsupportedVersion(bound_detail(
                &document.version,
            )));
        }
        if document.rules.len() > MAX_POLICY_RULES {
            return Err(PolicyFileError::TooManyRules(document.rules.len()));
        }
        let default_connect_action = parse_connect_action(&document.default_connect_action)?;
        let mut rules = Vec::with_capacity(document.rules.len());
        for rule in &document.rules {
            rules.push(parse_rule(rule)?);
        }
        let policy = Self {
            default_connect_action,
            rules,
        };
        policy.check_coherence()?;
        Ok(policy)
    }

    /// Reject mixed tunnel/intercept listeners fail-closed.
    ///
    /// Every non-deny rule whose kind covers `CONNECT` must agree with the
    /// configured default; otherwise neither the [`TargetPolicy`] mapping nor
    /// the operational `CONNECT` behavior would have a single truthful
    /// meaning. Operators needing both behaviors run separate listeners.
    fn check_coherence(&self) -> Result<(), PolicyFileError> {
        for rule in &self.rules {
            if rule.action == FileAction::Deny {
                continue;
            }
            if !rule.kind.covers(RequestKind::Connect) {
                continue;
            }
            if rule.action.connect_action() != self.default_connect_action {
                return Err(PolicyFileError::MixedActions);
            }
        }
        Ok(())
    }

    /// The configured default `CONNECT` action.
    #[must_use]
    pub fn default_connect_action(&self) -> ConnectAction {
        self.default_connect_action
    }

    /// Number of validated rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Validated rules (normalized).
    #[must_use]
    pub fn rules(&self) -> &[FileRule] {
        &self.rules
    }

    /// Count rules by action (`deny`, `tunnel`, `intercept`).
    #[must_use]
    pub fn action_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for rule in &self.rules {
            *counts.entry(rule.action.as_str()).or_insert(0) += 1;
        }
        counts
    }

    /// Convert to the transport-neutral [`TargetPolicy`].
    ///
    /// `deny` maps to [`RuleAction::Deny`]; `tunnel`/`intercept` map to
    /// [`RuleAction::Allow`] under the preserved default, so `CONNECT`
    /// resolution through the converted policy agrees with the file for every
    /// coherent policy (enforced at parse time).
    ///
    /// # Errors
    ///
    /// Returns the policy error only when the validated rule list somehow
    /// exceeds [`MAX_POLICY_RULES`] (unreachable after parsing).
    pub fn to_target_policy(&self) -> Result<TargetPolicy, crate::policy::PolicyError> {
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                Rule::new(
                    rule.host.clone(),
                    rule.ports.clone(),
                    rule.kind,
                    rule.action.rule_action(),
                )
            })
            .collect();
        TargetPolicy::new(rules, self.default_connect_action)
    }

    /// Normalized public summary for dry-run/validate output.
    ///
    /// Contains only hosts, ports, kinds, actions, and counts: policy files
    /// never carry CA key material, and no paths or credentials appear here.
    #[must_use]
    pub fn normalized_summary(&self) -> serde_json::Value {
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "host": rule.host_display,
                    "host_match": host_match_kind(&rule.host),
                    "ports": ports_display(&rule.ports),
                    "kind": request_kind_str(rule.kind),
                    "action": rule.action.as_str(),
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "version": INTERCEPT_POLICY_VERSION,
            "default_connect_action": connect_action_str(self.default_connect_action),
            "rule_count": self.rules.len(),
            "action_counts": self.action_counts(),
            "rules": rules,
        })
    }
}

fn host_match_kind(host: &HostMatch) -> &'static str {
    match host {
        HostMatch::ExactDns(_) => "exact",
        HostMatch::SuffixDns(_) => "suffix",
        HostMatch::ExactIp(_) => "ip",
    }
}

fn ports_display(ports: &PortMatch) -> String {
    match ports {
        PortMatch::Exact(port) => port.to_string(),
        PortMatch::Range { start, end } => format!("{start}-{end}"),
        PortMatch::Set(set) => set
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
        PortMatch::Any => "any".to_owned(),
    }
}

fn request_kind_str(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Plain => "plain",
        RequestKind::Connect => "connect",
        RequestKind::Any => "any",
    }
}

fn connect_action_str(action: ConnectAction) -> &'static str {
    match action {
        ConnectAction::Deny => "deny",
        ConnectAction::Tunnel => "tunnel",
        ConnectAction::Intercept => "intercept",
    }
}

fn parse_connect_action(input: &str) -> Result<ConnectAction, PolicyFileError> {
    match input.trim().to_ascii_lowercase().as_str() {
        "deny" => Ok(ConnectAction::Deny),
        "tunnel" => Ok(ConnectAction::Tunnel),
        "intercept" => Ok(ConnectAction::Intercept),
        other => Err(PolicyFileError::Invalid(format!(
            "unknown default_connect_action: {}",
            bound_detail(other)
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDocument {
    version: String,
    #[serde(default = "default_action_deny")]
    default_connect_action: String,
    #[serde(default)]
    rules: Vec<RuleDocument>,
}

fn default_action_deny() -> String {
    "deny".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDocument {
    host: String,
    #[serde(default)]
    ports: serde_json::Value,
    #[serde(default = "default_kind_any")]
    kind: String,
    action: String,
}

fn default_kind_any() -> String {
    "any".to_owned()
}

fn parse_rule(rule: &RuleDocument) -> Result<FileRule, PolicyFileError> {
    let host_display = rule.host.trim().to_owned();
    if host_display.is_empty() {
        return Err(PolicyFileError::Invalid(
            "rule host must not be empty".to_owned(),
        ));
    }
    let host = parse_host_pattern(&host_display)?;
    let ports = parse_ports(&rule.ports)?;
    let kind = parse_kind(&rule.kind)?;
    let action = FileAction::parse(&rule.action)?;
    Ok(FileRule {
        host,
        ports,
        kind,
        action,
        host_display: normalized_host_display(&host_display),
    })
}

/// Normalize a host pattern for display (lowercase DNS forms).
fn normalized_host_display(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

/// Parse an exact, safe-suffix (`*.` or leading `.`), or IP host pattern.
fn parse_host_pattern(input: &str) -> Result<HostMatch, PolicyFileError> {
    let trimmed = input.trim();
    if let Some(parent) = trimmed
        .strip_prefix("*.")
        .or_else(|| trimmed.strip_prefix('.'))
    {
        if parent.is_empty() {
            return Err(PolicyFileError::Invalid("empty suffix parent".to_owned()));
        }
        return HostMatch::suffix_dns(parent)
            .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())));
    }
    // Exact IP literal (bracketed IPv6 accepted).
    let bracketed = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'));
    if let Some(inner) = bracketed {
        return inner
            .parse::<std::net::IpAddr>()
            .map(HostMatch::exact_ip)
            .map_err(|_| {
                PolicyFileError::Invalid(format!("invalid host pattern: {}", bound_detail(trimmed)))
            });
    }
    if let Ok(addr) = trimmed.parse::<std::net::IpAddr>() {
        return Ok(HostMatch::exact_ip(addr));
    }
    HostMatch::exact_dns(trimmed)
        .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())))
}

/// Parse the `ports` value: `"any"` (or null/missing), a single port, a
/// `"lo-hi"` range string, or an array of ports.
fn parse_ports(value: &serde_json::Value) -> Result<PortMatch, PolicyFileError> {
    match value {
        serde_json::Value::Null => Ok(PortMatch::Any),
        serde_json::Value::String(text) => {
            let text = text.trim();
            if text.eq_ignore_ascii_case("any") {
                return Ok(PortMatch::Any);
            }
            if let Some((lo, hi)) = text.split_once('-') {
                let start = parse_one_port(lo.trim())?;
                let end = parse_one_port(hi.trim())?;
                return PortMatch::range(start, end)
                    .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())));
            }
            Ok(PortMatch::exact(parse_one_port(text)?)
                .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())))?)
        }
        serde_json::Value::Number(number) => {
            let port = number
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| {
                    PolicyFileError::Invalid(format!(
                        "invalid port: {}",
                        bound_detail(&number.to_string())
                    ))
                })?;
            PortMatch::exact(port)
                .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())))
        }
        serde_json::Value::Array(items) => {
            let mut ports = Vec::with_capacity(items.len());
            for item in items {
                let port = item
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok())
                    .ok_or_else(|| {
                        PolicyFileError::Invalid(format!(
                            "invalid port in set: {}",
                            bound_detail(&item.to_string())
                        ))
                    })?;
                ports.push(port);
            }
            PortMatch::set(ports)
                .map_err(|err| PolicyFileError::Invalid(bound_detail(&err.to_string())))
        }
        _ => Err(PolicyFileError::Invalid(
            "ports must be \"any\", a port, a \"lo-hi\" range, or an array of ports".to_owned(),
        )),
    }
}

fn parse_one_port(input: &str) -> Result<u16, PolicyFileError> {
    let port: u16 = input
        .parse()
        .map_err(|_| PolicyFileError::Invalid(format!("invalid port: {}", bound_detail(input))))?;
    if port == 0 {
        return Err(PolicyFileError::Invalid("port zero is rejected".to_owned()));
    }
    Ok(port)
}

fn parse_kind(input: &str) -> Result<RequestKind, PolicyFileError> {
    match input.trim().to_ascii_lowercase().as_str() {
        "plain" => Ok(RequestKind::Plain),
        "connect" => Ok(RequestKind::Connect),
        "any" => Ok(RequestKind::Any),
        other => Err(PolicyFileError::Invalid(format!(
            "unknown rule kind: {}",
            bound_detail(other)
        ))),
    }
}

/// Build a [`FilePolicy`] from CLI flag rules (no policy file).
///
/// `allow_hosts`/`deny_hosts` use the same host-pattern syntax as the file
/// (exact, `*.` suffix, or IP literal) with any port and any kind. The
/// allow-side file action is derived from `default_action` so the result is
/// always coherent: `deny`/`tunnel` defaults produce tunnel allows, and an
/// `intercept` default produces intercept allows.
///
/// # Errors
///
/// Returns [`PolicyFileError`] for malformed patterns or an unbounded list.
pub fn policy_from_flags(
    default_action: ConnectAction,
    allow_hosts: &[String],
    deny_hosts: &[String],
) -> Result<FilePolicy, PolicyFileError> {
    let total = allow_hosts.len().saturating_add(deny_hosts.len());
    if total > MAX_POLICY_RULES {
        return Err(PolicyFileError::TooManyRules(total));
    }
    let allow_action = match default_action {
        ConnectAction::Intercept => FileAction::Intercept,
        ConnectAction::Tunnel | ConnectAction::Deny => FileAction::Tunnel,
    };
    let mut rules = Vec::with_capacity(total);
    for host in allow_hosts {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(PolicyFileError::Invalid(
                "allow host must not be empty".to_owned(),
            ));
        }
        rules.push(FileRule {
            host: parse_host_pattern(trimmed)?,
            ports: PortMatch::Any,
            kind: RequestKind::Any,
            action: allow_action,
            host_display: normalized_host_display(trimmed),
        });
    }
    for host in deny_hosts {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(PolicyFileError::Invalid(
                "deny host must not be empty".to_owned(),
            ));
        }
        rules.push(FileRule {
            host: parse_host_pattern(trimmed)?,
            ports: PortMatch::Any,
            kind: RequestKind::Any,
            action: FileAction::Deny,
            host_display: normalized_host_display(trimmed),
        });
    }
    let policy = FilePolicy {
        default_connect_action: default_action,
        rules,
    };
    policy.check_coherence()?;
    Ok(policy)
}

/// Parse a `--default-action` flag value.
///
/// # Errors
///
/// Returns [`PolicyFileError::Invalid`] for unknown actions.
pub fn parse_default_action(input: &str) -> Result<ConnectAction, PolicyFileError> {
    parse_connect_action(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{NormalizedHost, normalize_host};

    fn target(host: &str, port: u16) -> (NormalizedHost, u16) {
        (normalize_host(host).expect("valid test host"), port)
    }

    #[test]
    fn minimal_policy_defaults_to_deny_with_no_rules() {
        let policy =
            FilePolicy::parse_json(br#"{"version": "eggreplay-intercept-policy/v1"}"#).unwrap();
        assert_eq!(policy.default_connect_action(), ConnectAction::Deny);
        assert_eq!(policy.rule_count(), 0);
        let target_policy = policy.to_target_policy().unwrap();
        let (host, port) = target("example.test", 443);
        assert_eq!(
            target_policy.resolve_connect(&host, port),
            ConnectAction::Deny
        );
    }

    #[test]
    fn exact_suffix_and_ip_rules_map_onto_target_policy() {
        let policy = FilePolicy::parse_json(
            br#"{
                "version": "eggreplay-intercept-policy/v1",
                "default_connect_action": "intercept",
                "rules": [
                    {"host": "Example.TEST.", "ports": 443, "kind": "connect", "action": "intercept"},
                    {"host": "*.example.test", "ports": "8000-8100", "kind": "any", "action": "intercept"},
                    {"host": "192.0.2.1", "ports": [80, 443], "kind": "plain", "action": "intercept"},
                    {"host": "blocked.test", "action": "deny"}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(policy.rule_count(), 4);
        assert_eq!(policy.action_counts()["intercept"], 3);
        let target_policy = policy.to_target_policy().unwrap();
        let (host, _) = target("example.test", 443);
        assert_eq!(
            target_policy.resolve_connect(&host, 443),
            ConnectAction::Intercept
        );
        let (sub, _) = target("a.example.test", 8005);
        assert_eq!(
            target_policy.resolve_connect(&sub, 8005),
            ConnectAction::Intercept
        );
        // Suffix safety: label-boundary only.
        let (evil, _) = target("evil-example.test", 8005);
        assert_eq!(
            target_policy.resolve_connect(&evil, 8005),
            ConnectAction::Deny
        );
        let (ip, _) = target("192.0.2.1", 80);
        assert!(target_policy.allows_plain(&ip, 80));
        // Summary echoes normalized facts and no key material.
        let summary = policy.normalized_summary().to_string();
        assert!(summary.contains(INTERCEPT_POLICY_VERSION));
        assert!(!summary.contains("PRIVATE KEY"));
    }

    #[test]
    fn unknown_version_is_rejected_fail_closed() {
        let err = FilePolicy::parse_json(
            br#"{"version": "eggreplay-intercept-policy/v99", "rules": []}"#,
        )
        .unwrap_err();
        assert!(matches!(err, PolicyFileError::UnsupportedVersion(_)));
    }

    #[test]
    fn missing_version_is_rejected_fail_closed() {
        let err = FilePolicy::parse_json(br#"{"rules": []}"#).unwrap_err();
        assert!(matches!(err, PolicyFileError::Invalid(_)));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let err = FilePolicy::parse_json(
            br#"{"version": "eggreplay-intercept-policy/v1", "evil": true}"#,
        )
        .unwrap_err();
        assert!(matches!(err, PolicyFileError::Invalid(_)));
        let err = FilePolicy::parse_json(
            br#"{"version": "eggreplay-intercept-policy/v1",
                "rules": [{"host": "a.test", "action": "deny", "script": "x"}]}"#,
        )
        .unwrap_err();
        assert!(matches!(err, PolicyFileError::Invalid(_)));
    }

    #[test]
    fn unknown_actions_kinds_and_ports_are_rejected() {
        for body in [
            r#"{"host": "a.test", "action": "allow"}"#,
            r#"{"host": "a.test", "action": "deny", "kind": "socks"}"#,
            r#"{"host": "a.test", "action": "deny", "ports": "lots"}"#,
            r#"{"host": "a.test", "action": "deny", "ports": 0}"#,
            r#"{"host": "a.test", "action": "deny", "ports": "0-10"}"#,
            r#"{"host": "not a host!!", "action": "deny"}"#,
            r#"{"host": "user@a.test", "action": "deny"}"#,
        ] {
            let doc = format!("{{\"version\": {INTERCEPT_POLICY_VERSION:?}, \"rules\": [{body}]}}");
            assert!(
                FilePolicy::parse_json(doc.as_bytes()).is_err(),
                "must reject: {body}"
            );
        }
    }

    #[test]
    fn mixed_tunnel_and_intercept_rules_are_rejected() {
        let err = FilePolicy::parse_json(
            br#"{
                "version": "eggreplay-intercept-policy/v1",
                "default_connect_action": "tunnel",
                "rules": [
                    {"host": "a.test", "action": "intercept"}
                ]
            }"#,
        )
        .unwrap_err();
        assert_eq!(err, PolicyFileError::MixedActions);
    }

    #[test]
    fn oversized_and_unbounded_inputs_are_rejected() {
        assert_eq!(
            FilePolicy::parse_json(&vec![b' '; MAX_POLICY_FILE_BYTES + 1]).unwrap_err(),
            PolicyFileError::TooLarge
        );
        let mut rules = String::new();
        for index in 0..=MAX_POLICY_RULES {
            use std::fmt::Write as _;
            if index > 0 {
                rules.push(',');
            }
            write!(
                rules,
                "{{\"host\": \"h{index}.test\", \"action\": \"deny\"}}"
            )
            .expect("test rule rendering must succeed");
        }
        let doc = format!("{{\"version\": {INTERCEPT_POLICY_VERSION:?}, \"rules\": [{rules}]}}");
        assert!(matches!(
            FilePolicy::parse_json(doc.as_bytes()).unwrap_err(),
            PolicyFileError::TooManyRules(_)
        ));
    }

    #[test]
    fn flag_rules_build_a_coherent_policy() {
        let policy = policy_from_flags(
            ConnectAction::Tunnel,
            &["*.example.test".to_owned()],
            &["evil.example.test".to_owned()],
        )
        .unwrap();
        assert_eq!(policy.rule_count(), 2);
        let target_policy = policy.to_target_policy().unwrap();
        let (sub, _) = target("a.example.test", 443);
        assert_eq!(
            target_policy.resolve_connect(&sub, 443),
            ConnectAction::Tunnel
        );
        let (evil, _) = target("evil.example.test", 443);
        assert_eq!(
            target_policy.resolve_connect(&evil, 443),
            ConnectAction::Deny
        );
        assert!(policy_from_flags(ConnectAction::Deny, &[String::new()], &[]).is_err());
    }

    #[test]
    fn error_strings_carry_no_key_material() {
        for error in [
            PolicyFileError::TooLarge,
            PolicyFileError::Io("read"),
            PolicyFileError::Invalid("x".to_owned()),
            PolicyFileError::UnsupportedVersion("v9".to_owned()),
            PolicyFileError::TooManyRules(999),
            PolicyFileError::MixedActions,
        ] {
            let text = error.to_string();
            assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
            assert!(!text.contains("BEGIN"), "leak in {text}");
        }
    }
}
