"""
Python types for the loom SDK.

Hand-maintained mirrors of the daemon's wire types. When the wire shape
changes in loom-rpc / loom-shared, edit this file in the same change —
``tests/test_types_drift.py`` pins the public surface (and the error-code
set) against the Rust sources.

Types mirror the wire types in:
  - loom-rpc/src/core_service_adapter/core_service_adapter.rs
  - loom-rpc/src/host_service_adapter/host_service_adapter.rs
  - loom-shared/src/error_format.rs (LoomErrorCode)
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum
from typing import Any


class LoomErrorCode(StrEnum):
    """Stable error codes mirroring ``loom_shared::LoomErrorCode::as_wire``.

    The full wire set (BC-RPC-03 + the consolidated loom-shared codes);
    ``tests/test_types_drift.py`` pins set-equality against the Rust source.
    Unknown codes never raise: ``LoomErrorCode(code)`` degrades to
    ``internal``, mirroring the tolerant ``from_wire`` on the Rust side.
    """

    # ---- Core / lifecycle ----
    session_not_found = "session_not_found"
    session_already_closed = "session_already_closed"
    session_aborted = "session_aborted"
    session_killed = "session_killed"
    surface_trap = "surface_trap"
    # ---- Vault ----
    vault_rejection = "vault_rejection"
    vault_grant_expired = "vault_grant_expired"
    vault_grant_revoked = "vault_grant_revoked"
    vault_unknown_label = "vault_unknown_label"
    vault_permission_denied = "vault_permission_denied"
    vault_backend_unavailable = "vault_backend_unavailable"
    vault_backend_timeout = "vault_backend_timeout"
    vault_non_interactive_prompt = "vault_non_interactive_prompt"
    vault_internal = "vault_internal"
    vault_invalid_label = "vault_invalid_label"
    # ---- Budget ----
    budget_exceeded = "budget_exceeded"
    budget_rate_limited = "budget_rate_limited"
    # ---- Content store ----
    store_integrity_failed = "store_integrity_failed"
    store_not_found = "store_not_found"
    store_full_no_evictable = "store_full_no_evictable"
    # ---- Manifest / replay ----
    manifest_corrupt = "manifest_corrupt"
    replay_divergence = "replay_divergence"
    replay_missing_blob = "replay_missing_blob"
    not_replayable = "not_replayable"
    # ---- Shim / transport ----
    shim_failure = "shim_failure"
    shim_timeout = "shim_timeout"
    shim_breaker_open = "shim_breaker_open"
    # ---- RPC / IO ----
    rpc_invalid_request = "rpc_invalid_request"
    rpc_auth_failed = "rpc_auth_failed"
    rpc_schema_violation = "rpc_schema_violation"
    request_timeout = "request_timeout"
    request_cancelled = "request_cancelled"
    too_many_requests = "too_many_requests"
    # session.create at the LOOM_MAX_CONCURRENT_SESSIONS cap. Capacity, not
    # a fault: back off and retry after a slot frees, or run
    # `loom session reap`; error data carries {active, cap, hint}.
    session_cap_exceeded = "session_cap_exceeded"
    transport_dropped = "transport_dropped"
    llm_cache_miss = "llm_cache_miss"
    io = "io"
    # ---- Safety profile ----
    schema_violation = "schema_violation"
    safe_profile_download_blocked = "safe_profile_download_blocked"
    profile_restricted = "profile_restricted"
    # ---- Browser / launch ----
    browser_not_found = "browser_not_found"
    # ---- Daemon protocol / validation layer (BC-RPC-03 frozen wire) ----
    protocol_auth_required = "protocol_auth_required"
    protocol_malformed = "protocol_malformed"
    method_not_found = "method_not_found"
    surface_unavailable = "surface_unavailable"
    session_closed = "session_closed"
    vault_grant_not_found = "vault_grant_not_found"
    vault_credential_type_unsupported = "vault_credential_type_unsupported"
    unknown_profile = "unknown_profile"
    invalid_network_mode = "invalid_network_mode"
    invalid_budget_key = "invalid_budget_key"
    invalid_capture_policy = "invalid_capture_policy"
    internal_error = "internal_error"
    # ---- Catch-alls ----
    invalid_argument = "invalid_argument"
    unsupported = "unsupported"
    internal = "internal"

    @classmethod
    def _missing_(cls, value: object) -> LoomErrorCode:
        """Tolerant decode mirroring ``LoomErrorCode::from_wire`` in Rust:
        accept legacy kebab-case spellings, and map any unrecognized code
        to ``internal`` instead of raising — so version skew against a
        newer daemon can never turn a typed error into a parse error."""
        if isinstance(value, str):
            snake = value.replace("-", "_")
            if snake != value and snake in cls._value2member_map_:
                return cls(snake)
        return cls.internal


@dataclass
class SessionInfo:
    """Response type for session.create / session.close / session.abort."""

    session_id: str
    status: str
    created_at_ms: int

    @classmethod
    def _from_dict(cls, d: dict) -> SessionInfo:
        return cls(
            session_id=d["session_id"],
            status=d["status"],
            created_at_ms=d.get("created_at_ms", 0),
        )


@dataclass
class SessionInspection:
    """Response type for session.inspect."""

    session_id: str
    at_action: int | None
    manifest_summary: dict

    @classmethod
    def _from_dict(cls, d: dict) -> SessionInspection:
        return cls(
            session_id=d["session_id"],
            at_action=d.get("at_action"),
            manifest_summary=d.get("manifest_summary", {}),
        )


@dataclass
class DiffReport:
    """Response type for session.diff."""

    a: str
    b: str
    diff: dict

    @classmethod
    def _from_dict(cls, d: dict) -> DiffReport:
        return cls(a=d["a"], b=d["b"], diff=d.get("diff", {}))


@dataclass
class ExportInfo:
    """Response type for session.export."""

    session_id: str
    format: str
    artifact_ref: str

    @classmethod
    def _from_dict(cls, d: dict) -> ExportInfo:
        return cls(
            session_id=d["session_id"],
            format=d["format"],
            artifact_ref=d.get("artifact_ref", ""),
        )


@dataclass
class ValidationResult:
    """Response type for session.validate.

    ``passed`` covers integrity only (hash chain + blob presence);
    ``replayable`` additionally applies the replay-refusal checks
    (crashed/aborted source, ``--no-determinism`` recording), so PASS does
    NOT imply replayable. ``replayable`` defaults to True when an older
    daemon omits the field (the pre-field meaning of PASS).
    """

    session_id: str
    passed: bool
    reasons: list[str]
    replayable: bool = True
    not_replayable_reason: str | None = None

    @classmethod
    def _from_dict(cls, d: dict) -> ValidationResult:
        return cls(
            session_id=d["session_id"],
            passed=d.get("passed", False),
            reasons=d.get("reasons", []),
            replayable=d.get("replayable", True),
            not_replayable_reason=d.get("not_replayable_reason"),
        )


@dataclass
class GrantInfo:
    """Response type for vault.grant. Contains grant_id only — no secrets."""

    grant_id: str
    origin: str
    scopes: list[str]
    ttl_seconds: int
    label: str

    @classmethod
    def _from_dict(cls, d: dict) -> GrantInfo:
        return cls(
            grant_id=d["grant_id"],
            origin=d.get("origin", ""),
            scopes=d.get("scopes", []),
            ttl_seconds=d.get("ttl_seconds", 0),
            label=d.get("label", ""),
        )


@dataclass
class ReceiptError:
    """Wire-shape error payload on a Receipt (host_service_adapter ReceiptError).

    ``kind`` is a stable typed string (e.g. ``"http_status"``, ``"dns_failure"``,
    ``"connect_refused"``, ``"tls_error"``, ``"url_blocked"``, ``"shim_failure"``);
    ``detail`` carries kind-specific fields (e.g. ``{"status_code", "url"}`` for
    ``http_status``). ``detail`` is None for kinds with no kind-specific data.
    """

    kind: str
    detail: Any | None = None

    @classmethod
    def _from_dict(cls, d: dict) -> ReceiptError:
        return cls(kind=d.get("kind", ""), detail=d.get("detail"))


@dataclass
class Receipt:
    """Action receipt returned by action.web.* methods (WIT receipt record)."""

    action_hash: str
    outcome_hash: str
    emitted_at_ms: int
    # settle-capture: readiness mode the capture was gated on
    # ("load" | "networkidle" | "settled"). Present on navigate receipts;
    # None for verbs without a readiness gate.
    settle_until: str | None = None
    # settle-capture: how the readiness wait ended
    # ("reached" | "timeout" | "dom_unstable"). "timeout"/"dom_unstable" mean
    # the bounded fallback fired (persistent connection / perpetual animation)
    # — the page never reached the requested readiness state.
    settle_outcome: str | None = None
    # Receipt-level outcome: "success" | "error" | "aborted" (ReceiptStatus
    # in loom-rpc host_service_adapter). Failed actions (DNS failure,
    # HTTP 4xx/5xx, blocked URL, shim failure) return as a SUCCESSFUL
    # JSON-RPC result whose receipt has status="error" — check ``ok`` /
    # ``status`` instead of relying on an exception.
    status: str = "success"
    # Typed failure payload when status != "success"; None on success.
    error: ReceiptError | None = None
    # ---- navigate tier-2 fields (None for non-navigate verbs) ----
    url: str | None = None
    final_url: str | None = None
    title: str | None = None
    status_code: int | None = None
    dom_snapshot_hash: str | None = None
    screenshot_after_hash: str | None = None
    # ---- evaluate tier fields ----
    # JS expression result, canonical-JSON encoded. None means "not an
    # evaluate action" or "result offloaded to the content store" (in which
    # case return_value_blob_ref carries the SHA-256).
    return_value_json: str | None = None
    return_value_blob_ref: str | None = None

    @property
    def ok(self) -> bool:
        """True when the action succeeded (``status == "success"``)."""
        return self.status == "success"

    @classmethod
    def _from_dict(cls, d: dict) -> Receipt:
        error = d.get("error")
        return cls(
            action_hash=d.get("action_hash", ""),
            outcome_hash=d.get("outcome_hash", ""),
            emitted_at_ms=d.get("emitted_at_ms", 0),
            settle_until=d.get("settle_until"),
            settle_outcome=d.get("settle_outcome"),
            status=d.get("status", "success"),
            error=ReceiptError._from_dict(error) if isinstance(error, dict) else None,
            url=d.get("url"),
            final_url=d.get("final_url"),
            title=d.get("title"),
            status_code=d.get("status_code"),
            dom_snapshot_hash=d.get("dom_snapshot_hash"),
            screenshot_after_hash=d.get("screenshot_after_hash"),
            return_value_json=d.get("return_value_json"),
            return_value_blob_ref=d.get("return_value_blob_ref"),
        )


@dataclass
class MethodSchema:
    """One method's request + response schemas from rpc.schemas()."""

    method: str
    request: dict
    response: dict

    @classmethod
    def _from_dict(cls, d: dict) -> MethodSchema:
        return cls(method=d["method"], request=d.get("request", {}), response=d.get("response", {}))


@dataclass
class SchemaRegistry:
    """Response type for rpc.schemas()."""

    methods: list[MethodSchema]
    source_wit_sha256: str

    @classmethod
    def _from_dict(cls, d: dict) -> SchemaRegistry:
        return cls(
            methods=[MethodSchema._from_dict(m) for m in d.get("methods", [])],
            source_wit_sha256=d.get("source_wit_sha256", ""),
        )
