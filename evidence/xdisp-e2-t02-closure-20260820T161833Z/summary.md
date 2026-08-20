E2-T02 result:
PASS

Production artifact qualification:
wrapper-free: yes
xdispd source HEAD: 6e4d8c5
xdispd built SHA256: 981a489c101fc979da1d4f5ff5cc66294db5d24fc705cac281975e6fda3efaf4
xdispd deployed SHA256: 981a489c101fc979da1d4f5ff5cc66294db5d24fc705cac281975e6fda3efaf4
mirgud built SHA256: 359689ea4b59519a3a7031bf6044e299696ab9b8e26484a4b017c7f61690dc1a
mirgud deployed SHA256: 359689ea4b59519a3a7031bf6044e299696ab9b8e26484a4b017c7f61690dc1a

Managed spawn:
xrgb8888 confirmed: yes

Presentation:
initial modeset complete: yes
frames_received: 456
frames_submitted: 455
frames_presented: 279
frames_dropped: 174
frames_cancelled: 1
submit_failures: 1

Bounds:
max_pending_observed: n/a
max_in_flight_observed: n/a

Final:
pending: none
in_flight: 0
accounting_ok: true

Lifecycle:
first-present ordering correct: yes
worker joined before KMS teardown: yes
no leftover mirgud: yes

Continuity:
Lomiri stable: yes
compositor stable: yes
LightDM stable: yes
phone boot stable: yes

Forensics:
pstore clean: yes
relevant kernel faults: no
