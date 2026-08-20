# Stage 1 - direct Pi-local DRM/KMS HDMI pattern

Pi boot ID:

    0721cfbe-d426-4d0a-84a6-681315473e2c

Before the test, `gud-userspace.service` was `disabled` and `inactive`; there
was no `gud-drm` process and `/sys/kernel/debug/dri/0/clients` contained no DRM
master. The directly selected VC4 resources were:

| Resource | Value |
| --- | --- |
| Connector | `33` (`HDMI-A-1`, connected) |
| CRTC | `95` |
| Primary plane | `84` |
| Test mode | `1280x720@60` |
| Framebuffer format | `XR24` |

`kmstest` normally waits for Enter before exiting. To keep each framebuffer
visibly scanned out while observing it remotely, stdin was held open by a
test-only `tail -f /dev/null` pipeline. Each local test process held VC4 DRM
master and was explicitly terminated before the next test. No GUD process was
running during this stage.

## Red

```sh
setsid sh -c \
  'tail -f /dev/null | kmstest -c @33 -r @95:1280x720@60 \
     -p @84:1280x720 -f 1280x720-XR24 -T red'
```

`kmstest` PID `1038` held DRM master:

```text
             command  tgid dev master a   uid      magic
             kmstest  1038   0   y    y  1000          0
```

Operator observation: **the physical HDMI monitor was visibly solid red.**

## Green

After explicitly terminating the red test process and confirming no `kmstest`
remained:

```sh
setsid sh -c \
  'tail -f /dev/null | kmstest -c @33 -r @95:1280x720@60 \
     -p @84:1280x720 -f 1280x720-XR24 -T green'
```

`kmstest` PID `1075` held DRM master. Operator observation: **the physical
HDMI monitor visibly changed from solid red to solid green.**

## Blue

After explicitly terminating the green test process and confirming no
`kmstest` remained:

```sh
setsid sh -c \
  'tail -f /dev/null | kmstest -c @33 -r @95:1280x720@60 \
     -p @84:1280x720 -f 1280x720-XR24 -T blue'
```

`kmstest` PID `1098` held DRM master. Operator observation: **the physical
HDMI monitor visibly changed from solid green to solid blue.**

After explicitly terminating the blue test process, no `kmstest` process or
VC4 DRM master remained. The normal Pi console resumed on VC4 connector 33,
CRTC 95, plane 84, framebuffer 668 at 1920x1080.
