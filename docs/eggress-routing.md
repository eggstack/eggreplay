# Eggress routing

Direct networking is always the default (`--route direct`). The optional
`EggressDialer` delegates listener-free TCP route establishment to
`eggress-outbound` using only the narrow `pproxy-compat` feature as a
construction grammar (`OutboundConnector::from_pproxy_uri`); extended, SSH,
QUIC, listener, or server surfaces are never enabled. EggFetch continues to
own logical Host/SNI, TLS, HTTP framing, pooling, and body semantics.
Unsupported or failed chains fail closed with credential-redacted diagnostics
and never fall back to direct; typed Eggress facts are mapped without parsing
display strings. The TCP adapter makes no H3 support claim. CLI `record`,
`replay`, and `test` expose the same `--route` surface; `physical_route`
(`direct`/`eggress` with redacted description) is recorded in flows.
