# Release process

Run formatting, workspace check, clippy, all-feature tests (`--locked` in CI),
local qualification, and dependency/advisory review (`cargo audit`). Build the
standalone CLI (`eggreplay`) for Linux/macOS/Windows on stable plus Linux MSRV
1.89 with the declared dependency revisions (EggFetch 0.2.0, pinned EggServe
and Eggress revisions, narrow `pproxy-compat` only). Record exact commands,
CI run links, test counts, supported HTTP/1.1 matrix (H2/H3 deferred), known
limitations, and deferred milestones (M009–M014) in the v0.1 corrective closure
record before tagging. Package artifact is the `eggreplay` binary; no Python
bindings or HAR artifacts in v0.1.
