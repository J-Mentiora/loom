/**
 * TypeScript types for the loom SDK.
 *
 * Types mirror wire shapes in:
 *   - systems/loom-rpc/modules/core_service_adapter/interfaces.rs
 *   - systems/loom-rpc/modules/host_service_adapter/interfaces.rs
 *   - systems/loom-rpc/modules/error_translator/interfaces.rs
 *
 * camelCase property names are used for TypeScript idiom; wire uses snake_case
 * (transformation happens in transport/session layers).
 */

export type LoomErrorCode =
  | "protocol_auth_required"
  | "protocol_malformed"
  | "schema_violation"
  | "method_not_found"
  | "session_not_found"
  | "session_aborted"
  | "budget_exceeded"
  | "surface_trap"
  | "surface_unavailable"
  | "vault_grant_not_found"
  | "vault_grant_revoked"
  | "vault_credential_type_unsupported"
  | "store_integrity_failed"
  | "internal_error";

export interface SessionInfo {
  sessionId: string;
  status: string;
  createdAtMs: number;
}

export interface SessionInspection {
  sessionId: string;
  atAction: number | null;
  manifestSummary: Record<string, unknown>;
}

export interface DiffReport {
  a: string;
  b: string;
  diff: Record<string, unknown>;
}

export interface ExportInfo {
  sessionId: string;
  format: string;
  artifactRef: string;
}

export interface ValidationResult {
  sessionId: string;
  passed: boolean;
  reasons: string[];
}

export interface GrantInfo {
  grantId: string;
  origin: string;
  scopes: string[];
  ttlSeconds: number;
  label: string;
}

export interface Receipt {
  actionHash: string;
  outcomeHash: string;
  emittedAtMs: number;
}

/**
 * A JSON Schema object as returned by rpc.schemas(). Covers the common fields;
 * additional keywords are carried in the extension index signature.
 */
export interface JsonSchemaObject {
  type?: "string" | "number" | "integer" | "boolean" | "array" | "object" | "null";
  properties?: Record<string, JsonSchemaObject>;
  items?: JsonSchemaObject;
  required?: string[];
  description?: string;
  enum?: unknown[];
}

export interface MethodSchema {
  method: string;
  request: JsonSchemaObject;
  response: JsonSchemaObject;
}

export interface SchemaRegistry {
  methods: MethodSchema[];
  sourceWitSha256: string;
}
