"""Python package for the EggReplay Rust authorities."""

from enum import Enum

from ._native import (
    BodyReader,
    ComparisonPolicy,
    ConfigurationError,
    DiffFinding,
    EggReplayError,
    Fixture,
    FixtureError,
    FlowErrorInfo,
    Flow,
    MatchError,
    NetworkError,
    RegressionError,
    RegressionReport,
    RedactionConfig,
    Server,
    Request,
    Response,
    RouteSpecification,
    StreamTimingMode,
    WebSocketOptions,
    async_sleep,
    async_value,
    enum_values,
    recording_gateway,
    regress_flow,
    replay_server,
    validate_consumption_mode,
    validate_matcher_profile,
    validate_record_mode,
    validate_stream_timing,
    version,
)


def _enum_values(family: str) -> dict[str, str]:
    return {value.upper().replace("-", "_"): value for value in enum_values(family)}


MatcherProfile = Enum("MatcherProfile", _enum_values("matcher_profile"), type=str)
ConsumptionMode = Enum("ConsumptionMode", _enum_values("consumption_mode"), type=str)
RecordMode = Enum("RecordMode", _enum_values("record_mode"), type=str)
WebSocketRedaction = Enum("WebSocketRedaction", _enum_values("websocket_redaction"), type=str)


def record_policy(mode: RecordMode, *, fixture_exists: bool, upstream_configured: bool):
    effective_mode, upstream_enabled = validate_record_mode(
        mode.value, fixture_exists, upstream_configured
    )
    return {"mode": RecordMode(effective_mode), "upstream_enabled": upstream_enabled}


from ._lifecycle import fixture_context, use_fixture

__all__ = [
    "BodyReader",
    "ComparisonPolicy",
    "ConsumptionMode",
    "ConfigurationError",
    "DiffFinding",
    "EggReplayError",
    "Fixture",
    "FixtureError",
    "FlowErrorInfo",
    "Flow",
    "MatchError",
    "NetworkError",
    "RegressionError",
    "RegressionReport",
    "RedactionConfig",
    "Server",
    "Request",
    "Response",
    "RouteSpecification",
    "StreamTimingMode",
    "WebSocketOptions",
    "WebSocketRedaction",
    "MatcherProfile",
    "RecordMode",
    "record_policy",
    "recording_gateway",
    "regress_flow",
    "replay_server",
    "validate_consumption_mode",
    "validate_matcher_profile",
    "validate_record_mode",
    "validate_stream_timing",
    "fixture_context",
    "use_fixture",
    "async_sleep",
    "async_value",
    "version",
]
