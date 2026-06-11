"""
Drift guards for the hand-maintained SDK type surface.

``loom/types.py`` and ``loom/_admin_types.py`` are hand-written mirrors of
the daemon's wire types (there is no codegen — a schema-driven generator
was removed after its output drifted irreconcilably from the shipped
module). These tests pin the hand-maintained surface instead:

  - every name ``loom/__init__.py`` imports from ``loom.types`` must exist
    there (the failure mode that breaks ``import loom``), and every
    ``loom.__all__`` entry must resolve;
  - the hand-written admin dataclasses must field-match their Rust structs.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

SDK_ROOT = Path(__file__).parent.parent
LOOM_PKG = SDK_ROOT / "loom"


# ─── Public type-surface guard ───────────────────────────────────────────


def _names_imported_from_types() -> set[str]:
    """Extract the names ``loom/__init__.py`` imports from ``loom.types``."""
    src = (LOOM_PKG / "__init__.py").read_text()
    m = re.search(r"from loom\.types import \(([^)]*)\)", src)
    assert m, "loom/__init__.py no longer has a `from loom.types import (...)` block"
    return {n.strip().rstrip(",") for n in m.group(1).split("\n") if n.strip().rstrip(",")}


def test_types_module_defines_every_name_init_imports():
    """types.py must define every name __init__.py imports from it.

    This is the regression the old codegen shipped: regenerating types.py
    dropped SessionInfo/Receipt/etc. and `import loom` died with ImportError.
    """
    import loom.types as types_mod

    missing = {n for n in _names_imported_from_types() if not hasattr(types_mod, n)}
    assert not missing, f"loom.types is missing names imported by __init__.py: {missing}"


def test_all_public_names_resolve():
    """Every loom.__all__ entry must be an attribute of the package."""
    import loom

    missing = {n for n in loom.__all__ if not hasattr(loom, n)}
    assert not missing, f"loom.__all__ names that do not resolve: {missing}"


# ─── LoomErrorCode wire-set drift guard ──────────────────────────────────
# The daemon serializes error codes via loom_shared::LoomErrorCode::as_wire
# (loom-shared/src/error_format.rs). The Python enum must carry the full
# wire set so users can match daemon errors without undocumented string
# literals (the audit found request_cancelled / request_timeout /
# too_many_requests / session_killed / unknown_profile / ... missing).


def _extract_as_wire_codes(rs_path: Path) -> set[str]:
    """Regex-extract the wire strings from the `as_wire` match arms."""
    src = rs_path.read_text()
    start = src.index("pub fn as_wire")
    end = src.index("pub fn from_wire")
    return set(re.findall(r'=>\s*"([a-z0-9_]+)"', src[start:end]))


def test_error_code_enum_matches_rust_as_wire():
    """Python LoomErrorCode values == the Rust as_wire set (set-equality)."""
    from loom.types import LoomErrorCode

    rs_path = SDK_ROOT.parent / "loom-shared" / "src" / "error_format.rs"
    if not rs_path.exists():
        pytest.skip("Rust source not co-located (running from installed wheel?)")

    rust_codes = _extract_as_wire_codes(rs_path)
    assert rust_codes, "failed to extract as_wire arms from error_format.rs"
    py_codes = {m.value for m in LoomErrorCode}
    assert rust_codes == py_codes, (
        f"LoomErrorCode wire drift between Rust and Python:\n"
        f"  Rust only: {sorted(rust_codes - py_codes)}\n"
        f"  Py only:   {sorted(py_codes - rust_codes)}\n"
    )


def test_error_code_unknown_degrades_to_internal():
    """Unknown / future wire codes parse to `internal` instead of raising."""
    from loom.types import LoomErrorCode

    assert LoomErrorCode("some_future_code") is LoomErrorCode.internal
    assert LoomErrorCode(42) is LoomErrorCode.internal


def test_error_code_accepts_legacy_kebab_spellings():
    """Kebab-case spellings (pre-consolidation clients) map to the member."""
    from loom.types import LoomErrorCode

    assert LoomErrorCode("session-not-found") is LoomErrorCode.session_not_found
    assert LoomErrorCode("browser-not-found") is LoomErrorCode.browser_not_found


# ─── Hand-written admin types drift guard ────────────────────────────────
# The admin RPC wrappers (kill_session, daemon_health) have HAND-WRITTEN
# result types in `python-sdk/loom/_admin_types.py`. This guard catches
# camelCase / snake_case drift between the Rust Server struct (snake_case
# JSON output) and the Python dataclass field names.
#
# It does NOT compile Rust source — it parses the source text via regex.
# Rust struct file: ../../loom-rpc/src/rpc_handlers/rpc_handlers.rs
# Python types file: ../loom/_admin_types.py


def _extract_rust_struct_fields(rs_path: Path, struct_name: str) -> set[str]:
    """Cheap regex extraction of `pub field_name: ` fields from a Rust
    struct block. Skips field-less generic-only structs."""
    src = rs_path.read_text()
    # Find `pub struct StructName {` ... `}` block.
    m = re.search(
        rf"pub struct {re.escape(struct_name)}\s*\{{(.*?)\n\}}",
        src,
        re.DOTALL,
    )
    if not m:
        return set()
    body = m.group(1)
    return set(re.findall(r"^\s*pub\s+([a-z_]\w*)\s*:", body, re.MULTILINE))


def _extract_python_dataclass_fields(py_path: Path, class_name: str) -> set[str]:
    """Cheap extraction of dataclass field names from a Python source.

    Slices from `class Name:` to the next `class ` (or EOF), then collects
    indented `field_name: TypeHint` lines while skipping docstring blocks
    and blank lines.
    """
    src = py_path.read_text()
    m = re.search(rf"class {re.escape(class_name)}\b[^:]*:\s*\n", src)
    if not m:
        return set()
    rest = src[m.end():]
    # Cut off at next top-level definition.
    cut = re.search(r"^(class |def |\w)", rest, re.MULTILINE)
    body = rest[: cut.start()] if cut else rest
    # Strip docstring block (line starting with `    """` and continuing
    # until closing `"""`).
    body = re.sub(r'(?ms)^\s+""".*?"""\s*$', "", body)
    return set(re.findall(r"^\s+([a-z_]\w*)\s*:", body, re.MULTILINE))


def test_handwritten_admin_types_field_drift():
    """Hand-written ShimDeepHealth in types.py must match Rust struct fields."""
    rs_path = (
        Path(__file__).parent.parent.parent
        / "loom-rpc"
        / "src"
        / "rpc_handlers"
        / "rpc_handlers.rs"
    )
    py_path = Path(__file__).parent.parent / "loom" / "_admin_types.py"
    if not rs_path.exists():
        pytest.skip("Rust source not co-located (running from installed wheel?)")
    if not py_path.exists():
        pytest.skip("Python _admin_types.py not found")

    # ShimDeepHealth: hand-written; check field names align.
    rust_fields = _extract_rust_struct_fields(rs_path, "ShimDeepHealth")
    py_fields = _extract_python_dataclass_fields(py_path, "ShimDeepHealth")
    assert py_fields, "ShimDeepHealth dataclass missing from _admin_types.py"
    assert rust_fields == py_fields, (
        f"ShimDeepHealth field drift between Rust and Python:\n"
        f"  Rust only: {rust_fields - py_fields}\n"
        f"  Py only:   {py_fields - rust_fields}\n"
    )
    # Also check ShimBreakerSnapshot (already existed on Rust side; we
    # added a hand-written mirror for symmetry).
    rust_breaker = _extract_rust_struct_fields(rs_path, "ShimBreakerSnapshot")
    py_breaker = _extract_python_dataclass_fields(py_path, "ShimBreakerSnapshot")
    assert rust_breaker == py_breaker, (
        f"ShimBreakerSnapshot field drift:\n"
        f"  Rust only: {rust_breaker - py_breaker}\n"
        f"  Py only:   {py_breaker - rust_breaker}\n"
    )


# (audit 2026-06-10, "DaemonHealth wire struct grew orphan_browser_trees and
# oldest_active_session_age_secs but both SDK mirrors lack them"): the Python
# DaemonHealthResult dataclass now mirrors the full Rust struct (the TS mirror
# was already updated in #163). This guard extends the same regex drift check
# to DaemonHealth itself so the blind spot can't recur. FIXED.
def test_daemon_health_mirror_field_drift():
    rs_path = (
        Path(__file__).parent.parent.parent
        / "loom-rpc"
        / "src"
        / "rpc_handlers"
        / "rpc_handlers.rs"
    )
    py_path = Path(__file__).parent.parent / "loom" / "_admin_types.py"
    if not rs_path.exists():
        pytest.skip("Rust source not co-located (running from installed wheel?)")
    if not py_path.exists():
        pytest.skip("Python _admin_types.py not found")

    rust_fields = _extract_rust_struct_fields(rs_path, "DaemonHealth")
    py_fields = _extract_python_dataclass_fields(py_path, "DaemonHealthResult")
    assert rust_fields, "DaemonHealth struct missing from rpc_handlers.rs"
    assert py_fields, "DaemonHealthResult dataclass missing from _admin_types.py"
    assert rust_fields == py_fields, (
        f"DaemonHealth field drift between Rust and Python:\n"
        f"  Rust only: {rust_fields - py_fields}\n"
        f"  Py only:   {py_fields - rust_fields}\n"
    )
