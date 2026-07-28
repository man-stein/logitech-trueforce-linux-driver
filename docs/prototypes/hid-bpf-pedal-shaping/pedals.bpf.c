// SPDX-License-Identifier: GPL-2.0
/*
 * Piecewise-linear HID-BPF pedal shaper for the RS50/G PRO direct-drive wheels.
 * Rewrites the throttle axis (report bytes 6-7) through a piecewise-linear curve
 * whose PS_NPOINTS y-values live in a pinned map (shaping_curve). Brake/clutch
 * and the combined-pedals combine are added in Phase 2. FFB is untouched.
 *
 * A LUT keyed by the throttle value miscompiles to identity in a HID-BPF
 * struct_ops program (verified on cachyos and mainline). This shaper avoids any
 * value-derived index: the segment x-boundaries are compile-time constants, so
 * segment selection is a constant comparison chain and the y-values are read at
 * constant indices. Only comparisons + constant reads + arithmetic - all proven
 * to work in this context.
 */
#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "pedal_shaping.h"

#define REPORT_SIZE 30
#define THROTTLE_OFF 6
#define PS_XMAX 65535
#define PS_SEGS (PS_NPOINTS - 1)

extern __u8 *hid_bpf_get_data(struct hid_bpf_ctx *ctx, unsigned int offset,
			      const size_t sz) __ksym;

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct pedal_curve);
	__uint(pinning, LIBBPF_PIN_BY_NAME);
} shaping_curve SEC(".maps");

/* Piecewise-linear: find the segment [x0,x1] (constant bounds) containing `in`
 * and interpolate between c->y[i] and c->y[i+1]. Assumes a monotonic curve. */
static __u16 apply_curve(const struct pedal_curve *c, __u16 in)
{
	__u16 out = c->y[PS_SEGS];	/* in >= last x -> clamp to last y */
	int i;

#pragma unroll
	for (i = 0; i < PS_SEGS; i++) {
		__u32 x0 = (__u32)i * PS_XMAX / PS_SEGS;
		__u32 x1 = (__u32)(i + 1) * PS_XMAX / PS_SEGS;
		__u16 y0 = c->y[i];
		__u16 y1 = c->y[i + 1];

		if (in >= x0 && in < x1)
			out = y0 + (__u16)(((__u32)(y1 - y0) * (in - x0)) / (x1 - x0));
	}
	return out;
}

SEC("struct_ops/hid_device_event")
int BPF_PROG(pedals_event, struct hid_bpf_ctx *hctx, enum hid_report_type type,
	     __u64 source)
{
	__u32 k = 0;
	struct pedal_curve *c = bpf_map_lookup_elem(&shaping_curve, &k);
	__u8 *data = hid_bpf_get_data(hctx, 0 /* offset */, REPORT_SIZE);
	__u16 thr;

	if (!c || !data || hctx->size < REPORT_SIZE || !c->enabled)
		return 0;

	thr = data[THROTTLE_OFF] | (data[THROTTLE_OFF + 1] << 8);
	thr = apply_curve(c, thr);
	data[THROTTLE_OFF] = thr & 0xff;
	data[THROTTLE_OFF + 1] = thr >> 8;
	return 0;
}

SEC(".struct_ops.link")
struct hid_bpf_ops pedals = {
	.hid_id = 0,	/* the loader sets this to the interface-0 hid device id */
	.hid_device_event = (void *)pedals_event,
};

char _license[] SEC("license") = "GPL";
