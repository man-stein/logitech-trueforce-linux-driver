/* SPDX-License-Identifier: GPL-2.0 */
#ifndef PEDAL_SHAPING_H
#define PEDAL_SHAPING_H

/*
 * Shared contract between the HID-BPF shaper (bpf/pedals.bpf.c) and the logi-dd
 * map writer (shaper_map.rs).
 *
 * The shaper is PIECEWISE-LINEAR, not a LUT. A LUT needs a variable-index read
 * into a map array keyed by the throttle value; that read miscompiles to
 * identity inside a HID-BPF struct_ops program (confirmed on both cachyos and
 * mainline kernels). Comparisons on the report value, constant-index map reads,
 * and arithmetic all work, so the curve is stored as PS_NPOINTS y-values at
 * FIXED, evenly-spaced x positions (x[i] = i*65535/(PS_NPOINTS-1)). The BPF
 * selects the segment with a compile-time-constant comparison chain and
 * interpolates - no value-derived index anywhere. logi-dd resamples the user's
 * curve (deadzone folded in) to these PS_NPOINTS points and live-writes y[].
 */
#define PS_NPOINTS 32

struct pedal_curve {
	__u8  enabled;			/* 0 = passthrough (no rewrite) */
	__u8  combined;			/* 1 = throttle-minus-brake onto throttle axis */
	__u16 neutral;			/* combined neutral point, default 0x8000 */
	__u16 y[PS_NPOINTS];		/* output at x[i] = i*65535/(PS_NPOINTS-1) */
};

#endif /* PEDAL_SHAPING_H */
