# HID-BPF pedal shaping (prototype, not shipped)

Reference material. Nothing here is built, packaged, loaded or referenced by
the driver. It is kept for the two findings below, which were expensive to
establish and are not written down anywhere else.

## Read this before trusting DESIGN.md

`DESIGN.md` opens by ruling out hardware response curves:

> Hardware curves are not usable on PC. [...] the wheel does **not** apply them
> to its live PC HID output.

**That conclusion was wrong, and was overturned on 2026-07-28.** The curves had
been going to the pedal MCU, which accepts an upload and reports it back as
loaded but never applies it to the axis it sends to the PC. Written to the base
instead (`dev 0xff`, axis `pedal + 1`), they work. This was proven with a step
curve, which an applied curve makes impossible to sweep through: on the pedal
MCU the axis swept straight through the band, on the base it pinned to the
step's plateau exactly.

v0.21.0 ships that fix. `wheel_{throttle,brake,clutch}_{curve,sensitivity,deadzone}`
are hardware curves applied in the wheel's own firmware, so they need no host
process, survive replug and reboot, and cost nothing per report.

The whole reason this prototype existed is therefore gone. Do not revive it for
pedal shaping, and do not cite DESIGN.md as evidence that hardware curves are
inert.

## The two findings worth keeping

**1. HID-BPF report rewrites reach evdev; the driver's `raw_event` rewrites did
not.** The driver used to shape pedals by modifying the report buffer in
`raw_event`, and the modification did not reliably propagate. It reproduced on
both CachyOS (clang/ThinLTO) and stock Fedora (GCC): a constant written to
`data[6]` reached evdev, but a computed value did not, with the write-back store
present in the objdump. Rewriting the same report from a HID-BPF `struct_ops`
program did work (gate passed 2026-07-15). If a future feature needs to rewrite
HID reports, this is the mechanism that works.

**2. A variable-index read into a BPF map array miscompiles to identity inside a
HID-BPF `struct_ops` program.** Confirmed on both CachyOS and mainline kernels.
Comparisons on the report value, constant-index map reads, and arithmetic are
all fine; only the value-derived index is affected. `pedals.bpf.c` here is the
piecewise-linear version that works around it: segment boundaries are
compile-time constants, so segment selection is a constant comparison chain and
every map read uses a constant index. The earlier LUT version needed one map per
index to dodge the same bug.

## What this would still be useful for

Anything the wheel's firmware cannot do, where the report has to be rewritten on
the host: button remapping, axis remapping, per-game profile switching. Pedal
shaping is not on that list any more.

## Provenance

Salvaged from the local branch `dd-profile-rename` before it was deleted.
`pedal_shaping.h` and `pedals.bpf.c` are the newer piecewise-linear rewrite that
was still uncommitted work in progress on that branch; the loaders, `Makefile`
and the proof-of-concept files are from the branch tip. The rest of that branch
was a stale copy of the userspace app (shipped since as `userspace/logi-wheel`)
and a commit removing the in-kernel pedal shaping that v0.21.0 went on to fix.
Neither was worth keeping.
