"""
Hand-written admin payload types.

``daemon.health`` returns a complex typed payload that isn't fully
captured by the rpc.schemas() registry (BUILTIN_CORE methods bypass
schema validation in loom-rpc, so their response shapes aren't visible
there). These dataclasses mirror the Rust structs
in ``loom-rpc/src/rpc_handlers/rpc_handlers.rs`` and are kept in sync via
the drift test in ``tests/test_types_drift.py::test_handwritten_admin_types_field_drift``.

When you change a field on the Rust side, mirror the change here and
the drift test will catch it.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal, Optional

ProbeStatus = Literal["ok", "timeout", "error"]


@dataclass(frozen=True)
class ShimBreakerSnapshot:
    """Per-shim circuit-breaker snapshot from ``daemon.health``."""

    shim_id: str
    state: str  # "closed" | "open" | "half-open"
    consecutive_failures: int
    opened_at_ms: Optional[int]


@dataclass(frozen=True)
class ShimDeepHealth:
    """Per-shim deep-probe outcome from ``daemon.health({deep: true})``."""

    shim_id: str
    daemon_restart_count: int
    daemon_last_restart_at_ms: Optional[int]
    shim_uptime_ms: int
    shim_requests_served: int
    shim_last_request_at_ms: Optional[int]
    probe_status: ProbeStatus


@dataclass(frozen=True)
class DaemonHealthResult:
    """Wire payload returned by ``daemon.health``."""

    active_sessions: int
    shim_breaker_states: list[ShimBreakerSnapshot]
    otel_exporter: str  # "enabled" | "disabled" | "unwired"
    deep: Optional[list[ShimDeepHealth]]
