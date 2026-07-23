/* SPDX-License-Identifier: GPL-2.0-or-later */
#ifndef DD_LG4FF_H
#define DD_LG4FF_H
struct hid_device;
int dd_lg4ff_init(struct hid_device *hdev);
void dd_lg4ff_deinit(struct hid_device *hdev);
#endif
