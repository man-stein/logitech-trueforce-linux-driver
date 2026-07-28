# HID-BPF Pedal Shaping — Design

**Status:** approved (brainstorming), ready for implementation planning
**Date:** 2026-07-15
**Wheels:** Logitech RS50 (c276) and G PRO (c272/c268) direct-drive

## Problem and motivation

Sim-racers want per-pedal **response curves**, **deadzones**, and a **combined-pedals**
mode (throttle − brake on one axis) — exactly what G HUB offers on Windows.

Two dead ends were ruled out by hardware evidence before this design:

1. **Hardware curves are not usable on PC.** G HUB uploads curves to the wheel's
   `0x80A4` feature, but a raw-HID capture (`dev/tools/windows_curve_hwtest.bat`,
   2026-07-15) proved the wheel does **not** apply them to its live PC HID output:
   with an aggressive dead-zone curve set and Saved in G HUB, the raw throttle
   output was an identical straight ramp to the no-curve baseline. G HUB shapes the
   axis **in software** on the PC; the `0x80A4` store does not affect the HID axis
   games read. "Do what G HUB does" therefore means **software shaping**.
2. **In-kernel `raw_event` rewriting is broken.** The driver's `hidpp_dd_process_pedals`
   modifies the report buffer in `raw_event`, but the modification does not reliably
   reach evdev. This was proven load-independent and reproduces on both CachyOS
   (clang/ThinLTO 7.1.3) and stock Fedora (GCC 7.1.3): a constant written to
   `data[6]` reaches evdev, but the computed curve output does not. The write-back
   store is emitted (objdump-verified) and mainline hid-core guarantees the buffer
   is shared — yet the value is lost. It defeated every fix (write ordering,
   double-write, barriers) and is not worth further investment.

**HID-BPF** is the modern, upstream-sanctioned mechanism for rewriting HID reports
at the driver layer. It runs in the kernel like G HUB's driver filter, leaves force
feedback completely untouched, and hooks at the well-tested BPF report-fixup point —
which very likely dodges the `raw_event` propagation bug (HID-BPF report edits are
used in production to fix broken devices). This design does software pedal shaping
via HID-BPF, driven by the logi-dd userspace app.

## Goals / non-goals

**Goals**
- Apply throttle/brake/clutch response curves, per-pedal deadzones, and
  combined-pedals mode by rewriting the wheel's HID input report.
- Live editing: a curve change from logi-dd takes effect on the next HID report.
- Persist across the wheel being unplugged/replugged and across reboots, with no
  always-running daemon required.
- Leave FFB/TrueForce and every other driver function unchanged.

**Non-goals**
- No hardware `0x80A4` curve upload (proven inert for PC output).
- No in-kernel `process_pedals` rewriting (removed — see Driver changes).
- Button remapping / per-game auto-switch / telemetry (separate logi-dd phases).

## Environment (verified on shu)

`CONFIG_HID_BPF=y`, `CONFIG_BPF_SYSCALL=y`, `CONFIG_DEBUG_INFO_BTF=y`,
`/sys/kernel/btf/vmlinux` present, `clang` + `bpftool` installed. `udev-hid-bpf`
is **not** yet installed (added as a packaged dependency in Phase 3).

## Architecture

Three units with well-defined interfaces:

1. **BPF program** (`hid-logitech-dd-pedals.bpf.c`, CO-RE) — dumb and fast. One
   `SEC("struct_ops/hid_device_event")` hook, matched to the RS50/G PRO interface-0
   joystick by modalias. Per report it does only: bounds-check, three LUT lookups
   with interpolation, an optional combined-pedals combine, and writes bytes 6/8/10.
   No curve or deadzone logic lives here. ~40 lines, verifier-trivial (no loops,
   fixed indices).

2. **logi-dd** (Rust, existing `userspace/logi-dd` workspace) — all expressive math.
   A new `shaping` module in `logi-dd-core` folds deadzone + curve into a per-pedal
   lookup table and writes the parameter struct into the pinned BPF map. The map
   index/interpolation math mirrors the BPF exactly. The TUI gains a curve editor
   (Phase 3).

3. **udev-hid-bpf** — loads, attaches, and pins the program + map when the wheel
   appears; persistence with no daemon.

```
   wheel HID input report (iface 0)
        -> HID-BPF program: lut lookups + optional combine -> rewrites bytes 6/8/10 -> evdev
        BPF program reads params from a pinned ARRAY map
        logi-dd computes LUTs and live-writes that pinned map
        udev-hid-bpf loads+attaches+pins the program+map on plug
```

## Data model — the pinned map

One `BPF_MAP_TYPE_ARRAY`, one entry, holding:

```c
struct pedal_shaping {
    __u8  enabled;              /* 0 = passthrough (no rewrite) */
    __u8  combined;             /* 1 = throttle-minus-brake onto throttle axis */
    __u16 neutral;              /* combined neutral point, default 0x8000 */
    __u16 throttle_lut[1024];
    __u16 brake_lut[1024];
    __u16 clutch_lut[1024];
};                             /* ~6 KB */
```

- LUT semantics (shared contract between logi-dd and BPF): input is a 16-bit pedal
  value; `idx = in >> 6` (1024 buckets); output =
  `lut[idx] + ((lut[idx+1] - lut[idx]) * (in & 0x3f)) >> 6`. `lut[1023]` is the
  clamp for the top bucket. logi-dd computes `lut[i]` as the final output for input
  `i << 6` after applying deadzone then curve.
- **Deadzone folds into the LUT** (logi-dd applies it before the curve when building
  the table), so the BPF needs no deadzone code.
- **Combined-pedals** is the one cross-axis op the BPF does itself: after the
  throttle and brake lookups, `throttle = clamp((throttle - brake + 65536)/2)` and
  `brake = 0`, writing the combined value onto the throttle axis.
- The map is pinned (e.g. `/sys/fs/bpf/hid-logitech-dd/pedal_shaping`), owned
  `root:input`, group-writable, so logi-dd can update it live without root — the
  same permission model as the driver's existing `wheel_*` sysfs attrs.

## logi-dd integration

- `logi-dd-core::shaping`: pure functions — `curve_points + deadzone -> [u16; 1024]`
  (index/interp math matching the BPF constants), plus a writer that opens the pinned
  map and updates the `pedal_shaping` struct via `libbpf-rs`.
- Config persistence: logi-dd saves the user's per-pedal curve/deadzone/combined
  choice to a config file. On wheel appearance a udev-triggered `logi-dd apply`
  (oneshot) re-writes the map so a replug/reboot restores the user's settings.
  (Phase 3.)
- The sysfs settings registry loses the removed pedal-shaping attrs; those settings
  now route through the shaping module + map, not sysfs.

## Loading and persistence

`udev-hid-bpf` loads/attaches/pins the program and map on plug. The BPF ships as a
compiled CO-RE object with the modalias match metadata; packaging (Phase 3) installs
it to the `udev-hid-bpf` firmware dir and adds `udev-hid-bpf` as a dependency across
the AUR/COPR/OBS/Debian channels. Until Phase 3, load manually for validation.

## Phasing

- **Phase 0 — de-risk (throwaway).** A minimal HID-BPF program with a *hardcoded*
  dead-zone on the throttle, loaded manually. Sole goal: confirm an HID-BPF report
  rewrite reaches evdev (dodges the propagation bug). Acceptance: the dead-zone test
  (ABS_RX stays 0 for the dead region, then rises) passes on the wheel. If it fails,
  stop and reconsider userspace uinput before building anything else.
- **Phase 1 — throttle-curve MVP.** Real map-driven BPF (throttle only) + the
  `shaping` LUT module + live map write + manual load. Acceptance: a live custom
  throttle curve visibly shapes ABS_RX on the wheel.
- **Phase 2 — full pedals.** Brake + clutch LUTs, per-pedal deadzones (folded into
  the LUT), combined-pedals (BPF combine).
- **Phase 3 — productionize.** `udev-hid-bpf` packaging (auto-load), config
  persistence + re-apply on plug, TUI curve editor.

Each phase is separately testable on hardware. The final driver cleanup + merge with
profile-rename + push happens once the pipeline works (Phase 1 minimum; ideally after
Phase 2).

## Driver changes (bundled into the merge)

Delete `hidpp_dd_process_pedals` and its sysfs attrs — `wheel_throttle_curve`,
`wheel_brake_curve`, `wheel_clutch_curve`, `wheel_pedal_response_curve`, the six
`wheel_*_deadzone_*` attrs, `wheel_combined_pedals`, and the Oversteer-compat
`combine_pedals` — plus the now-unused curve-upload helpers and `idx_pedal_curve`
discovery. The driver returns to exposing raw pedals; FFB/TrueForce/LEDs/mode and the
validated **profile-rename** (`0x8137` fn4, currently on `dd-profile-rename`) are
kept. The in-progress in-kernel "curve rework" on `dd-profile-rename` is reverted;
only the rename and the `process_pedals` removal remain on the driver side.

Removing these attrs is not a regression: the transforms never functioned (the
propagation bug), so there is no working behavior to lose.

## Testing

- **logi-dd `shaping` (unit):** curve points + deadzone produce the expected 1024-entry
  LUT; the index/interpolation math equals the BPF constants; combined-pedals neutral
  math is correct.
- **BPF (host):** a small harness feeds synthetic reports through the same lookup
  logic to confirm byte-offset handling and interpolation; the real gate is hardware.
- **Hardware (acceptance):** the refined dead-zone test (ABS_RX flat through the dead
  region, then rising) at Phase 0 and Phase 1; a hyper curve and a preset for feel.

## Global constraints

- No PII in any committed artifact (genericize serials / profile names).
- No AI/assistant mentions in commits, code, or docs; drop AI commit trailers.
- Licensing: driver GPL-2.0; BPF program GPL-2.0 (required for BPF helpers); logi-dd
  GPL-2.0 (matches the workspace).
- Concise comments; match surrounding style.
- Do not regress FFB/TrueForce or any existing driver function.

## Open risks

- Phase 0 is the make-or-break: if HID-BPF report rewrites also fail to reach evdev
  on this hardware, the whole approach falls back to userspace `uinput` (with FFB
  forwarding). Phase 0 exists precisely to find this out cheaply and first.
- `libbpf-rs` adds logi-dd's first non-trivial dependency; acceptable for BPF map
  access, and gated to the shaping feature.
