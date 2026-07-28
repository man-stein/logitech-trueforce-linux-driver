# HID-BPF Pedal Shaping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply throttle/brake/clutch response curves, deadzones, and combined-pedals mode by rewriting the RS50/G PRO HID input report with an HID-BPF program driven by logi-dd, replacing the broken in-kernel `process_pedals` path.

**Architecture:** An HID-BPF `struct_ops/hid_device_event` program does per-report LUT lookups + an optional combined-pedals combine on report bytes 6/8/10 (throttle/brake/clutch, LE16). It reads per-pedal 1024-entry `u16` lookup tables from a pinned `ARRAY` map. logi-dd folds deadzone+curve into those LUTs (pure Rust) and live-writes the map via `libbpf-rs`. The driver is trimmed to expose raw pedals; profile-rename is kept.

**Tech Stack:** C (BPF CO-RE, clang), libbpf/bpftool, Rust (`libbpf-rs`), the out-of-tree `hid-logitech-dd` kernel module, existing `userspace/logi-dd` Rust workspace.

## Global Constraints

- No PII in any committed artifact (genericize serials / profile names).
- No AI/assistant mentions in commits, code, or docs; no AI commit trailers.
- Licensing: BPF program and driver GPL-2.0; logi-dd GPL-2.0 (matches workspace).
- Do not regress FFB/TrueForce or any other driver function.
- Concise comments; match surrounding style.
- Environment (shu): `CONFIG_HID_BPF=y`, `/sys/kernel/btf/vmlinux` present, `clang`+`bpftool` installed; RS50 live, joystick on `/dev/input/event3`.
- Kernel module reloads on shu need root via the `%7` tmux pane (root@shu). Build the module with `make` in `mainline/` (auto-detects clang).
- **Wheel handling:** reloading the module churns the wheel; if it wedges (USB present, no HID enum), it needs a physical power-cycle. Reload sparingly; reset any test curve before finishing.

---

## PHASE 0 — De-risk gate (throwaway)

**Sole purpose:** prove that an HID-BPF report rewrite reaches evdev (i.e. dodges the `raw_event` propagation bug). If Phase 0 fails, STOP — the approach falls back to userspace uinput and this plan is void. Nothing in later phases is built until Phase 0 passes.

### Task 1: Scaffold the BPF build and a hardcoded dead-zone program

**Files:**
- Create: `bpf/Makefile`
- Create: `bpf/pedals_poc.bpf.c`
- Create: `bpf/.gitignore` (`vmlinux.h`, `*.bpf.o`, `*.skel.h`)

**Interfaces:**
- Produces: `bpf/pedals_poc.bpf.o` — a compiled CO-RE HID-BPF object with one
  `struct_ops` link `poc` whose `hid_device_event` zeroes throttle (report byte 6-7)
  while raw throttle < 40000 and passes it through otherwise (a dead zone).

- [ ] **Step 1: Generate vmlinux.h from the running kernel's BTF**

```bash
mkdir -p bpf
bpftool btf dump file /sys/kernel/btf/vmlinux format c > bpf/vmlinux.h
head -3 bpf/vmlinux.h   # expect: /* SPDX... */  #ifndef __VMLINUX_H__
```

- [ ] **Step 2: Write the PoC BPF program**

Verify the HID-BPF API against `/usr/src/linux-cachyos/Documentation/hid/hid-bpf.rst`
(or the running kernel's) and `samples/hid/` if present; the struct_ops shape below
matches kernel 6.3+ HID-BPF.

`bpf/pedals_poc.bpf.c`:
```c
// SPDX-License-Identifier: GPL-2.0
#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define HID_INPUT_REPORT_SIZE 30
#define THROTTLE_OFF 6
#define DEADZONE 40000

extern __u8 *hid_bpf_get_data(struct hid_bpf_ctx *ctx, unsigned int offset,
			      const size_t sz) __ksym;

SEC("struct_ops/hid_device_event")
int BPF_PROG(poc_event, struct hid_bpf_ctx *hctx)
{
	__u8 *data = hid_bpf_get_data(hctx, 0, HID_INPUT_REPORT_SIZE);

	if (!data)
		return 0;
	if (hctx->size < HID_INPUT_REPORT_SIZE)
		return 0;

	__u16 thr = data[THROTTLE_OFF] | (data[THROTTLE_OFF + 1] << 8);
	if (thr < DEADZONE)
		thr = 0;
	data[THROTTLE_OFF] = thr & 0xff;
	data[THROTTLE_OFF + 1] = thr >> 8;
	return 0;
}

SEC(".struct_ops.link")
struct hid_bpf_ops poc = {
	.hid_device_event = (void *)poc_event,
};

char _license[] SEC("license") = "GPL";
```

- [ ] **Step 3: Write the Makefile**

`bpf/Makefile`:
```make
CLANG ?= clang
BPFTOOL ?= bpftool
ARCH := $(shell uname -m | sed 's/x86_64/x86/')

all: pedals_poc.bpf.o

vmlinux.h:
	$(BPFTOOL) btf dump file /sys/kernel/btf/vmlinux format c > $@

%.bpf.o: %.bpf.c vmlinux.h
	$(CLANG) -g -O2 -target bpf -D__TARGET_ARCH_$(ARCH) \
		-I. -c $< -o $@
	$(BPFTOOL) gen object $@ $@

clean:
	rm -f *.bpf.o
```

- [ ] **Step 4: Build and verify the object loads its struct_ops shape**

Run:
```bash
cd bpf && make pedals_poc.bpf.o && bpftool btf dump file pedals_poc.bpf.o | grep -i hid_bpf_ops
```
Expected: builds without error; the dump references `hid_bpf_ops`.

- [ ] **Step 5: Commit**

```bash
git add bpf/Makefile bpf/pedals_poc.bpf.c bpf/.gitignore
git commit -m "bpf: PoC hardcoded dead-zone HID-BPF program + build"
```

### Task 2: Load the PoC on the wheel and run the dead-zone acceptance test (GATE)

**Files:**
- Create: `bpf/load_poc.sh`

**Interfaces:**
- Consumes: `bpf/pedals_poc.bpf.o` from Task 1.
- Produces: a pass/fail decision. PASS = ABS_RX stays ~0 while the throttle is
  below ~60% travel, then rises. FAIL = ABS_RX is a linear ramp from the start.

- [ ] **Step 1: Write a minimal loader script**

`bpf/load_poc.sh` (uses bpftool to load + attach the struct_ops to the wheel's hid id):
```bash
#!/bin/bash
# Attach pedals_poc.bpf.o to the RS50 interface-0 hid device.
set -e
OBJ=${1:-pedals_poc.bpf.o}
# Find the hid device id (the sysfs name's trailing hex) for interface 0 (joystick).
HIDDEV=$(for p in /sys/bus/hid/devices/*C276*; do
  [ -e "$p" ] && find "$p" -name 'event*' >/dev/null 2>&1 && echo "$p"; done | head -1)
echo "wheel hid dev: $HIDDEV"
# HID-BPF struct_ops needs hid_id set before attach; use bpftool's hid subcommand
# if available, else a tiny libbpf loader. Verify the exact incantation against
# `bpftool help` / `udev-hid-bpf` on this kernel.
bpftool struct_ops register "$OBJ" /sys/fs/bpf/pedals_poc
echo "loaded"
```
Note: the exact attach mechanism (setting `hid_id`) is kernel-version-sensitive.
If `bpftool struct_ops register` cannot set the target hid id, write a ~30-line
libbpf C loader (`bpf/load_poc.c`) that opens the skeleton, sets
`skel->struct_ops.poc->hid_id = <id>`, and calls `bpf_map__attach_struct_ops`.
Confirm against `Documentation/hid/hid-bpf.rst` before choosing.

- [ ] **Step 2: Load it on the wheel (root pane)**

In the root@shu pane (`%7`):
```bash
cd /home/mescon/Projects/logitech-trueforce-linux-driver/bpf
bash load_poc.sh
```
Expected: "loaded"; `bpftool struct_ops list` shows it; no dmesg errors.

- [ ] **Step 3: Run the dead-zone acceptance test**

Record ABS_RX while pressing the throttle slowly to the floor:
```bash
evtest /dev/input/event3 > /tmp/poc.txt 2>&1 &   # then press throttle slowly, kill after
grep 'ABS_RX' /tmp/poc.txt | grep -oE 'value [0-9]+' | awk '{print $2}' \
 | awk '{n++; if($1==0)z++} END{printf "zeros: %d/%d (%d%%)\n",z,n,n?100*z/n:0}'
```
Expected on PASS: a large fraction of samples are 0 (the dead region), then rising.
On FAIL: 0% zeros, linear from the start.

- [ ] **Step 4: Record the gate outcome**

- PASS -> proceed to the driver cleanup and Phase 1.
- FAIL -> STOP. Detach (`bpftool struct_ops unregister ...`), reset the wheel,
  and escalate: the HID-BPF path does not reach evdev either; revisit userspace
  uinput. Do not build Phase 1.

- [ ] **Step 5: Detach the PoC and commit the loader**

```bash
# root pane: bpftool struct_ops unregister name poc   (or rm the pinned link)
git add bpf/load_poc.sh
git commit -m "bpf: PoC loader + dead-zone gate test"
```

---

## DRIVER CLEANUP (bundled into the dd-profile-rename merge)

Independent of the BPF work; makes the driver lean and unblocks the merge. Do this
on the `dd-profile-rename` branch.

### Task 3: Revert the in-kernel curve rework, keep the rename

**Files:**
- Modify: `mainline/hid-logitech-hidpp.c` (revert the uncommitted curve-rework hunks; keep the `wheel_profile_names` fn4 write path).

**Interfaces:**
- Produces: a working tree where the only change vs the branch base is the
  profile-rename (`wheel_profile_names` becomes 0664 with a fn4 store) — the
  curve-rework (CUSTOM LUT, resample-helper extraction, repointed pedal curve) is gone.

- [ ] **Step 1: Inspect the current diff to identify rework vs rename**

```bash
cd /home/mescon/Projects/logitech-trueforce-linux-driver
git diff --stat mainline/hid-logitech-hidpp.c
git diff mainline/hid-logitech-hidpp.c | grep -nE '^\+' | grep -iE 'CURVE_CUSTOM|throttle_lut|resample_curve|apply_curve|profile_names' | head
```
Expected: shows both the rename (`wheel_profile_names_store`, 0664) and the curve
rework (CUSTOM/LUT/resample) additions.

- [ ] **Step 2: Discard all changes, then re-apply ONLY the rename**

```bash
git checkout -- mainline/hid-logitech-hidpp.c
```
Then re-apply the profile-rename edit: make `wheel_profile_names` writable and add
`wheel_profile_names_store` sending `0x8137` fn4 `[slot][len][ascii]`. The exact
store body is the validated one recorded in memory `ghub-captures-decoded-2026-07-14`
(slot 1-5, 1-14 char name, `hidpp_send_fap_command_sync(..., ff->idx_profile, 0x40, params, 2+namelen, ...)`),
and the attr line becomes:
```c
static DEVICE_ATTR(wheel_profile_names, 0664, wheel_profile_names_show,
		   wheel_profile_names_store);
```

- [ ] **Step 3: Build**

```bash
cd mainline && make 2>&1 | grep -icE 'error|warning'   # expect 0
```

- [ ] **Step 4: Commit**

```bash
git add mainline/hid-logitech-hidpp.c
git commit -m "hid-logitech-dd: profile rename via 0x8137 fn4 (writable wheel_profile_names)"
```

### Task 4: Remove `process_pedals` and its sysfs attrs

**Files:**
- Modify: `mainline/hid-logitech-hidpp.c` (delete `hidpp_dd_process_pedals`, `hidpp_dd_apply_curve`, `hidpp_dd_apply_deadzone`, its call site in `hidpp_raw_event`, the sysfs attrs and their entries in the attribute group, and unused fields/helpers).

**Interfaces:**
- Produces: a driver that exposes raw pedals; no pedal-shaping sysfs attrs.

- [ ] **Step 1: Delete the pedal-shaping sysfs attrs and their group entries**

Remove the `DEVICE_ATTR`/`show`/`store` for: `wheel_throttle_curve`,
`wheel_brake_curve`, `wheel_clutch_curve`, `wheel_pedal_response_curve`, the six
`wheel_*_deadzone_{lower,upper}` attrs, `wheel_combined_pedals`, and the compat
`combine_pedals`; and their `&dev_attr_*.attr` lines in the attribute group array.

- [ ] **Step 2: Delete the transform code and its call site**

Delete `hidpp_dd_process_pedals`, `hidpp_dd_apply_curve`, `hidpp_dd_apply_deadzone`,
the `struct hidpp_dd_ff_data` fields they used (`throttle_curve`/`brake_curve`/
`clutch_curve`, the `*_lut` if present, `combined_pedals`, the six deadzone fields),
their defaults in `hidpp_dd_ff_init`, and the call block in `hidpp_raw_event` that
invokes `hidpp_dd_process_pedals` for interface-0 30-byte reports.

- [ ] **Step 3: Remove now-unused curve-upload helpers if orphaned**

If `hidpp_dd_response_curve_upload`/`_revert` and `idx_pedal_curve` are only used by
removed code, delete them and the `idx_pedal_curve` discovery. Keep
`wheel_response_curve` (steering) and its helper if it still uses them — check with:
```bash
grep -nE 'response_curve_upload|idx_pedal_curve|idx_response_curve' mainline/hid-logitech-hidpp.c
```

- [ ] **Step 4: Build**

```bash
cd mainline && make 2>&1 | grep -icE 'error|warning'   # expect 0
```

- [ ] **Step 5: Load and verify raw pedals + rename still work (root pane)**

Reload once (gentle), then: pressing the throttle moves ABS_RX linearly (raw), and
`echo "5:TEST2" > .../wheel_profile_names; cat .../wheel_profile_names` renames slot 5.
The removed attrs are gone (`ls .../wheel_*curve* .../wheel_*deadzone* 2>&1` -> not found).

- [ ] **Step 6: Commit**

```bash
git add mainline/hid-logitech-hidpp.c
git commit -m "hid-logitech-dd: drop non-functional in-kernel pedal shaping (moves to HID-BPF)"
```

---

## PHASE 1 — Throttle-curve MVP (only after Phase 0 PASS)

### Task 5: logi-dd-core `shaping` module — curve+deadzone -> LUT

**Files:**
- Create: `userspace/logi-dd/crates/logi-dd-core/src/shaping.rs`
- Modify: `userspace/logi-dd/crates/logi-dd-core/src/lib.rs` (add `pub mod shaping;`)

**Interfaces:**
- Produces:
  - `pub const LUT_LEN: usize = 1024;`
  - `pub struct Curve { pub points: Vec<(u16,u16)> }` — user "in:out" pairs, 0:0 first, 65535:65535 last, strictly increasing in, non-decreasing out.
  - `pub struct Deadzone { pub lower_pct: u8, pub upper_pct: u8 }`
  - `pub fn build_lut(curve: &Curve, dz: &Deadzone) -> [u16; LUT_LEN]` — for each bucket `i`, input = `i << 6`; apply deadzone (scale `[lower,1-upper]` to full range, clamp), then interpolate the curve; the LUT is consumed by the BPF as `out = lut[in>>6]` with low-6-bit interpolation, so `lut[i]` = output at input `i<<6`.

- [ ] **Step 1: Write failing tests**

`shaping.rs` test module:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn linear() -> Curve { Curve { points: vec![(0,0),(65535,65535)] } }
    fn nodz() -> Deadzone { Deadzone { lower_pct: 0, upper_pct: 0 } }

    #[test] fn linear_lut_is_identity_at_buckets() {
        let lut = build_lut(&linear(), &nodz());
        assert_eq!(lut[0], 0);
        assert_eq!(lut[LUT_LEN-1], 65535);
        // bucket 512 -> input 512<<6 = 32768 -> ~32768
        assert!((lut[512] as i32 - 32768).abs() < 128);
    }
    #[test] fn dead_until_50pct_is_zero_then_rises() {
        // curve linear, lower deadzone 50%
        let lut = build_lut(&linear(), &Deadzone{lower_pct:50,upper_pct:0});
        assert_eq!(lut[0], 0);
        assert_eq!(lut[256], 0);            // input 16384 (25%) still in dead zone
        assert!(lut[LUT_LEN-1] >= 65000);   // full travel -> ~full
        assert!(lut[600] > lut[520]);       // rises past the dead zone
    }
    #[test] fn curve_bends_output() {
        // aggressive: 0:0 32768:4096 65535:65535
        let c = Curve{points: vec![(0,0),(32768,4096),(65535,65535)]};
        let lut = build_lut(&c, &nodz());
        assert!(lut[512] < 8000);           // mid input -> low output (bent)
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd userspace/logi-dd && cargo test -p logi-dd-core shaping 2>&1 | tail -5
```
Expected: fails to compile (`build_lut` not found).

- [ ] **Step 3: Implement `shaping.rs`**

```rust
//! Pedal response-curve + deadzone -> lookup table for the HID-BPF shaper.
//! The BPF reads `out = lut[in>>6]` with low-6-bit interpolation, so entry i
//! holds the output for input `i << 6`.

pub const LUT_LEN: usize = 1024;

pub struct Curve { pub points: Vec<(u16, u16)> }
pub struct Deadzone { pub lower_pct: u8, pub upper_pct: u8 }

fn apply_deadzone(input: u16, dz: &Deadzone) -> u16 {
    let lo = (dz.lower_pct as u32 * 65535) / 100;
    let hi = 65535 - (dz.upper_pct as u32 * 65535) / 100;
    let x = input as u32;
    if x <= lo { return 0; }
    if x >= hi { return 65535; }
    let range = hi - lo;
    (((x - lo) * 65535) / range).min(65535) as u16
}

fn apply_curve(input: u16, c: &Curve) -> u16 {
    let x = input as u32;
    let p = &c.points;
    for w in p.windows(2) {
        let (in0, out0) = (w[0].0 as u32, w[0].1 as u32);
        let (in1, out1) = (w[1].0 as u32, w[1].1 as u32);
        if x <= in1 {
            if in1 == in0 { return out1 as u16; }
            return (out0 + (out1 - out0) * (x - in0) / (in1 - in0)) as u16;
        }
    }
    p.last().map(|q| q.1).unwrap_or(0)
}

pub fn build_lut(curve: &Curve, dz: &Deadzone) -> [u16; LUT_LEN] {
    let mut lut = [0u16; LUT_LEN];
    for i in 0..LUT_LEN {
        let input = ((i as u32) << 6).min(65535) as u16;
        lut[i] = apply_curve(apply_deadzone(input, dz), curve);
    }
    lut
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd userspace/logi-dd && cargo test -p logi-dd-core shaping 2>&1 | tail -5
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add userspace/logi-dd/crates/logi-dd-core/src/shaping.rs userspace/logi-dd/crates/logi-dd-core/src/lib.rs
git commit -m "logi-dd-core: shaping module (curve+deadzone -> 1024-entry LUT)"
```

### Task 6: Shared map struct + real map-driven BPF (throttle)

**Files:**
- Create: `bpf/pedal_shaping.h` (the `struct pedal_shaping` shared contract + constants)
- Create: `bpf/pedals.bpf.c` (the production throttle shaper)
- Modify: `bpf/Makefile` (build `pedals.bpf.o`)

**Interfaces:**
- Consumes: `pedal_shaping` layout must match `shaping::LUT_LEN` (1024) and the
  `in>>6`/low-6 interpolation from Task 5.
- Produces: `bpf/pedals.bpf.o` with a pinned `ARRAY` map `shaping_map` (1 entry of
  `struct pedal_shaping`) and a `hid_device_event` that applies `throttle_lut`.

- [ ] **Step 1: Write the shared header**

`bpf/pedal_shaping.h`:
```c
/* SPDX-License-Identifier: GPL-2.0 */
#ifndef PEDAL_SHAPING_H
#define PEDAL_SHAPING_H
#define PS_LUT_LEN 1024
struct pedal_shaping {
	__u8  enabled;
	__u8  combined;
	__u16 neutral;
	__u16 throttle_lut[PS_LUT_LEN];
	__u16 brake_lut[PS_LUT_LEN];
	__u16 clutch_lut[PS_LUT_LEN];
};
#endif
```

- [ ] **Step 2: Write the BPF shaper (throttle only for Phase 1)**

`bpf/pedals.bpf.c`:
```c
// SPDX-License-Identifier: GPL-2.0
#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "pedal_shaping.h"

#define REPORT_SIZE 30
#define THROTTLE_OFF 6

extern __u8 *hid_bpf_get_data(struct hid_bpf_ctx *ctx, unsigned int offset,
			      const size_t sz) __ksym;

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct pedal_shaping);
	__uint(pinning, LIBBPF_PIN_BY_NAME);
} shaping_map SEC(".maps");

static __u16 apply_lut(const __u16 *lut, __u16 in)
{
	__u32 idx = in >> 6;             /* 0..1023 */
	if (idx >= PS_LUT_LEN - 1)
		return lut[PS_LUT_LEN - 1];
	__u32 frac = in & 0x3f;          /* 0..63 */
	__u16 a = lut[idx], b = lut[idx + 1];
	return a + (__u16)(((__u32)(b - a) * frac) >> 6);
}

SEC("struct_ops/hid_device_event")
int BPF_PROG(pedals_event, struct hid_bpf_ctx *hctx)
{
	__u32 k = 0;
	struct pedal_shaping *s = bpf_map_lookup_elem(&shaping_map, &k);
	__u8 *data = hid_bpf_get_data(hctx, 0, REPORT_SIZE);

	if (!s || !data || hctx->size < REPORT_SIZE || !s->enabled)
		return 0;
	__u16 thr = data[THROTTLE_OFF] | (data[THROTTLE_OFF + 1] << 8);
	thr = apply_lut(s->throttle_lut, thr);
	data[THROTTLE_OFF] = thr & 0xff;
	data[THROTTLE_OFF + 1] = thr >> 8;
	return 0;
}

SEC(".struct_ops.link")
struct hid_bpf_ops pedals = { .hid_device_event = (void *)pedals_event };
char _license[] SEC("license") = "GPL";
```

- [ ] **Step 3: Add to the Makefile and build**

Add `pedals.bpf.o` to `all:` in `bpf/Makefile`, then:
```bash
cd bpf && make pedals.bpf.o && echo built
```
Expected: builds; `bpftool map show` after load will list the `shaping_map` ARRAY.

- [ ] **Step 4: Commit**

```bash
git add bpf/pedal_shaping.h bpf/pedals.bpf.c bpf/Makefile
git commit -m "bpf: map-driven throttle shaper (1024-LUT + interpolation)"
```

### Task 7: logi-dd map writer via libbpf-rs

**Files:**
- Modify: `userspace/logi-dd/crates/logi-dd-core/Cargo.toml` (add `libbpf-rs` under an optional `bpf` feature)
- Create: `userspace/logi-dd/crates/logi-dd-core/src/shaper_map.rs`
- Modify: `userspace/logi-dd/crates/logi-dd-core/src/lib.rs` (`#[cfg(feature="bpf")] pub mod shaper_map;`)

**Interfaces:**
- Consumes: `build_lut` from Task 5; the pinned map path
  `/sys/fs/bpf/hid-logitech-dd/shaping_map` (set by the loader/pinning).
- Produces: `pub fn write_shaping(map_path: &Path, enabled: bool, combined: bool, neutral: u16, throttle: &[u16;1024], brake: &[u16;1024], clutch: &[u16;1024]) -> io::Result<()>` — opens the pinned `ARRAY` map and updates key 0 with the packed `pedal_shaping` bytes (little-endian, layout matching `bpf/pedal_shaping.h`).

- [ ] **Step 1: Add the optional dependency**

In `logi-dd-core/Cargo.toml`:
```toml
[features]
bpf = ["dep:libbpf-rs"]

[dependencies]
libbpf-rs = { version = "0.24", optional = true }
```

- [ ] **Step 2: Write a failing test (byte packing, no kernel needed)**

`shaper_map.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::pack_shaping;
    #[test] fn packs_header_then_luts_le() {
        let z = [0u16; 1024];
        let mut t = z; t[0] = 0x1234;
        let bytes = pack_shaping(true, false, 0x8000, &t, &z, &z);
        assert_eq!(bytes.len(), 4 + 3*1024*2);
        assert_eq!(bytes[0], 1);            // enabled
        assert_eq!(bytes[1], 0);            // combined
        assert_eq!(&bytes[2..4], &0x8000u16.to_le_bytes()); // neutral
        assert_eq!(&bytes[4..6], &0x1234u16.to_le_bytes()); // throttle_lut[0]
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd userspace/logi-dd && cargo test -p logi-dd-core shaper_map 2>&1 | tail -5
```
Expected: fails (`pack_shaping` not found).

- [ ] **Step 4: Implement `shaper_map.rs`**

```rust
//! Pack + write the pedal_shaping struct into the pinned BPF ARRAY map.
use std::io;
use std::path::Path;

/// Layout MUST match bpf/pedal_shaping.h: enabled u8, combined u8, neutral u16,
/// then throttle/brake/clutch [u16;1024], all little-endian, packed.
pub fn pack_shaping(enabled: bool, combined: bool, neutral: u16,
                    throttle: &[u16;1024], brake: &[u16;1024], clutch: &[u16;1024]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + 3*1024*2);
    v.push(enabled as u8);
    v.push(combined as u8);
    v.extend_from_slice(&neutral.to_le_bytes());
    for lut in [throttle, brake, clutch] {
        for &x in lut.iter() { v.extend_from_slice(&x.to_le_bytes()); }
    }
    v
}

#[cfg(feature = "bpf")]
pub fn write_shaping(map_path: &Path, enabled: bool, combined: bool, neutral: u16,
                     throttle: &[u16;1024], brake: &[u16;1024], clutch: &[u16;1024]) -> io::Result<()> {
    use libbpf_rs::MapHandle;
    let bytes = pack_shaping(enabled, combined, neutral, throttle, brake, clutch);
    let map = MapHandle::from_pinned_path(map_path)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let key = 0u32.to_le_bytes();
    map.update(&key, &bytes, libbpf_rs::MapFlags::ANY)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
}
```
(Verify `MapHandle::from_pinned_path` / `update` signatures against the pinned
`libbpf-rs` version; adjust the API names if the crate version differs.)

- [ ] **Step 5: Run the packing test (default features, no kernel)**

```bash
cd userspace/logi-dd && cargo test -p logi-dd-core shaper_map 2>&1 | tail -5
```
Expected: passes.

- [ ] **Step 6: Build with the bpf feature to check it compiles**

```bash
cd userspace/logi-dd && cargo build -p logi-dd-core --features bpf 2>&1 | tail -5
```
Expected: compiles (or documents the exact `libbpf-rs` API tweak needed).

- [ ] **Step 7: Commit**

```bash
git add userspace/logi-dd/crates/logi-dd-core/Cargo.toml userspace/logi-dd/crates/logi-dd-core/src/shaper_map.rs userspace/logi-dd/crates/logi-dd-core/src/lib.rs
git commit -m "logi-dd-core: pinned-map writer for the HID-BPF shaper (bpf feature)"
```

### Task 8: End-to-end throttle-curve validation on hardware

**Files:**
- Create: `bpf/load_pedals.sh` (load `pedals.bpf.o`, pin the map at `/sys/fs/bpf/hid-logitech-dd/shaping_map`)
- Create: `userspace/logi-dd/crates/logi-dd-tui/src/bin/apply_curve.rs` (a tiny CLI: build a LUT from hardcoded aggressive points and call `write_shaping`) — throwaway harness for validation, deleted or folded into the TUI in Phase 3.

**Interfaces:**
- Consumes: `pedals.bpf.o` (Task 6), `build_lut` (Task 5), `write_shaping` (Task 7).

- [ ] **Step 1: Write the loader that pins the map**

`bpf/load_pedals.sh`: attach `pedals.bpf.o` to the wheel (as in Task 2), ensuring the
`shaping_map` is pinned at `/sys/fs/bpf/hid-logitech-dd/shaping_map` (via
`LIBBPF_PIN_BY_NAME` + a `/sys/fs/bpf/hid-logitech-dd` pin dir, or `bpftool map pin`).
Set the pin's group to `input` and mode `0660`:
```bash
chgrp input /sys/fs/bpf/hid-logitech-dd/shaping_map && chmod 0660 /sys/fs/bpf/hid-logitech-dd/shaping_map
```

- [ ] **Step 2: Write the apply-curve harness**

```rust
// apply_curve.rs
use logi_dd_core::shaping::{build_lut, Curve, Deadzone};
use logi_dd_core::shaper_map::write_shaping;
use std::path::Path;
fn main() {
    let dead = build_lut(&Curve{points: vec![(0,0),(40000,0),(65535,65535)]},
                         &Deadzone{lower_pct:0,upper_pct:0});
    let z = [0u16;1024];
    write_shaping(Path::new("/sys/fs/bpf/hid-logitech-dd/shaping_map"),
                  true, false, 0x8000, &dead, &z, &z).unwrap();
    println!("applied dead-zone curve");
}
```

- [ ] **Step 3: Load + apply + hardware test (root pane / input group)**

```bash
# root pane: bash bpf/load_pedals.sh
cargo run -p logi-dd-tui --features bpf --bin apply_curve   # writes the map
# press throttle slowly; capture ABS_RX:
evtest /dev/input/event3 > /tmp/e2e.txt 2>&1 &   # kill after the press
grep ABS_RX /tmp/e2e.txt | grep -oE 'value [0-9]+' | awk '{print $2}' \
 | awk '{n++; if($1==0)z++} END{printf "zeros: %d/%d\n",z,n}'
```
Expected: dead-zone shape — ABS_RX ~0 for the first ~60% of travel, then rises
(matching the Phase 0 gate but now map-driven and live-writable).

- [ ] **Step 4: Verify live edit**

Re-run `apply_curve` with a linear curve (`(0,0),(65535,65535)`) and confirm ABS_RX
becomes a straight ramp on the next press — no reload needed.

- [ ] **Step 5: Reset + commit**

Reset to linear (or `enabled=false`), detach, and commit the loader + harness:
```bash
git add bpf/load_pedals.sh userspace/logi-dd/crates/logi-dd-tui/src/bin/apply_curve.rs
git commit -m "bpf: pinned-map loader + throttle-curve e2e validation harness"
```

---

## PHASE 2 & 3 — deferred to a follow-up plan

Detailed only after Phase 1 validates on hardware (the exact BPF/map interfaces are
proven by then). Outline:

- **Phase 2:** extend `pedals.bpf.c` to apply `brake_lut` (offset 8) and `clutch_lut`
  (offset 10) and the combined-pedals combine (`throttle = neutral + (thr-brake)/2`,
  `brake = 0` when `s->combined`); extend logi-dd to build brake/clutch LUTs (deadzone
  folded in) and set `combined`. Hardware test each.
- **Phase 3:** `udev-hid-bpf` packaging (ship the CO-RE object + modalias match, add
  the dependency to AUR/COPR/OBS/Debian), config persistence + a `logi-dd apply`
  oneshot re-writing the map on wheel appearance (udev-triggered), and a TUI curve
  editor calling `build_lut` + `write_shaping`. Then merge `dd-profile-rename`
  (rename + `process_pedals` removal + `bpf/` + logi-dd shaping) and push.
