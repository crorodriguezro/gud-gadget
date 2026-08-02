# FunctionFS Full-Speed Endpoint Descriptor Diagnosis

The FunctionFS configuration failure is proven to originate in userspace
descriptor generation. The vendored `usb-gadget` custom-function builder
serialized its full-speed endpoint descriptor from an `EndpointDesc` initialized
with `max_packet_size: 0`. It only replaced that field for high-speed and
super-speed sections.

The successful exact-read run negotiated high-speed. The current failing
connection is full-speed (12 Mb/s) and the OnePlus host observed the full-speed
bulk OUT endpoint descriptor with `wMaxPacketSize=0`. FunctionFS therefore
selects that descriptor at `SET_CONFIGURATION`, and `usb_ep_enable()` rejects
it. This is not evidence that a DWC2 hardware endpoint capability is zero.

The narrow correction adds a full-speed packet-size field, defaults bulk
endpoints to the USB full-speed bulk maximum (64), and serializes it into the
full-speed descriptor section. A byte-level unit test verifies the FunctionFS
header section layout and full/high/super-speed endpoint fields.

No Pi service action, deployment, configuration retry, or framebuffer payload
occurred in this diagnostic session. The Pi was unreachable over SSH, so its
receiver state remains unknown and the corrected binary must not be deployed
until the required unused-Idle safety gate is captured.
