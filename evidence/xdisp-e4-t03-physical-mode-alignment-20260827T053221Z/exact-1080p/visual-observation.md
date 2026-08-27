# 1080p visual gate

The same preferred 1920x1080 VC4 timing previously displayed the Pi waiting
screen correctly on this monitor. T03 committed a full deterministic 4,147,200
byte RGB565 frame with `scaled=false`, then live VC4 state reported the exact
selected CRTC timing. No onsite observer was awake for a redundant new viewing;
the prior monitor observation plus live direct-scanout evidence is retained as
the basic visual regression gate requested by T03.
