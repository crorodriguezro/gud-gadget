# XDISP E2 bandwidth and damage follow-up

This bounded follow-up packages the qualified FunctionFS large-AIO
vmalloc/sequential-chunk fix, corrects derived half-frame accounting, compares
one-admission full-frame LZ4 with the qualified full-frame RAW control, and
audits native Mir damage exposure. E1 admission semantics were unchanged and
E2-T04 was not started.

The LZ4 run used a temporary Pi descriptor of `compression=lz4,
max_buffer_size=3686400`, a phone host cap of 3,686,400 bytes, XRGB8888
1280x720, and one logical 1280x720 rectangle per frame. The Pi and phone were
returned to the production-safe RAW/12,800-byte configuration after testing.

The detailed result tables are under `lz4/`; the native-damage audit is under
`damage/`; cleanup and reproducibility records are under `cleanup/`; and the
restored hardware state is under `runtime/`.
