// SPDX-License-Identifier: GPL-2.0
/*
 * Load pedals.bpf.o onto a given hid device id, pin the shaping_cfg + throttle_lut
 * maps under /sys/fs/bpf/hid-logitech-dd/, hand them to the `input` group (0660)
 * so logi-dd can update them without root, and hold the attachment until Ctrl-C.
 *
 * Phase-1 validation loader; Phase 3 replaces it with udev-hid-bpf.
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <signal.h>
#include <errno.h>
#include <string.h>
#include <grp.h>
#include <sys/stat.h>
#include <bpf/libbpf.h>
#include "pedals.skel.h"

#define PIN_DIR "/sys/fs/bpf/hid-logitech-dd"

static const char *const MAP_PINS[] = {
	PIN_DIR "/shaping_cfg",
	PIN_DIR "/throttle_lut",
};

static volatile int stop_flag;
static void on_sig(int s) { (void)s; stop_flag = 1; }

int main(int argc, char **argv)
{
	if (argc < 2) { fprintf(stderr, "usage: %s <hid_id>\n", argv[0]); return 1; }
	int hid_id = atoi(argv[1]);

	if (mkdir(PIN_DIR, 0755) && errno != EEXIST) {
		fprintf(stderr, "mkdir %s: %s\n", PIN_DIR, strerror(errno));
		return 1;
	}

	LIBBPF_OPTS(bpf_object_open_opts, opts, .pin_root_path = PIN_DIR);
	struct pedals_bpf *skel = pedals_bpf__open_opts(&opts);
	if (!skel) { fprintf(stderr, "open failed\n"); return 1; }

	skel->struct_ops.pedals->hid_id = hid_id;
	if (pedals_bpf__load(skel)) { fprintf(stderr, "load failed\n"); goto err; }
	/* shaping_cfg + throttle_lut are now pinned via LIBBPF_PIN_BY_NAME. */

	struct group *g = getgrnam("input");
	if (g) {
		for (unsigned i = 0; i < sizeof(MAP_PINS) / sizeof(MAP_PINS[0]); i++)
			if (chown(MAP_PINS[i], 0, g->gr_gid) || chmod(MAP_PINS[i], 0660))
				fprintf(stderr, "warn: could not set %s perms: %s\n",
					MAP_PINS[i], strerror(errno));
	}

	struct bpf_link *link = bpf_map__attach_struct_ops(skel->maps.pedals);
	if (!link) { fprintf(stderr, "attach failed\n"); goto err; }

	printf("attached to hid_id %d; maps pinned under %s\n", hid_id, PIN_DIR);
	signal(SIGINT, on_sig);
	while (!stop_flag)
		sleep(1);

	bpf_link__destroy(link);
	pedals_bpf__destroy(skel);
	printf("detached\n");
	return 0;
err:
	pedals_bpf__destroy(skel);
	return 1;
}
