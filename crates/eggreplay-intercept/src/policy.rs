//! Transport-neutral explicit-proxy target policy.
//!
//! The policy decides, before any route establishment or certificate action,
//! whether a plain absolute-form HTTP request or a `CONNECT` authority may
//! proceed. It is transport-neutral: it only inspects normalized host/port
//! facts, never sockets, TLS state, or credentials.
//!
//! Matching rules:
//!
//! - [`HostMatch::ExactDns`] matches one normalized DNS name.
//! - [`HostMatch::SuffixDns`] matches an explicitly configured parent name on
//!   a label boundary: the parent itself and any name ending in
//!   `.parent`. `evil-example.com` never matches suffix `example.com`.
//! - [`HostMatch::ExactIp`] matches one IPv4/IPv6 literal.
//! - Ports match exactly, within a bounded range/set, or (rarely) any port.
//! - Request kind distinguishes plain proxy HTTP from `CONNECT`.
//!
//! Deterministic priority: the most specific matching rule wins; at equal
//! specificity deny wins over allow. Anything unmatched denies (fail closed).
//! There is no allow-all default.

use std::net::IpAddr;

use thiserror::Error;

/// Maximum number of rules in one [`TargetPolicy`].
pub const MAX_POLICY_RULES: usize = 128;
/// Maximum length of one configured host pattern.
pub const MAX_HOST_PATTERN_LEN: usize = 253;
/// Maximum entries in one [`PortMatch::Set`].
pub const MAX_PORT_SET_SIZE: usize = 64;
/// Preview length used to keep diagnostics bounded.
const DIAGNOSTIC_PREVIEW_LEN: usize = 64;

/// Fail-closed policy construction/normalization errors.
///
/// Messages carry only truncated input previews, never credentials or key
/// material.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// A host pattern or authority host is malformed.
    #[error("invalid host: {0}")]
    InvalidHost(String),
    /// A port pattern or authority port is malformed.
    #[error("invalid port: {0}")]
    InvalidPort(String),
    /// A rule or policy composition is invalid.
    #[error("invalid rule: {0}")]
    InvalidRule(String),
    /// The rule list exceeds [`MAX_POLICY_RULES`].
    #[error("too many policy rules: {0}")]
    TooManyRules(usize),
}

/// Truncate an input preview so diagnostics stay bounded.
fn preview(input: &str) -> String {
    let mut out: String = input.chars().take(DIAGNOSTIC_PREVIEW_LEN).collect();
    if input.chars().count() > DIAGNOSTIC_PREVIEW_LEN {
        out.push_str("...");
    }
    out
}

/// A normalized dial/match host: lowercase DNS or a parsed IP literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormalizedHost {
    /// Normalized DNS name (lowercase, no trailing dot).
    Dns(String),
    /// Parsed IP literal.
    Ip(IpAddr),
}

impl NormalizedHost {
    /// Render the host for dialing or diagnostics (never credentials).
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::Dns(name) => name.clone(),
            Self::Ip(addr) => addr.to_string(),
        }
    }
}

/// A normalized `host[:port]` target with an explicit port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTarget {
    /// Normalized host.
    pub host: NormalizedHost,
    /// Explicit port (authority default already applied).
    pub port: u16,
}

/// Normalize a bare host (no port, no userinfo) for matching or dialing.
///
/// Accepts DNS names (case-insensitive, one optional trailing dot),
/// IPv4 literals, and bracketed or bare IPv6 literals. Rejects userinfo
/// (`@`), empty input, overlong names, non-ASCII names (IDNA is rejected in
/// M013B), and malformed labels.
///
/// # Errors
///
/// Returns [`PolicyError::InvalidHost`] when the input is malformed.
pub fn normalize_host(input: &str) -> Result<NormalizedHost, PolicyError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::InvalidHost("empty host".to_owned()));
    }
    if trimmed.len() > MAX_HOST_PATTERN_LEN + 2 {
        return Err(PolicyError::InvalidHost(format!(
            "host too long: {}",
            preview(trimmed)
        )));
    }
    if trimmed.contains('@') {
        return Err(PolicyError::InvalidHost(
            "host must not contain userinfo".to_owned(),
        ));
    }
    if trimmed.contains(['/', '?', '#']) {
        return Err(PolicyError::InvalidHost(format!(
            "host contains path syntax: {}",
            preview(trimmed)
        )));
    }
    // Bracketed IPv6 literal: `[::1]`.
    if let Some(stripped) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return match stripped.parse::<IpAddr>() {
            Ok(IpAddr::V6(addr)) => Ok(NormalizedHost::Ip(IpAddr::V6(addr))),
            Ok(_) | Err(_) => Err(PolicyError::InvalidHost(format!(
                "invalid bracketed host: {}",
                preview(trimmed)
            ))),
        };
    }
    if trimmed.contains('[') || trimmed.contains(']') {
        return Err(PolicyError::InvalidHost(format!(
            "unbalanced brackets: {}",
            preview(trimmed)
        )));
    }
    // IP literal fast path (IPv4 or bare IPv6).
    if let Ok(addr) = trimmed.parse::<IpAddr>() {
        return Ok(NormalizedHost::Ip(addr));
    }
    // A bare colon can only be an IPv6 literal (already failed to parse) or
    // a port separator; bare hosts must not carry one.
    if trimmed.contains(':') {
        return Err(PolicyError::InvalidHost(format!(
            "invalid host: {}",
            preview(trimmed)
        )));
    }
    normalize_dns(trimmed)
}

/// Normalize a DNS name: lowercase, single trailing dot removed, ASCII-only.
fn normalize_dns(input: &str) -> Result<NormalizedHost, PolicyError> {
    if !input.is_ascii() {
        return Err(PolicyError::InvalidHost(
            "non-ASCII names are rejected".to_owned(),
        ));
    }
    let lower = input.to_ascii_lowercase();
    let without_dot = lower.strip_suffix('.').unwrap_or(&lower);
    if without_dot.is_empty() {
        return Err(PolicyError::InvalidHost("empty host".to_owned()));
    }
    if without_dot.len() > MAX_HOST_PATTERN_LEN {
        return Err(PolicyError::InvalidHost(format!(
            "host too long: {}",
            preview(input)
        )));
    }
    if without_dot.chars().any(char::is_whitespace) {
        return Err(PolicyError::InvalidHost(format!(
            "host contains whitespace: {}",
            preview(input)
        )));
    }
    for label in without_dot.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(PolicyError::InvalidHost(format!(
                "invalid DNS label: {}",
                preview(input)
            )));
        }
        let bytes = label.as_bytes();
        if bytes.first().is_some_and(|b| *b == b'-') || bytes.last().is_some_and(|b| *b == b'-') {
            return Err(PolicyError::InvalidHost(format!(
                "invalid DNS label: {}",
                preview(input)
            )));
        }
        if !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        {
            return Err(PolicyError::InvalidHost(format!(
                "invalid DNS characters: {}",
                preview(input)
            )));
        }
    }
    Ok(NormalizedHost::Dns(without_dot.to_owned()))
}

/// Normalize an authority-form string (`host`, `host:port`, `[v6]:port`).
///
/// `default_port` applies when no explicit port is present. Rejects userinfo,
/// empty hosts, port zero, unparseable ports, and path/query fragments.
///
/// # Errors
///
/// Returns [`PolicyError`] when the authority is malformed or unbounded.
pub fn normalize_authority(
    authority: &str,
    default_port: u16,
) -> Result<NormalizedTarget, PolicyError> {
    let trimmed = authority.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::InvalidHost("empty authority".to_owned()));
    }
    if trimmed.contains('@') {
        return Err(PolicyError::InvalidHost(
            "authority must not contain userinfo".to_owned(),
        ));
    }
    if trimmed.contains(['/', '?', '#']) {
        return Err(PolicyError::InvalidHost(format!(
            "authority contains path syntax: {}",
            preview(trimmed)
        )));
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| {
            PolicyError::InvalidHost(format!("unbalanced brackets: {}", preview(trimmed)))
        })?;
        let host_part = &rest[..end];
        let remainder = &rest[end + 1..];
        let host = match host_part.parse::<IpAddr>() {
            Ok(IpAddr::V6(addr)) => NormalizedHost::Ip(IpAddr::V6(addr)),
            Ok(_) | Err(_) => {
                return Err(PolicyError::InvalidHost(format!(
                    "invalid bracketed host: {}",
                    preview(trimmed)
                )));
            }
        };
        let port = if remainder.is_empty() {
            default_port
        } else if let Some(port_str) = remainder.strip_prefix(':') {
            parse_port(port_str)?
        } else {
            return Err(PolicyError::InvalidHost(format!(
                "invalid authority suffix: {}",
                preview(trimmed)
            )));
        };
        return Ok(NormalizedTarget { host, port });
    }
    // Reject a second authority smuggled behind whitespace.
    if trimmed.contains(char::is_whitespace) {
        return Err(PolicyError::InvalidHost(format!(
            "authority contains whitespace: {}",
            preview(trimmed)
        )));
    }
    match trimmed.rsplit_once(':') {
        Some((host_part, port_str)) => {
            if host_part.contains(':') {
                return Err(PolicyError::InvalidHost(format!(
                    "unbracketed IPv6 must use brackets: {}",
                    preview(trimmed)
                )));
            }
            Ok(NormalizedTarget {
                host: normalize_host(host_part)?,
                port: parse_port(port_str)?,
            })
        }
        None => Ok(NormalizedTarget {
            host: normalize_host(trimmed)?,
            port: default_port,
        }),
    }
}

/// Parse and validate one port number; port zero is rejected fail-closed.
fn parse_port(port_str: &str) -> Result<u16, PolicyError> {
    let port: u16 = port_str
        .parse()
        .map_err(|_| PolicyError::InvalidPort(format!("invalid port: {}", preview(port_str))))?;
    if port == 0 {
        return Err(PolicyError::InvalidPort("port zero is rejected".to_owned()));
    }
    Ok(port)
}

/// Host matching dimension of a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMatch {
    /// One exact normalized DNS name.
    ExactDns(String),
    /// An explicitly configured parent; matches the parent and subdomains on
    /// a label boundary only.
    SuffixDns(String),
    /// One exact IP literal.
    ExactIp(IpAddr),
}

impl HostMatch {
    /// Build an exact-DNS matcher from caller input (normalized).
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidHost`] for malformed or overlong names.
    pub fn exact_dns(input: &str) -> Result<Self, PolicyError> {
        match normalize_host(input)? {
            NormalizedHost::Dns(name) => Ok(Self::ExactDns(name)),
            NormalizedHost::Ip(_) => Err(PolicyError::InvalidHost(
                "exact DNS pattern must not be an IP literal".to_owned(),
            )),
        }
    }

    /// Build a suffix matcher from caller input (normalized).
    ///
    /// A leading `*.` or `.` is accepted and stripped; the remainder is the
    /// parent name matched on a label boundary.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidHost`] for malformed or overlong names.
    pub fn suffix_dns(input: &str) -> Result<Self, PolicyError> {
        let trimmed = input.trim();
        let parent = trimmed
            .strip_prefix("*.")
            .or_else(|| trimmed.strip_prefix('.'))
            .unwrap_or(trimmed);
        if parent.len() > MAX_HOST_PATTERN_LEN {
            return Err(PolicyError::InvalidHost(format!(
                "suffix too long: {}",
                preview(input)
            )));
        }
        match normalize_host(parent)? {
            NormalizedHost::Dns(name) => Ok(Self::SuffixDns(name)),
            NormalizedHost::Ip(_) => Err(PolicyError::InvalidHost(
                "DNS suffix must not be an IP literal".to_owned(),
            )),
        }
    }

    /// Build an exact-IP matcher.
    #[must_use]
    pub fn exact_ip(addr: IpAddr) -> Self {
        Self::ExactIp(addr)
    }

    /// Test a normalized target host against this matcher.
    #[must_use]
    pub fn matches(&self, target: &NormalizedHost) -> bool {
        match (self, target) {
            (Self::ExactDns(pattern), NormalizedHost::Dns(name)) => pattern == name,
            (Self::SuffixDns(parent), NormalizedHost::Dns(name)) => {
                name == parent || name.ends_with(&format!(".{parent}"))
            }
            (Self::ExactIp(pattern), NormalizedHost::Ip(addr)) => pattern == addr,
            _ => false,
        }
    }

    /// Host specificity contribution: exact IP > exact DNS > suffix.
    fn specificity(&self) -> u32 {
        match self {
            Self::ExactIp(_) => 3000,
            Self::ExactDns(_) => 2000,
            Self::SuffixDns(parent) => 1000 + u32::try_from(parent.len().min(253)).unwrap_or(253),
        }
    }
}

/// Port matching dimension of a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortMatch {
    /// One exact port.
    Exact(u16),
    /// Inclusive bounded range.
    Range {
        /// Range start (inclusive).
        start: u16,
        /// Range end (inclusive).
        end: u16,
    },
    /// Bounded set of ports.
    Set(Vec<u16>),
    /// Any port. Permitted only inside an explicit rule; the policy default
    /// still denies unmatched targets.
    Any,
}

impl PortMatch {
    /// Build an exact-port matcher; port zero is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidPort`] for port zero.
    pub fn exact(port: u16) -> Result<Self, PolicyError> {
        if port == 0 {
            return Err(PolicyError::InvalidPort("port zero is rejected".to_owned()));
        }
        Ok(Self::Exact(port))
    }

    /// Build a bounded inclusive range matcher.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidPort`] for inverted ranges or port zero.
    pub fn range(start: u16, end: u16) -> Result<Self, PolicyError> {
        if start == 0 || end == 0 {
            return Err(PolicyError::InvalidPort("port zero is rejected".to_owned()));
        }
        if start > end {
            return Err(PolicyError::InvalidPort(format!(
                "inverted port range: {start}-{end}"
            )));
        }
        Ok(Self::Range { start, end })
    }

    /// Build a bounded port-set matcher.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidPort`] for empty or oversized sets, or
    /// when the set contains port zero.
    pub fn set(ports: Vec<u16>) -> Result<Self, PolicyError> {
        if ports.is_empty() {
            return Err(PolicyError::InvalidPort("empty port set".to_owned()));
        }
        if ports.len() > MAX_PORT_SET_SIZE {
            return Err(PolicyError::InvalidPort(format!(
                "port set too large: {}",
                ports.len()
            )));
        }
        if ports.contains(&0) {
            return Err(PolicyError::InvalidPort("port zero is rejected".to_owned()));
        }
        Ok(Self::Set(ports))
    }

    /// Test a port against this matcher.
    #[must_use]
    pub fn matches(&self, port: u16) -> bool {
        match self {
            Self::Exact(expected) => *expected == port,
            Self::Range { start, end } => (*start..=*end).contains(&port),
            Self::Set(ports) => ports.contains(&port),
            Self::Any => true,
        }
    }

    /// Port specificity contribution: exact > set > range > any.
    fn specificity(&self) -> u32 {
        match self {
            Self::Exact(_) => 300,
            Self::Set(ports) => {
                200 + u32::try_from(MAX_PORT_SET_SIZE - ports.len().min(MAX_PORT_SET_SIZE))
                    .unwrap_or(0)
            }
            Self::Range { .. } => 100,
            Self::Any => 0,
        }
    }
}

/// Which proxy request kind a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// Plain absolute-form HTTP request.
    Plain,
    /// `CONNECT` authority.
    Connect,
    /// Either kind.
    Any,
}

impl RequestKind {
    /// Test whether this rule kind covers an observed kind.
    #[must_use]
    pub fn covers(&self, observed: Self) -> bool {
        matches!((self, observed), (a, b) if *a == Self::Any || *a == b)
    }

    /// Kind specificity contribution: kind-specific beats `Any`.
    fn specificity(self) -> u32 {
        match self {
            Self::Any => 0,
            Self::Plain | Self::Connect => 10,
        }
    }
}

/// Allow/deny verdict of one rule evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    /// Permit the target.
    Allow,
    /// Refuse the target.
    Deny,
}

/// `CONNECT` action selected by [`TargetPolicy::resolve_connect`].
///
/// M013B supports deny and tunnel; M013D adds opt-in interception. An
/// `Intercept` default arms TLS termination for allowed targets, but the
/// proxy only honors it when an interception issuer is configured (see
/// [`crate::mitm`]); without one the request fails closed before `200`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectAction {
    /// Refuse the tunnel with an ordinary denial; never dial.
    Deny,
    /// Establish a raw opaque tunnel after dialing succeeds.
    Tunnel,
    /// Terminate client TLS with a freshly issued exact-host leaf and record
    /// the decrypted HTTP/1.1 exchange (M013D; policy opt-in only).
    Intercept,
}

/// One allow/deny rule over host, ports, and request kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Host matcher.
    pub host: HostMatch,
    /// Port matcher.
    pub ports: PortMatch,
    /// Request-kind matcher.
    pub kind: RequestKind,
    /// Verdict when this rule is the most specific match.
    pub action: RuleAction,
}

impl Rule {
    /// Build a rule. Ports and hosts are already validated by their
    /// constructors; this only assembles the rule.
    #[must_use]
    pub fn new(host: HostMatch, ports: PortMatch, kind: RequestKind, action: RuleAction) -> Self {
        Self {
            host,
            ports,
            kind,
            action,
        }
    }

    /// Test a normalized target against this rule.
    fn matches(&self, host: &NormalizedHost, port: u16, kind: RequestKind) -> bool {
        self.host.matches(host) && self.ports.matches(port) && self.kind.covers(kind)
    }

    /// Total specificity score used for deterministic priority.
    fn specificity(&self) -> u32 {
        self.host.specificity() + self.ports.specificity() + self.kind.specificity()
    }
}

/// Transport-neutral proxy target policy owned by `eggreplay-intercept`.
///
/// Unmatched targets deny. `CONNECT` targets that evaluate to allow resolve
/// through `default_connect_action` (deny or tunnel).
#[derive(Debug, Clone)]
pub struct TargetPolicy {
    rules: Vec<Rule>,
    default_connect_action: ConnectAction,
}

impl TargetPolicy {
    /// Build a policy from explicit rules (bounded to [`MAX_POLICY_RULES`]).
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::TooManyRules`] when the rule list is unbounded.
    pub fn new(
        rules: Vec<Rule>,
        default_connect_action: ConnectAction,
    ) -> Result<Self, PolicyError> {
        if rules.len() > MAX_POLICY_RULES {
            return Err(PolicyError::TooManyRules(rules.len()));
        }
        Ok(Self {
            rules,
            default_connect_action,
        })
    }

    /// Build a deny-everything policy (no rules).
    #[must_use]
    pub fn deny_all(default_connect_action: ConnectAction) -> Self {
        Self {
            rules: Vec::new(),
            default_connect_action,
        }
    }

    /// Number of configured rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// The configured default `CONNECT` action for allowed targets.
    #[must_use]
    pub fn default_connect_action(&self) -> ConnectAction {
        self.default_connect_action
    }

    /// Evaluate a normalized target: most specific matching rule wins; deny
    /// wins ties; unmatched targets deny.
    #[must_use]
    pub fn evaluate(&self, host: &NormalizedHost, port: u16, kind: RequestKind) -> RuleAction {
        let mut best: Option<(u32, RuleAction)> = None;
        for rule in &self.rules {
            if !rule.matches(host, port, kind) {
                continue;
            }
            let score = rule.specificity();
            best = Some(match best {
                None => (score, rule.action),
                Some((best_score, best_action)) => {
                    if score > best_score
                        || (score == best_score
                            && rule.action == RuleAction::Deny
                            && best_action == RuleAction::Allow)
                    {
                        (score, rule.action)
                    } else {
                        (best_score, best_action)
                    }
                }
            });
        }
        best.map_or(RuleAction::Deny, |(_, action)| action)
    }

    /// Convenience check for plain absolute-form HTTP targets.
    #[must_use]
    pub fn allows_plain(&self, host: &NormalizedHost, port: u16) -> bool {
        self.evaluate(host, port, RequestKind::Plain) == RuleAction::Allow
    }

    /// Resolve a `CONNECT` target to deny, tunnel, or intercept.
    ///
    /// Denied targets (by rule or by default) resolve to
    /// [`ConnectAction::Deny`]; allowed targets resolve through the
    /// configured default action. Deny wins ties at equal specificity, and
    /// unmatched targets deny, so `Intercept` only fires for explicitly
    /// allowed targets under an intercept default. There is no allow-all
    /// default.
    #[must_use]
    pub fn resolve_connect(&self, host: &NormalizedHost, port: u16) -> ConnectAction {
        match self.evaluate(host, port, RequestKind::Connect) {
            RuleAction::Deny => ConnectAction::Deny,
            RuleAction::Allow => self.default_connect_action,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns(name: &str) -> NormalizedHost {
        normalize_host(name).expect("valid test host")
    }

    #[test]
    fn normalization_handles_case_trailing_dot_and_brackets() {
        assert_eq!(
            normalize_host("Example.TEST.").unwrap(),
            NormalizedHost::Dns("example.test".to_owned())
        );
        assert_eq!(
            normalize_host("[::1]").unwrap(),
            NormalizedHost::Ip("::1".parse().unwrap())
        );
        assert_eq!(
            normalize_host("127.0.0.1").unwrap(),
            NormalizedHost::Ip("127.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn normalization_rejects_userinfo_idna_and_empty() {
        assert!(normalize_host("user@example.test").is_err());
        assert!(normalize_host("").is_err());
        assert!(normalize_host("münchen.test").is_err());
        assert!(normalize_host("bad..label.test").is_err());
        assert!(normalize_authority("user@example.test:80", 80).is_err());
    }

    #[test]
    fn authority_defaults_and_validates_ports() {
        assert_eq!(normalize_authority("example.test", 80).unwrap().port, 80);
        assert_eq!(
            normalize_authority("example.test:8080", 80).unwrap().port,
            8080
        );
        assert_eq!(normalize_authority("[::1]:443", 80).unwrap().port, 443);
        assert!(normalize_authority("example.test:0", 80).is_err());
        assert!(normalize_authority("example.test:notaport", 80).is_err());
        assert!(normalize_authority("[::1", 443).is_err());
    }

    #[test]
    fn suffix_matches_on_label_boundary_only() {
        let suffix = HostMatch::suffix_dns("example.com").unwrap();
        assert!(suffix.matches(&dns("example.com")));
        assert!(suffix.matches(&dns("a.example.com")));
        assert!(suffix.matches(&dns("a.b.example.com")));
        assert!(!suffix.matches(&dns("evil-example.com")));
        assert!(!suffix.matches(&dns("example.com.evil.test")));
    }

    #[test]
    fn deny_wins_at_equal_specificity_and_default_denies() {
        let policy = TargetPolicy::new(
            vec![
                Rule::new(
                    HostMatch::exact_dns("example.test").unwrap(),
                    PortMatch::exact(80).unwrap(),
                    RequestKind::Any,
                    RuleAction::Allow,
                ),
                Rule::new(
                    HostMatch::exact_dns("example.test").unwrap(),
                    PortMatch::exact(80).unwrap(),
                    RequestKind::Any,
                    RuleAction::Deny,
                ),
            ],
            ConnectAction::Tunnel,
        )
        .unwrap();
        assert_eq!(
            policy.evaluate(&dns("example.test"), 80, RequestKind::Plain),
            RuleAction::Deny
        );
        assert_eq!(
            policy.evaluate(&dns("other.test"), 80, RequestKind::Plain),
            RuleAction::Deny
        );
    }

    #[test]
    fn most_specific_rule_wins() {
        let policy = TargetPolicy::new(
            vec![
                Rule::new(
                    HostMatch::suffix_dns("example.test").unwrap(),
                    PortMatch::Any,
                    RequestKind::Any,
                    RuleAction::Deny,
                ),
                Rule::new(
                    HostMatch::exact_dns("api.example.test").unwrap(),
                    PortMatch::exact(80).unwrap(),
                    RequestKind::Plain,
                    RuleAction::Allow,
                ),
            ],
            ConnectAction::Deny,
        )
        .unwrap();
        assert!(policy.allows_plain(&dns("api.example.test"), 80));
        assert!(!policy.allows_plain(&dns("other.example.test"), 80));
    }

    #[test]
    fn connect_resolution_uses_default_action_for_allowed_targets() {
        let host = dns("example.test");
        let tunnel_policy = TargetPolicy::new(
            vec![Rule::new(
                HostMatch::exact_dns("example.test").unwrap(),
                PortMatch::exact(443).unwrap(),
                RequestKind::Any,
                RuleAction::Allow,
            )],
            ConnectAction::Tunnel,
        )
        .unwrap();
        assert_eq!(
            tunnel_policy.resolve_connect(&host, 443),
            ConnectAction::Tunnel
        );
        let deny_policy = TargetPolicy::new(
            vec![Rule::new(
                HostMatch::exact_dns("example.test").unwrap(),
                PortMatch::exact(443).unwrap(),
                RequestKind::Any,
                RuleAction::Allow,
            )],
            ConnectAction::Deny,
        )
        .unwrap();
        assert_eq!(deny_policy.resolve_connect(&host, 443), ConnectAction::Deny);
        assert_eq!(
            tunnel_policy.resolve_connect(&host, 8443),
            ConnectAction::Deny
        );
    }

    #[test]
    fn rule_bounds_are_enforced() {
        assert!(PortMatch::set(vec![]).is_err());
        assert!(PortMatch::set(vec![80; MAX_PORT_SET_SIZE + 1]).is_err());
        assert!(PortMatch::range(8080, 80).is_err());
        assert!(PortMatch::exact(0).is_err());
        let rules = (0..=MAX_POLICY_RULES)
            .map(|_| {
                Rule::new(
                    HostMatch::exact_dns("example.test").unwrap(),
                    PortMatch::Any,
                    RequestKind::Any,
                    RuleAction::Deny,
                )
            })
            .collect();
        assert!(TargetPolicy::new(rules, ConnectAction::Deny).is_err());
    }
}
