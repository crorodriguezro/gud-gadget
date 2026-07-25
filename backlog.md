# Backlog

## Revisit FunctionFS AIO receives

The GUD bulk OUT endpoint now uses ordinary blocking FunctionFS reads. This is
intentional: on the Pi Zero 2 W, the `usb-gadget` Linux-AIO receive queue
accepted 16 requests and then returned the invalid completion value `0x7f600`
before any payload bytes arrived.

Revisit AIO only after the blocking-read path has passed repeated OnePlus frame
transfers without USB errors. Before restoring it, reproduce and explain the
invalid AIO completion, validate the behavior against the Pi's DWC2/FunctionFS
kernel, and compare sustained frame throughput against the blocking baseline.
