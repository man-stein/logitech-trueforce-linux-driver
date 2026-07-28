// SPDX-License-Identifier: GPL-2.0
/* Phase-0 loader: attach pedals_poc to a given hid device id, hold until Ctrl-C. */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <signal.h>
#include <bpf/libbpf.h>
#include "pedals_poc.skel.h"

static volatile int stop_flag;
static void on_sig(int s) { (void)s; stop_flag = 1; }

int main(int argc, char **argv)
{
	if (argc < 2) { fprintf(stderr, "usage: %s <hid_id>\n", argv[0]); return 1; }
	int hid_id = atoi(argv[1]);

	struct pedals_poc_bpf *skel = pedals_poc_bpf__open();
	if (!skel) { fprintf(stderr, "open failed\n"); return 1; }
	skel->struct_ops.poc->hid_id = hid_id;
	if (pedals_poc_bpf__load(skel)) { fprintf(stderr, "load failed\n"); return 1; }

	struct bpf_link *link = bpf_map__attach_struct_ops(skel->maps.poc);
	if (!link) { fprintf(stderr, "attach failed\n"); return 1; }

	printf("attached to hid_id %d; press throttle, Ctrl-C to detach\n", hid_id);
	signal(SIGINT, on_sig);
	while (!stop_flag) sleep(1);

	bpf_link__destroy(link);
	pedals_poc_bpf__destroy(skel);
	printf("detached\n");
	return 0;
}
