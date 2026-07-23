// SPDX-License-Identifier: GPL-2.0-or-later
/*
 *  Classic Logitech wheel force feedback for the G923 (PlayStation variants),
 *  ported into hid-logitech-dd from berarma/new-lg4ff.
 *
 *  Copyright (c) 2010 Simon Wood <simon@mungewell.org>
 *  Copyright (c) 2019 Bernat Arlandis <berarma@hotmail.com>
 */

#include <linux/bits.h>
#include <linux/fixp-arith.h>
#include <linux/hid.h>
#include <linux/hrtimer.h>
#include <linux/input.h>
#include <linux/math.h>
#include <linux/spinlock.h>
#include <linux/timer.h>
#ifdef CONFIG_LEDS_CLASS
#include <linux/leds.h>
#endif

#include "dd-lg4ff.h"
#include "hid-ids.h"

/*
 * Scaling/translation helpers, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:74-95). These convert between the kernel's FF core value
 * ranges and the wire formats the classic Logitech wheel commands expect.
 */
#define DD_LG4FF_CLAMP_VALUE_U16(x) ((unsigned short)((x) > 0xffff ? 0xffff : (x)))
#define DD_LG4FF_CLAMP_VALUE_S16(x) ((unsigned short)((x) <= -0x8000 ? -0x8000 : ((x) > 0x7fff ? 0x7fff : (x))))
#define DD_LG4FF_SCALE_VALUE_U16(x, bits) (DD_LG4FF_CLAMP_VALUE_U16(x) >> (16 - bits))
#define DD_LG4FF_SCALE_COEFF(x, bits) DD_LG4FF_SCALE_VALUE_U16(abs(x) * 2, bits)
#define DD_LG4FF_TRANSLATE_FORCE(x) ((DD_LG4FF_CLAMP_VALUE_S16(x) + 0x8000) >> 8)
#define DD_LG4FF_STOP_EFFECT(state) ((state)->flags = 0)
#define DD_LG4FF_JIFFIES2MS(jiffies) ((jiffies) * 1000 / HZ)
#undef fixp_sin16
#define fixp_sin16(v) (((v % 360) > 180) ? -(fixp_sin32((v % 360) - 180) >> 16) : fixp_sin32(v) >> 16)

#define DD_LG4FF_MAX_EFFECTS 16

#define DD_LG4FF_FF_EFFECT_STARTED 0
#define DD_LG4FF_FF_EFFECT_ALLSET 1
#define DD_LG4FF_FF_EFFECT_PLAYING 2
#define DD_LG4FF_FF_EFFECT_UPDATING 3

/*
 * new-lg4ff numbers its multimode-wheel mode bits 0..8, one per supported
 * wheel family (native, DF-EX, DFP, G25, DFGT, G27, G29, G923 PS, G923).
 * We only carry the G923 family, so the bit indices are renumbered to a
 * self-contained 0..1 range instead of keeping the upstream 7/8 slots.
 */
#define DD_LG4FF_MODE_G923_PS_IDX 0
#define DD_LG4FF_MODE_G923_IDX 1
#define DD_LG4FF_MODE_MAX_IDX 2

#define DD_LG4FF_MODE_G923_PS BIT(DD_LG4FF_MODE_G923_PS_IDX)
#define DD_LG4FF_MODE_G923 BIT(DD_LG4FF_MODE_G923_IDX)

#define DD_LG4FF_G923_TAG "G923"
#define DD_LG4FF_G923_NAME "G923 Racing Wheel"
#define DD_LG4FF_G923_PS_TAG "G923"
#define DD_LG4FF_G923_PS_NAME "G923 Racing Wheel (Playstation mode)"

struct dd_lg4ff_effect_state {
	struct ff_effect effect;
	struct ff_envelope *envelope;
	unsigned long start_at;
	unsigned long play_at;
	unsigned long stop_at;
	unsigned long flags;
	unsigned long time_playing;
	unsigned long updated_at;
	unsigned int phase;
	unsigned int phase_adj;
	unsigned int count;
	unsigned int cmd;
	unsigned int cmd_start_time;
	unsigned int cmd_start_count;
	int direction_gain;
	int slope;
	unsigned int slot;
};

struct dd_lg4ff_effect_parameters {
	int level;
	int d1;
	int d2;
	int k1;
	int k2;
	unsigned int clip;
};

struct dd_lg4ff_slot {
	int id;
	struct dd_lg4ff_effect_parameters parameters;
	u8 current_cmd[7];
	int cmd_op;
	int is_updated;
	int effect_type;
};

struct dd_lg4ff_wheel_data {
	const u32 product_id;
	u16 combine;
	u16 range;
	u16 autocenter;
	u16 master_gain;
	u16 gain;
	const u16 min_range;
	const u16 max_range;
#ifdef CONFIG_LEDS_CLASS
	u8  led_state;
	struct led_classdev *led[5];
#endif
	const u32 alternate_modes;
	const char * const real_tag;
	const char * const real_name;
	const u16 real_product_id;
	const u16 capabilities;

	void (*set_range)(struct hid_device *hid, u16 range);
};

struct dd_lg4ff_device_entry {
	spinlock_t report_lock; /* Protect output HID report */
	spinlock_t timer_lock;
	struct hid_report *report;
	struct dd_lg4ff_wheel_data wdata;
	struct hid_device *hid;
	struct timer_list timer;
	struct hrtimer hrtimer;
	struct dd_lg4ff_slot slots[4];
	struct dd_lg4ff_effect_state states[DD_LG4FF_MAX_EFFECTS];
	unsigned peak_ffb_level;
	int effects_used;
#ifdef CONFIG_LEDS_CLASS
	int has_leds;
#endif
};

static const signed short dd_lg4ff_wheel_effects[] __maybe_unused = {
	FF_CONSTANT,
	FF_SPRING,
	FF_DAMPER,
	FF_AUTOCENTER,
	FF_PERIODIC,
	FF_SINE,
	FF_SQUARE,
	FF_TRIANGLE,
	FF_SAW_UP,
	FF_SAW_DOWN,
	FF_RAMP,
	FF_FRICTION,
	FF_INERTIA,
	-1
};

struct dd_lg4ff_wheel {
	const u32 product_id;
	const signed short *ff_effects;
	const u16 min_range;
	const u16 max_range;
	const u16 capabilities;
	void (*set_range)(struct hid_device *hid, u16 range);
};

struct dd_lg4ff_compat_mode_switch {
	const u8 cmd_count;	/* Number of commands to send */
	const u8 cmd[];
};

struct dd_lg4ff_wheel_ident_info {
	const u32 modes;
	const u16 mask;
	const u16 result;
	const u16 real_product_id;
};

struct dd_lg4ff_multimode_wheel {
	const u16 product_id;
	const u32 alternate_modes;
	const char *real_tag;
	const char *real_name;
};

struct dd_lg4ff_alternate_mode {
	const u16 product_id;
	const char *tag;
	const char *name;
};

/*
 * Device table, trimmed to the G923 (c266) row. set_range is left NULL
 * until the command/engine port task adds dd_lg4ff_set_range_g25(); it
 * is not called before then.
 */
static const struct dd_lg4ff_wheel dd_lg4ff_devices[] __maybe_unused = {
	{USB_DEVICE_ID_LOGITECH_G923_WHEEL,
		dd_lg4ff_wheel_effects, 40, 900, 0, NULL},
};

/* Multimode wheel table, trimmed to the G923 PS (c267) and G923 (c266) rows. */
static const struct dd_lg4ff_multimode_wheel dd_lg4ff_multimode_wheels[] __maybe_unused = {
	{USB_DEVICE_ID_LOGITECH_G923_PS_WHEEL,
	 DD_LG4FF_MODE_G923_PS | DD_LG4FF_MODE_G923,
	 DD_LG4FF_G923_PS_TAG, DD_LG4FF_G923_PS_NAME},
	{USB_DEVICE_ID_LOGITECH_G923_WHEEL,
	 DD_LG4FF_MODE_G923,
	 DD_LG4FF_G923_TAG, DD_LG4FF_G923_NAME},
};

static const struct dd_lg4ff_alternate_mode dd_lg4ff_alternate_modes[DD_LG4FF_MODE_MAX_IDX] __maybe_unused = {
	[DD_LG4FF_MODE_G923_PS_IDX] = {USB_DEVICE_ID_LOGITECH_G923_PS_WHEEL,
					DD_LG4FF_G923_PS_TAG, DD_LG4FF_G923_PS_NAME},
	[DD_LG4FF_MODE_G923_IDX] = {USB_DEVICE_ID_LOGITECH_G923_WHEEL,
				     DD_LG4FF_G923_TAG, DD_LG4FF_G923_NAME},
};

/* Multimode wheel identificator for the G923 family. */
static const struct dd_lg4ff_wheel_ident_info dd_lg4ff_g923_ident_info __maybe_unused = {
	DD_LG4FF_MODE_G923_PS | DD_LG4FF_MODE_G923,
	0xff00,
	0x3800,
	USB_DEVICE_ID_LOGITECH_G923_WHEEL
};

/* Multimode wheel identification checklist, reduced to the G923 entry. */
static const struct dd_lg4ff_wheel_ident_info *dd_lg4ff_main_checklist[] __maybe_unused = {
	&dd_lg4ff_g923_ident_info,
};

/*
 * Single choke point for reaching the ported engine's per-device state.
 * new-lg4ff keeps this pointer in lg_drv_data->device_props; we have no
 * lg_drv_data, so it lives directly on struct hidpp_device (lg4ff_entry)
 * and is reached through the hidpp_dd_lg4ff_slot() accessor exported by
 * hid-logitech-hidpp.c, which is the only place that knows that struct's
 * layout. This replaces new-lg4ff's lg4ff_get_device_entry
 * (hid-lg4ff.c:457-478); every later ported function calls this instead
 * of touching drv_data->device_props directly.
 */
static struct dd_lg4ff_device_entry *dd_lg4ff_get_entry(struct hid_device *hdev) __maybe_unused;
static struct dd_lg4ff_device_entry *dd_lg4ff_get_entry(struct hid_device *hdev)
{
	void **slot;

	if (!hdev) {
		hid_err(hdev, "HID not found!\n");
		return NULL;
	}

	slot = (void **)hidpp_dd_lg4ff_slot(hdev);
	if (!slot) {
		hid_err(hdev, "Private driver data not found!\n");
		return NULL;
	}

	return (struct dd_lg4ff_device_entry *)*slot;
}

int dd_lg4ff_init(struct hid_device *hdev)
{
	return 0;
}

void dd_lg4ff_deinit(struct hid_device *hdev)
{
}
