"""
Receipt wire-field parsing: status / error / navigate tier-2 / evaluate fields.

The daemon returns failed actions as SUCCESSFUL JSON-RPC results whose receipt
carries ``status="error"`` plus a typed error payload (loom-rpc
host_service_adapter ``Receipt`` / ``ReceiptStatus`` / ``ReceiptError``) — the
SDK must surface those fields so failed actions are distinguishable from
successes.
"""

from __future__ import annotations

import loom
from loom.types import Receipt, ReceiptError

# Wire-shaped dict mirroring loom-rpc host_service_adapter::Receipt
# (snake_case keys; status enum is success|error|aborted).
WIRE_ERROR_RECEIPT = {
    "action_id": 7,
    "session_id": "01TEST" + "0" * 20,
    "status": "error",
    "timing_ticks": 12,
    "side_effects": [],
    "error": {
        "kind": "dns_failure",
        "detail": {
            "url": "https://nonexistent.invalid",
            "chromium_error": "net::ERR_NAME_NOT_RESOLVED",
        },
    },
    "action_hash": "a" * 64,
    "outcome_hash": "b" * 64,
    "emitted_at_ms": 1_730_000_000_000,
    "url": "https://nonexistent.invalid",
    "final_url": "https://nonexistent.invalid",
    "settle_until": "settled",
    "settle_outcome": "reached",
}


def test_error_receipt_surfaces_status_and_error():
    r = Receipt._from_dict(WIRE_ERROR_RECEIPT)
    assert r.status == "error"
    assert r.ok is False
    assert isinstance(r.error, ReceiptError)
    assert r.error.kind == "dns_failure"
    assert r.error.detail["chromium_error"] == "net::ERR_NAME_NOT_RESOLVED"
    assert r.url == "https://nonexistent.invalid"
    # Existing fields still parse.
    assert r.action_hash == "a" * 64
    assert r.settle_outcome == "reached"


def test_success_receipt_with_navigate_tier2_fields():
    d = {
        "status": "success",
        "error": None,  # the daemon serializes error: null on success
        "action_hash": "a" * 64,
        "outcome_hash": "b" * 64,
        "emitted_at_ms": 1,
        "url": "https://example.com",
        "final_url": "https://example.com/home",
        "title": "Example",
        "status_code": 200,
        "dom_snapshot_hash": "c" * 64,
        "screenshot_after_hash": "d" * 64,
    }
    r = Receipt._from_dict(d)
    assert r.ok is True
    assert r.status == "success"
    assert r.error is None
    assert r.final_url == "https://example.com/home"
    assert r.title == "Example"
    assert r.status_code == 200
    assert r.dom_snapshot_hash == "c" * 64
    assert r.screenshot_after_hash == "d" * 64


def test_evaluate_receipt_surfaces_return_value():
    r = Receipt._from_dict({"status": "success", "return_value_json": "42"})
    assert r.return_value_json == "42"
    assert r.return_value_blob_ref is None


def test_aborted_receipt_is_not_ok():
    r = Receipt._from_dict({"status": "aborted"})
    assert r.status == "aborted"
    assert r.ok is False


def test_legacy_receipt_without_status_defaults_to_success():
    """Backward compatible: receipts lacking the new fields parse as before."""
    r = Receipt._from_dict({"action_hash": "a", "outcome_hash": "b", "emitted_at_ms": 5})
    assert r.status == "success"
    assert r.ok is True
    assert r.error is None
    assert r.url is None
    assert r.return_value_json is None


def test_navigate_failure_visible_via_session(daemon):
    """End-to-end: a navigate failure returned as a JSON-RPC SUCCESS with
    status="error" is visible on the typed Receipt."""

    def _navigate(_params: dict) -> dict:
        return {
            "status": "error",
            "error": {
                "kind": "http_status",
                "detail": {"status_code": 404, "url": "https://example.com/missing"},
            },
            "url": "https://example.com/missing",
            "status_code": 404,
        }

    daemon.register_handler("action.web.navigate", _navigate)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.navigate("https://example.com/missing")
    assert receipt.ok is False
    assert receipt.status == "error"
    assert receipt.error is not None
    assert receipt.error.kind == "http_status"
    assert receipt.status_code == 404
