# Eggress routing

Direct networking is always the default. The optional `EggressDialer`
delegates listener-free TCP route establishment to `eggress-outbound`; EggFetch
continues to own logical Host/SNI, TLS, HTTP framing, pooling, and body
semantics. Unsupported or failed chains fail closed and typed Eggress facts
are mapped without parsing display strings. The TCP adapter makes no H3
support claim.
