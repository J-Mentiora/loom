"""
loom SDK codegen — regenerates ``loom/types.py`` from the daemon's rpc.schemas() response.

Usage::

    python -m loom._codegen \\
        --socket /path/to/loom.sock \\
        --token <token> \\
        --output loom/types.py

When ``--output`` is omitted, writes to ``loom/types.py`` relative to this file's directory.

Run this whenever the daemon schema changes. The generated file must be
committed to the repository. No hand-editing of `types.py` is permitted.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from loom._errors import LoomConnectionError, LoomRPCError, LoomTokenError
from loom._transport import LoomTransport


def _method_to_python_name(method: str) -> str:
    """Convert 'session.create' → 'SessionCreate'."""
    return "".join(part.capitalize() for part in method.replace(".", "_").split("_"))


def _json_type_to_python(schema: dict) -> str:
    """Best-effort conversion of a JSON Schema type hint to Python annotation."""
    t = schema.get("type", "object")
    if t == "string":
        return "str"
    if t == "integer":
        return "int"
    if t == "boolean":
        return "bool"
    if t == "number":
        return "float"
    if t == "array":
        items = schema.get("items", {})
        return f"list[{_json_type_to_python(items)}]"
    return "dict"


def _generate_types(schema_registry: dict) -> str:
    """Generate types.py content from a SchemaRegistry dict."""
    methods = schema_registry.get("methods", [])
    source_sha = schema_registry.get("source_wit_sha256", "")

    lines: list[str] = [
        "# GENERATED — do not edit by hand. Run: python -m loom._codegen",
        f"# source_wit_sha256: {source_sha}",
        "from __future__ import annotations",
        "",
        "from dataclasses import dataclass",
        "from enum import Enum",
        "from typing import Any",
        "",
        "",
        "class LoomErrorCode(str, Enum):",
        '    """Stable error codes (BC-RPC-03)."""',
        "",
        '    protocol_auth_required = "protocol_auth_required"',
        '    protocol_malformed = "protocol_malformed"',
        '    schema_violation = "schema_violation"',
        '    method_not_found = "method_not_found"',
        '    session_not_found = "session_not_found"',
        '    session_aborted = "session_aborted"',
        '    budget_exceeded = "budget_exceeded"',
        '    surface_trap = "surface_trap"',
        '    surface_unavailable = "surface_unavailable"',
        '    vault_grant_not_found = "vault_grant_not_found"',
        '    vault_grant_revoked = "vault_grant_revoked"',
        '    vault_credential_type_unsupported = "vault_credential_type_unsupported"',
        '    store_integrity_failed = "store_integrity_failed"',
        '    internal_error = "internal_error"',
        "",
        "",
    ]

    # Emit a dataclass for each method's response type
    for m in methods:
        method_name = m.get("method", "")
        class_name = _method_to_python_name(method_name) + "Result"
        response_schema = m.get("response", {})
        props = response_schema.get("properties", {})

        lines.append("@dataclass")
        lines.append(f"class {class_name}:")
        docstring = f'"""Response type for {method_name}. Auto-generated from rpc.schemas()."""'
        lines.append(f"    {docstring}")
        lines.append("")

        if props:
            for field_name, field_schema in props.items():
                py_type = _json_type_to_python(field_schema)
                lines.append(f"    {field_name}: {py_type}")
        else:
            lines.append("    data: dict")

        lines.append("")
        lines.append("    @classmethod")
        lines.append(f'    def _from_dict(cls, d: dict) -> "{class_name}":')
        if props:
            args = ", ".join(
                f"{k}=d.get({k!r})"
                if field_schema.get("type") not in ("string",)
                else f'{k}=d.get({k!r}, "")'
                for k, field_schema in props.items()
            )
            lines.append(f"        return cls({args})")
        else:
            lines.append("        return cls(data=d)")
        lines.append("")
        lines.append("")

    # Emit method name constants for schema parity verification
    lines.append("# Method names — for parity verification")
    lines.append("SCHEMA_METHODS: list[str] = [")
    for m in methods:
        lines.append(f"    {m.get('method', '')!r},")
    lines.append("]")
    lines.append("")

    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Regenerate loom/types.py from the daemon rpc.schemas() endpoint."
    )
    parser.add_argument("--socket", help="Path to loom.sock (default: auto-detect)")
    parser.add_argument("--token", help="Auth token (default: auto-discover)")
    parser.add_argument(
        "--output",
        help="Output path (default: loom/types.py next to this file)",
    )
    args = parser.parse_args()

    try:
        with LoomTransport(args.socket, args.token) as t:
            registry = t.call("rpc.schemas", {})
    except (LoomConnectionError, LoomRPCError, LoomTokenError) as exc:
        print(f"ERROR: failed to fetch rpc.schemas(): {exc}", file=sys.stderr)
        return 1

    if not registry or "methods" not in registry:
        print("ERROR: rpc.schemas() returned unexpected response", file=sys.stderr)
        return 1

    output_path = Path(args.output) if args.output else Path(__file__).parent / "types.py"
    content = _generate_types(registry)
    output_path.write_text(content, encoding="utf-8")
    print(f"Generated {output_path} ({len(registry['methods'])} methods)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
