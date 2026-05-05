## Summary

This plan implements TTY-aware output formatting for a CLI tool, replacing JSON-only output with human-readable formats when connected to a terminal. While the plan is well-structured and comprehensive, it contains no security considerations whatsoever. The implementation touches output formatting, error handling, and configuration systems without addressing any of the critical security risks inherent in a multi-tenant CX chatbot platform that processes customer PII and integrates with external systems.

## Issues Found

- **[SEVERITY: critical]** No tenant isolation in output formatting
  - Impact: The new `emit()` function and curated renderers have no tenant filtering. When rendering lists (session.list, vault.list), there's no guarantee that only the current tenant's data is shown. A user could potentially see sessions or vault entries from other tenants.
  - Recommendation: Add tenant filtering to all list renderers. Pass tenant context through the `emit()` function. Ensure `session.list` and `vault.list` only show items belonging to the authenticated tenant.

- **[SEVERITY: critical]** Sensitive data exposure in pretty output
  - Impact: While D-18 adds redaction for sensitive fields in the tail block, the main curated output has no redaction. OAuth tokens, API keys, and customer PII could be displayed in terminal output, potentially logged by terminal emulators or screen recording software.
  - Recommendation: Apply comprehensive redaction to ALL output, not just the tail block. Mask OAuth tokens, API keys, and PII fields in the main rendered output. Consider a `--show-secrets` flag for cases where full values are needed.

- **[SEVERITY: high]** No consideration for log leakage
  - Impact: The plan adds colored error output to stderr but doesn't address what customer data might be included in those error messages. Stack traces could contain conversation content, PII, or API keys.
  - Recommendation: Sanitize all error messages before display. Strip customer data from stack traces. Add tests to verify no PII appears in error output.

- **[SEVERITY: high]** Subprocess tests with real data risk
  - Impact: Integration tests spawn actual CLI processes. If these tests use real customer data or connect to real systems, they could leak data through process listings, temp files, or network traffic.
  - Recommendation: Ensure all tests use mock data only. Add assertions that no real tenant IDs, customer data, or external API calls occur during tests.

- **[SEVERITY: medium]** No audit logging for data access
  - Impact: The new output system doesn't log what data was displayed to which user. In a security incident, there's no way to determine what customer data was potentially exposed.
  - Recommendation: Add audit logging to track which tenant's data was displayed, when, and in what format (json/pretty/quiet).

- **[SEVERITY: medium]** Global flag injection risk
  - Impact: The `--quiet` flag could be used to suppress security warnings or audit messages, potentially hiding malicious activity.
  - Recommendation: Ensure security-critical messages are never suppressed by `--quiet`. Add tests to verify security warnings always appear regardless of output mode.

## Strengths

- Comprehensive test coverage including byte-exact regression tests
- Well-structured implementation with clear separation of concerns
- Thoughtful handling of edge cases like empty lists and malformed data
- Good consideration of terminal detection and color handling
- Clear documentation of breaking changes

## Verdict

**BLOCK**

This plan must not proceed without addressing critical tenant isolation and data leakage concerns. For a multi-tenant CX platform handling customer PII, implementing output formatting without any security controls is unacceptable and could lead to severe data breaches.