// SPDX-License-Identifier: GPL-2.0
/*
 * Phase-0 de-risk: a throwaway HID-BPF program that applies a hardcoded dead
 * zone to the throttle (report byte 6-7). Its only job is to prove that an
 * HID-BPF report rewrite reaches evdev on this hardware, i.e. that it dodges the
 * raw_event propagation bug the in-kernel process_pedals path hit.
 */
#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define HID_INPUT_REPORT_SIZE 30
#define THROTTLE_OFF 6
#define DEADZONE 40000

extern __u8 *hid_bpf_get_data(struct hid_bpf_ctx *ctx, unsigned int offset,
			      const size_t sz) __ksym;

SEC("struct_ops/hid_device_event")
int BPF_PROG(poc_event, struct hid_bpf_ctx *hctx, enum hid_report_type type,
	     __u64 source)
{
	__u8 *data = hid_bpf_get_data(hctx, 0 /* offset */, HID_INPUT_REPORT_SIZE);
	__u16 thr;

	if (!data)
		return 0;
	if (hctx->size < HID_INPUT_REPORT_SIZE)
		return 0;

	thr = data[THROTTLE_OFF] | (data[THROTTLE_OFF + 1] << 8);
	if (thr < DEADZONE)
		thr = 0;
	data[THROTTLE_OFF] = thr & 0xff;
	data[THROTTLE_OFF + 1] = thr >> 8;
	return 0;
}

SEC(".struct_ops.link")
struct hid_bpf_ops poc = {
	.hid_id = 0,	/* the loader sets this to the interface-0 hid device id */
	.hid_device_event = (void *)poc_event,
};

char _license[] SEC("license") = "GPL";
