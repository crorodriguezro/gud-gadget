# E2-T03 nonblocking qualification - partial hardware matrix

**Status: BLOCKED; not verified.**

This clean rerun followed recovery from the earlier Poisoned receiver preflight.
The committed producer-enqueue telemetry was deployed to the existing writable
`mirgud` bind override and verified by SHA-256 before activation.

Healthy, failing, and absent-device cases completed. The ticket cannot be
verified because there is no safe existing mechanism to create a *controlled*
slow GUD worker condition in the production Pi service:

- the release service exposes no delay-injection control;
- the existing `GUD_TEST_E1_T05_PROCESSING_BARRIER` is debug-only and injects a
  one-shot Poisoned lifecycle failure, not a slow presentation;
- using it for T03 would change the E1 fault semantics and violate the ticket
  boundary.

The observed healthy worker maximum was 3,143,113 us while producer enqueue
remained bounded, but it was not an intentionally controlled slow-GUD window
and therefore is retained as supporting evidence only.

No roadmap state was changed. Do not proceed to E2-T04 or E2-T05 from this
partial matrix.
