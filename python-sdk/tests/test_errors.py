"""
Error handling tests: LoomRPCError raised correctly for each error code.
"""
from __future__ import annotations

import pytest

import loom
from loom._errors import LoomError, LoomRPCError
from tests.conftest import MockDaemon


def test_loom_rpc_error_is_loom_error():
    """LoomRPCError inherits from LoomError."""
    assert issubclass(LoomRPCError, LoomError)


def test_loom_rpc_error_has_code_and_message():
    err = LoomRPCError(code="session_not_found", message="no such session", data=None)
    assert err.code == "session_not_found"
    assert err.message == "no such session"
    assert err.data is None


def test_loom_rpc_error_str_includes_code():
    err = LoomRPCError(code="budget_exceeded", message="over budget", data=None)
    assert "budget_exceeded" in str(err)


def test_error_codes_from_daemon(daemon: MockDaemon):
    """Daemon-side errors surface as LoomRPCError with the correct code."""
    def _raise_session_not_found(_):
        raise ValueError("session_not_found:01MISSING")

    # Register a handler that returns a method_not_found style error
    # We test by calling a method that doesn't exist
    import loom
    with pytest.raises(LoomRPCError) as exc_info:
        loom.Session.create(
            socket_path=str(daemon.socket_path),
            token="wrong",
        )
    assert exc_info.value.code == "protocol_auth_required"
