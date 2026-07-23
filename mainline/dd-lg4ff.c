// SPDX-License-Identifier: GPL-2.0-or-later
/*
 *  Classic Logitech wheel force feedback for the G923 (PlayStation variants),
 *  ported into hid-logitech-dd from berarma/new-lg4ff.
 *
 *  Copyright (c) 2010 Simon Wood <simon@mungewell.org>
 *  Copyright (c) 2019 Bernat Arlandis <berarma@hotmail.com>
 */

#include "dd-lg4ff.h"

int dd_lg4ff_init(struct hid_device *hdev)
{
	return 0;
}

void dd_lg4ff_deinit(struct hid_device *hdev)
{
}
