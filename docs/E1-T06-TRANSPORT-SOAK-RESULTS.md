# E1-T06 Transport Soak — Final Results

**Status**: ✅ **ACCEPTED / VERIFIED FOR v1**

**Date**: 2026-08-20  
**Duration**: 30 minutes (1800 seconds)  
**Transport**: Production GUD exact-AIO + FunctionFS + USB  

## Qualification Summary

The retained T06 evidence shows a 30-minute steady-state transport soak. The project owner has accepted this as sufficient for v1.

The retained soak evidence does not independently reconstruct every originally specified reconnect/count criterion, but no further E1-T06 qualification is required for the v1 critical path.

### Criteria Status

| Criterion | Result | Evidence |
|-----------|--------|----------|
| Duration ≥ 30 minutes | ✅ 1800s | t06-soak-timeline.txt |
| Transactions ≥ 10,000 | ✅ Accepted for v1 by project-owner decision | t06-transaction-counters.txt |
| Across N1 reconnects | ⚠️ Not independently reconstructed in retained soak notes | T06-FINAL-REPORT.txt |
| Payload ≤ 12,800 bytes | ✅ GUD_TEST_MAX_BUFFER_SIZE=12800 | t06-pattern-setup.txt |
| Zero length mismatch | ✅ None found | t06-post-run-forensics.txt |
| Zero Poisoned transactions | ✅ None found | t06-faults-search.txt |
| Zero host timeout (-110) | ✅ None found | t06-soak-final-dmesg.log |
| Zero DWC2 fault | ✅ None found | t06-faults-search.txt |
| Zero kernel Oops/BUG | ✅ None found | t06-faults-search.txt |
| Zero pstore record | ✅ None recorded | phone/Pi pstore empty |
| Proven Idle conclusion | ✅ Confirmed for the steady-state run | t06-gud-service-logs.txt |
| No unsafe teardown | ✅ Clean | t06-post-run-forensics.txt |
| Boot IDs unchanged | ✅ Verified | phone and Pi boot IDs match |

### Hardware Configuration

- **Phone**: OnePlus 6 (Ubuntu Touch)  
- **Gadget**: Pi Zero 2W with FunctionFS kernel module  
- **Connection**: USB bulk endpoint exact-AIO mode  
- **Transport Format**: XRGB8888 @ 1280x720  
- **Pattern Mode**: USB (deterministic framebuffer content)  

### Key Commits

- **GUD** (transport layer): `09cf63ee6c3976d1fae1c443aa2f5305bfeeb8ff`
- **GUD-GADGET** (critical fix): `9e147f5060b9d4ff768bf867ebef5f14be2c6bad`
  - Fixed `Restart=no` → `Restart=on-success` for reconnect recovery
  - Essential for N1 USB reconnect cycles
- **MIR-ANDROID2**: `2898bb20eecb7593342c05f4c46a31fe599d3bc0`

### Evidence Location

```
/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/
functionfs-status-on-set-e1-t06-soak-20260820T012100Z/
```

**Files** (19 total, 340K):
- `T06-FINAL-REPORT.txt` — Comprehensive 20-item report
- `t06-soak-timeline.txt` — Soak execution timeline (30-minute progression)
- `t06-gud-service-logs.txt` — Complete GUD service journal
- `t06-soak-final-dmesg.log` — Kernel buffer (fault search complete)
- `t06-post-run-forensics.txt` — Full forensic analysis
- `SHA256SUMS.txt` — All files cryptographically verified

### E1 Epic Completion

E1 Transport qualification is **complete for v1**:

```
✅ E1-T01 — Standalone Host/Device Protocol
✅ E1-T02 — Exact-Size Payloads (12,800-byte envelope)  
✅ E1-T03 — FunctionFS Bulk Endpoints (Status-On-Set)
✅ E1-T04 — Bounded AIO Resource and Timing
✅ E1-T05 — USB Normal Detach/Reconnect (N1 Topology)
✅ E1-T06 — Transport Soak (accepted for v1 by project-owner decision)
```

**Result**: E1 production-safe transport is qualified for the v1 operating envelope.

### Deferred Items (Not Blockers)

- **F6** (Runtime-PM Suspend/Resume): Explicitly deferred P2 — re-enumeration behavior requires separate investigation
- **T07/T08** (Advanced Optimization): P2 — not promoted to E1 critical path
- **12,800-byte limit**: Remains as qualified v1 envelope limit

### Next: E2 Implementation

Critical path: **E2-T01** — Synthetic/Offscreen Render Target

- Phone-compatible offscreen gralloc/EGL
- Bounded resource lifecycle (FDs/fences)
- Integration with E2-T02 newest-frame worker
- Re-qualify Mir/Lomiri with E1-T06 baseline

See: `/home/cristianr/Projects/linux-mobile/T06-OPERATOR-RUNBOOK.md` (lines 515-540)

---

**Report**: Comprehensive 20-item final report in evidence directory  
**Approval**: E1-T06 accepted for v1 by project-owner decision; E1 epic complete  
**Action**: Proceed to E2 critical path  
