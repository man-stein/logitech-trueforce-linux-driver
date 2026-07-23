/* SPDX-License-Identifier: GPL-2.0-or-later */
#ifndef DD_LG4FF_H
#define DD_LG4FF_H
struct hid_device;
int dd_lg4ff_init(struct hid_device *hdev);
void dd_lg4ff_deinit(struct hid_device *hdev);

/*
 * Returns the address of this hdev's struct hidpp_device::lg4ff_entry slot
 * (i.e. a struct dd_lg4ff_device_entry **, opaque here), or NULL if the
 * device has no hidpp_device drvdata yet. Defined in hid-logitech-hidpp.c,
 * the only file that knows struct hidpp_device's layout; this is the sole
 * point where dd-lg4ff.c reaches into it.
 */
void *hidpp_dd_lg4ff_slot(struct hid_device *hdev);
#endif
