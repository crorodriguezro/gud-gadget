# First physical failure timeline

Kernel monotonic timestamps establish the following sequence:

| Timestamp | Event |
| ---: | --- |
| 142581.146146 | upstream hub disconnect begins (`usb 1-1`) |
| 142581.146198 | GUD `1-1.3` disconnect |
| 142581.173026 | GUD driver reports disconnected |
| 142581.185875 | xHCI removal begins after helper's `device` write |
| 142581.453146 | old USB bus 1 deregistered |
| 142581.457346 | DWC3 enters low-power mode |
| 142581.465893 | DWC3 exits low-power mode |
| 142581.467210 | xHCI host controller recreated after helper's `host` write |
| 142583.818911 | root port cannot enable the attached topology |
| 142586.729048 | first bounded enumeration attempt fails |
| 142590.509079 | second bounded enumeration attempt fails |
| 142591.518728 | repeated `could not transition HS PHY to L2` begins |

The first xHCI removal-to-recreation interval was only 281.335 ms.  The
original helper wrote `device` and `host` back-to-back and therefore did not
observe a completed/settled device-role transition.  The kernel recreated the
host while the physical detach/charger event burst was still active.

Classification: `ROLE_TRANSITION_NOT_SETTLED` remains the leading userspace
hypothesis.  It must be tested with observable xHCI teardown and readiness
gates.  This evidence does not yet prove that timing is the only kernel fault.
