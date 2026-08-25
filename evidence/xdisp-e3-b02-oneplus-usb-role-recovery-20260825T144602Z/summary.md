# E3-B02 summary

E3-B02 passes. A OnePlus 6 running the 4.9 downstream Qualcomm USB stack now
recovers a stale host controller after a proven GUD detach/physical root-port
reattach without SSH, reboot, service restart, or test-harness role forcing.

The GUD backport observes the affected xHCI root port, requires a disconnected
state followed by 15 stable 100 ms connected polls, and emits one tagged
uevent. A product-installed phone helper consumes only that event and owns one
bounded `device -> host` transaction. It has no startup action, generic USB or
charger trigger, retry loop, or automatic desktop activation behavior.

Hardware qualification passed 3/3 inactive reconnects and 1/1 active
detach/reconnect/explicit-reactivation. GUD returned as `1d50:614d` at 480
Mbit/s with `/dev/dri/card1` each time. The active desktop was visually
confirmed after explicit reactivation.

One later stale Virtual Trackpad session reopened the verdict temporarily.
Clean-boot A/B using unchanged userspace showed normal phone-UI return with
both the old module (~10 s) and new module (~5 s), excluding the root-port
monitor as a deterministic cause. The low variable presentation rate was also
present with both modules and is a separate performance issue.

No Mir, Lomiri, xdispd timeout, RGB565/LZ4 transport, Pi FunctionFS ownership,
or E4-T01 behavior was changed. E3-T02 was not run and is READY TO RERUN.
