/**
 * @mentiora-ai/loom-sdk — TypeScript client library for the loom browser-automation daemon.
 *
 * Quick start:
 *
 *   import { Session } from "@mentiora-ai/loom-sdk";
 *
 *   const session = await Session.create();
 *   await session.navigate("https://example.com");
 *   await session.close();
 *
 *   // Or with async disposal:
 *   await using session = await Session.create();
 *   await session.navigate("https://example.com");
 */

export { Session, sessionList, vaultGrant, vaultRevoke, vaultListGrants } from "./session.js";
export { LoomTransport } from "./transport.js";
export { LoomError, LoomRPCError, LoomConnectionError, LoomTokenError } from "./errors.js";
export type {
  SessionInfo,
  SessionInspection,
  Receipt,
  DiffReport,
  ExportInfo,
  ValidationResult,
  GrantInfo,
  MethodSchema,
  SchemaRegistry,
  LoomErrorCode,
} from "./types.js";
