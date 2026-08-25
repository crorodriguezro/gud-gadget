# E3-B01 physical active detach/reconnect extension

## Result

**Strict E3-B01 verdict: FAIL.** The FunctionFS epoch-recovery implementation
worked after a fresh enumeration, but the OnePlus 6 did not autonomously
enumerate the powered hub after the physical reconnect. The documented
controller `device -> host` recovery was required, so this run cannot satisfy
the no-administrative-recovery success criterion.

## Timeline (UTC)

- `05:11:54`: explicitly activated direct Mir RGB565 with GUD LZ4 at
  `1280x720`; the operator confirmed the external desktop was visible.
- `05:14:44.239`: transaction 1791 completed in USB epoch 1.
- `05:14:44.280`: FunctionFS reported `SUSPEND` from proven Idle ownership;
  the exact-AIO queue was empty and no operation was owned.
- Phone kernel time `114316.737`: GUD device `1-1.3` disconnected and
  `/dev/dri/card1` disappeared. The managed MirGUD child exited.
- `05:16:40.367`: FunctionFS reported the definitive
  `functionfs-disable-disconnect` boundary from proven Idle ownership. The Pi
  service remained active with unchanged PID 1462.
- Through the complete 30-second reconnect poll the phone controller already
  reported `host`, but only root hubs `1d6b:0002` and `1d6b:0003` existed. The
  Pi UDC was `not attached` and `1d50:614d` was absent.
- The runbook recovery cycled the OnePlus controller `device -> host`.
  `1d50:614d` then immediately enumerated at `1-1.3`, 480 Mbit/s.
- GUD reprobed, `/dev/dri/card1` returned, the unchanged Pi process entered
  USB epoch 2, and managed MirGUD restarted as PID 88186.
- RGB565+LZ4 frames completed in epoch 2 with zero poisoned, timed-out, or
  failed transactions. The operator confirmed the external desktop was
  visible again.

## Safety conclusions

- Detach containment: PASS.
- Definitive old endpoint-generation boundary: PASS.
- Fresh epoch after recovery: PASS (`usb_epoch=1 -> 2`).
- Same Pi service process across the cycle: PASS (PID 1462).
- Receiver/AIO safety: PASS (Idle boundary, empty queue, no poison/timeouts).
- Phone and display restoration after documented recovery: PASS.
- Autonomous physical reconnect without USB-role forcing: **FAIL**.

The failure is below GUD: the OnePlus host controller exposed only its root
hubs until its role was cycled. No Pi service restart, xdispd restart, phone
reboot, or Pi reboot was used.
