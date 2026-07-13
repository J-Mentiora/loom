"""
Audio verbs + retrieval helpers (voice-call-io task 08, AC8/AC11).

Verifies the SDK exposes ``inject_audio`` / ``start_audio_capture`` /
``stop_audio_capture`` mirroring the recording-verb precedent, surfaces the
wire receipt's ``audio_after_hash`` / ``audio_stop_reason``, and fetches the
captured WAV bytes via ``content.get`` (``fetch_audio_capture`` /
``save_audio_capture``) without touching the internal ContentStore API.
"""

from __future__ import annotations

import json
import logging
from typing import Any

import loom
from loom.types import Receipt

AUDIO_HASH = "9f" * 32
WAV_BYTES = b"RIFF\x24\x00\x00\x00WAVEfmt " + bytes(28)


def _decode_payload(params: dict) -> dict:
    return json.loads(bytes(params["action"]["payload"]).decode("utf-8"))


def _stop_receipt(stop_reason: str) -> dict:
    return {
        "action_hash": "a" * 64,
        "outcome_hash": "b" * 64,
        "emitted_at_ms": 1_730_000_000_000,
        "audio_after_hash": AUDIO_HASH,
        "audio_stop_reason": stop_reason,
    }


def _register_audio(daemon: Any, stop_reason: str = "explicit") -> dict[str, Any]:
    state: dict[str, Any] = {"inject": None, "start": None, "stop_calls": 0}

    def _inject(params: dict) -> dict:
        state["inject"] = _decode_payload(params)
        return {"action_hash": "a" * 64, "outcome_hash": "b" * 64, "emitted_at_ms": 1}

    def _start(params: dict) -> dict:
        state["start"] = _decode_payload(params)
        return {"action_hash": "a" * 64, "outcome_hash": "b" * 64, "emitted_at_ms": 1}

    def _stop(params: dict) -> dict:
        state["stop_calls"] += 1
        return _stop_receipt(stop_reason)

    daemon.register_handler("action.web.inject_audio", _inject)
    daemon.register_handler("action.web.start_audio_capture", _start)
    daemon.register_handler("action.web.stop_audio_capture", _stop)
    return state


def _register_content_get(daemon: Any) -> dict[str, Any]:
    state: dict[str, Any] = {"artifact_ref": None}

    def _content_get(params: dict) -> dict:
        state["artifact_ref"] = params["artifact_ref"]
        return {
            "artifact_ref": params["artifact_ref"],
            "data_hex": WAV_BYTES.hex(),
            "size_bytes": len(WAV_BYTES),
        }

    daemon.register_handler("content.get", _content_get)
    return state


def test_inject_audio_threads_payload(daemon):
    state = _register_audio(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.inject_audio(blob_ref=AUDIO_HASH, await_playout=True)

    assert state["inject"]["blob_ref"] == AUDIO_HASH
    assert state["inject"]["await_playout"] is True
    assert "audio_b64" not in state["inject"]
    assert isinstance(receipt, Receipt)


def test_start_audio_capture_threads_caps(daemon):
    state = _register_audio(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        s.start_audio_capture(max_duration_ms=30_000, max_bytes=1_000_000)

    assert state["start"]["max_duration_ms"] == 30_000
    assert state["start"]["max_bytes"] == 1_000_000


def test_stop_audio_capture_surfaces_audio_fields(daemon):
    _register_audio(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        s.start_audio_capture()
        receipt = s.stop_audio_capture()

    assert receipt.audio_after_hash == AUDIO_HASH
    assert receipt.audio_stop_reason == "explicit"


def test_stop_audio_capture_warns_on_cap_truncation(daemon, caplog):
    for reason in ("byte_cap", "duration_cap"):
        caplog.clear()
        _register_audio(daemon, stop_reason=reason)
        with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
            with caplog.at_level(logging.WARNING, logger="loom"):
                receipt = s.stop_audio_capture()

        assert receipt.audio_stop_reason == reason
        warn_messages = [rec.message for rec in caplog.records]
        assert any(reason in m and "truncated" in m for m in warn_messages), warn_messages


def test_stop_audio_capture_does_not_warn_on_explicit(daemon, caplog):
    _register_audio(daemon, stop_reason="explicit")
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        with caplog.at_level(logging.WARNING, logger="loom"):
            s.stop_audio_capture()

    assert not caplog.records


def test_fetch_audio_capture_returns_bytes(daemon):
    _register_audio(daemon)
    content_state = _register_content_get(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.stop_audio_capture()
        data = s.fetch_audio_capture(receipt)

    assert data == WAV_BYTES
    assert content_state["artifact_ref"] == AUDIO_HASH


def test_save_audio_capture_writes_file(daemon, tmp_path):
    _register_audio(daemon)
    _register_content_get(daemon)
    out = tmp_path / "answer.wav"
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.stop_audio_capture()
        s.save_audio_capture(receipt, str(out))

    assert out.read_bytes() == WAV_BYTES


def test_fetch_audio_capture_without_hash_raises(daemon):
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        bare = Receipt._from_dict({"action_hash": "a" * 64, "outcome_hash": "b" * 64})
        try:
            s.fetch_audio_capture(bare)
        except ValueError as exc:
            assert "audio_after_hash" in str(exc)
        else:
            raise AssertionError("expected ValueError for receipt without audio_after_hash")
