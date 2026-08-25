# Bounded one-shot recovery state machine

The recovery owner is the phone-side `mir-android2-platform-gud` package.  It
is armed only by removal of a previously observed `1d50:614d` USB device while
the Qualcomm controller reports `host`.

## States and gates

1. `DISARMED`: service start with no GUD never changes the USB role.
2. `ARMED`: a GUD add permits exactly one later removal transaction.
3. `WAITING_FOR_REATTACH`: a real GUD removal was consumed. The GUD kernel
   backport must report the affected xHCI root port disconnected and then
   connected for 15 consecutive 100 ms polls. Only its tagged uevent starts the
   userspace stability deadline.
4. `RESETTING_TO_DEVICE`: write `device` once.
5. `WAITING_FOR_XHCI_TEARDOWN`: wait up to a bounded timeout for the Qualcomm
   controller's xHCI platform child to disappear.
6. `DEVICE_SETTLE`: only after teardown is observed, hold a bounded settle
   interval so the asynchronous DWC3/PHY transition completes.
7. `RESETTING_TO_HOST`: write `host` once.
8. `WAITING_FOR_HOST_READY`: wait up to a bounded timeout for the xHCI child
   and both root hubs to return.
9. `CONSUMED`: log success or failure and never retry.  A new real GUD add is
   required to arm another recovery.

If teardown times out after the `device` write, the helper performs the single
planned `host` write as an ownership rollback, records failure, and does not
retry.  Failure of the `device` write performs no host write because ownership
never changed.  Failure of the host write is terminal and prominently logged.

## Physical reattach signal

The failed active run proved that charger `PRESENT` and ordinary USB topology
events are not reliable physical reattach signals on this hardware. The GUD
backport now retains the root hub and affected port when GUD disconnects. Its
bounded delayed-work monitor first requires `USB_PORT_STAT_CONNECTION=0`, then
15 stable connected samples at 100 ms intervals. It emits one root-hub
`KOBJ_CHANGE` with `GUD_HOST_REATTACH=1` and `GUD_ROOT_PORT=<port>`. The monitor
is cancelled by a normal GUD probe and expires after 10 minutes.

The userspace helper ignores charger-PRESENT and generic USB add/change events.
Only the exact tagged kernel event is accepted while a consumed GUD removal is
pending. The kernel monitor cannot write the role or retry; the userspace
helper remains the sole owner of the bounded role transaction.

There is no periodic role action, startup recovery, role action for an unknown
USB device, or automatic retry.
