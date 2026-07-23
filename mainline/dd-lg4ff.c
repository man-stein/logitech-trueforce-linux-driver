// SPDX-License-Identifier: GPL-2.0-or-later
/*
 *  Classic Logitech wheel force feedback for the G923 (PlayStation variants),
 *  ported into hid-logitech-dd from berarma/new-lg4ff.
 *
 *  Copyright (c) 2010 Simon Wood <simon@mungewell.org>
 *  Copyright (c) 2019 Bernat Arlandis <berarma@hotmail.com>
 */

#include <linux/bitops.h>
#include <linux/bits.h>
#include <linux/fixp-arith.h>
#include <linux/hid.h>
#include <linux/hrtimer.h>
#include <linux/input.h>
#include <linux/jiffies.h>
#include <linux/math.h>
#include <linux/module.h>
#include <linux/spinlock.h>
#include <linux/timer.h>
#include <linux/usb.h>
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
#define DD_LG4FF_DEBUG(...) pr_debug("dd_lg4ff: " __VA_ARGS__)
#define DD_LG4FF_TIME_DIFF(a, b) ({ \
		typecheck(unsigned long, a); \
		typecheck(unsigned long, b); \
		((a) - (long)(b)); })

#define DD_LG4FF_MAX_EFFECTS 16
#define DD_LG4FF_DEFAULT_TIMER_PERIOD 2
#define DD_LG4FF_CAP_FRICTION 1

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

/* Forward declaration: defined below, needed by the device table row. */
static void dd_lg4ff_set_range_g25(struct hid_device *hid, u16 range);

/* Device table, trimmed to the G923 (c266) row. */
static const struct dd_lg4ff_wheel dd_lg4ff_devices[] __maybe_unused = {
	{USB_DEVICE_ID_LOGITECH_G923_WHEEL,
		dd_lg4ff_wheel_effects, 40, 900, 0, dd_lg4ff_set_range_g25},
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
 * Module parameters for the hrtimer effect engine, ported from new-lg4ff
 * (hid-lg4ff.c:423-455). The exposed parameter names are additionally
 * given a dd_lg4ff_ prefix (on top of the module's own hid-logitech-dd
 * namespace) so they cannot be confused with an in-tree lg4ff.ko's
 * timer_msecs/timer_mode/etc if both happen to be loaded at once.
 */
static int dd_lg4ff_timer_msecs = DD_LG4FF_DEFAULT_TIMER_PERIOD;
module_param_named(dd_lg4ff_timer_msecs, dd_lg4ff_timer_msecs, int, 0660);
MODULE_PARM_DESC(dd_lg4ff_timer_msecs, "Timer resolution in msecs.");

static int dd_lg4ff_fixed_loop;
module_param_named(dd_lg4ff_fixed_loop, dd_lg4ff_fixed_loop, int, 0);
MODULE_PARM_DESC(dd_lg4ff_fixed_loop, "Put the device into fixed loop mode.");

static int dd_lg4ff_timer_mode = 2;
module_param_named(dd_lg4ff_timer_mode, dd_lg4ff_timer_mode, int, 0660);
MODULE_PARM_DESC(dd_lg4ff_timer_mode, "Timer mode: 0) fixed, 1) static, 2) dynamic (default).");

static int dd_lg4ff_spring_level = 30;
module_param_named(dd_lg4ff_spring_level, dd_lg4ff_spring_level, int, 0);
MODULE_PARM_DESC(dd_lg4ff_spring_level, "Level of spring force (0-100).");

static int dd_lg4ff_damper_level = 30;
module_param_named(dd_lg4ff_damper_level, dd_lg4ff_damper_level, int, 0);
MODULE_PARM_DESC(dd_lg4ff_damper_level, "Level of damper force (0-100).");

static int dd_lg4ff_friction_level = 30;
module_param_named(dd_lg4ff_friction_level, dd_lg4ff_friction_level, int, 0);
MODULE_PARM_DESC(dd_lg4ff_friction_level, "Level of friction force (0-100).");

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

/*
 * 7-byte SET_REPORT command senders, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:480-514). dd_lg4ff_send_cmd_with_id() forces the report's
 * id first; it is used only by the mode-switch sequence, wired up once
 * that lands, so it stays __maybe_unused. dd_lg4ff_send_cmd() is now called
 * from the hrtimer effect engine below.
 */
static void __maybe_unused dd_lg4ff_send_cmd_with_id(struct dd_lg4ff_device_entry *entry, u8 *cmd, u8 id)
{
	unsigned long flags;
	s32 *value = entry->report->field[0]->value;

	spin_lock_irqsave(&entry->report_lock, flags);
	entry->report->id = id;
	value[0] = cmd[0];
	value[1] = cmd[1];
	value[2] = cmd[2];
	value[3] = cmd[3];
	value[4] = cmd[4];
	value[5] = cmd[5];
	value[6] = cmd[6];
	hid_hw_request(entry->hid, entry->report, HID_REQ_SET_REPORT);
	spin_unlock_irqrestore(&entry->report_lock, flags);
	DD_LG4FF_DEBUG("send_cmd: %02X %02X %02X %02X %02X %02X %02X %02X\n", id, cmd[0], cmd[1], cmd[2], cmd[3], cmd[4], cmd[5], cmd[6]);
}

static void dd_lg4ff_send_cmd(struct dd_lg4ff_device_entry *entry, u8 *cmd)
{
	unsigned long flags;
	s32 *value = entry->report->field[0]->value;

	spin_lock_irqsave(&entry->report_lock, flags);
	value[0] = cmd[0];
	value[1] = cmd[1];
	value[2] = cmd[2];
	value[3] = cmd[3];
	value[4] = cmd[4];
	value[5] = cmd[5];
	value[6] = cmd[6];
	hid_hw_request(entry->hid, entry->report, HID_REQ_SET_REPORT);
	spin_unlock_irqrestore(&entry->report_lock, flags);
	DD_LG4FF_DEBUG("send_cmd: %02X %02X %02X %02X %02X %02X %02X", cmd[0], cmd[1], cmd[2], cmd[3], cmd[4], cmd[5], cmd[6]);
}

/*
 * Wire-format packer, ported verbatim from new-lg4ff (hid-lg4ff.c:516-618).
 * This is the heart of the classic command protocol: it fills
 * slot->current_cmd[0..6] with the F8/3E slot-select byte plus the
 * per-effect-type payload (CONSTANT 0x00, SPRING 0x0b, DAMPER 0x0c,
 * FRICTION 0x0e; op3 stops the slot). Called from the hrtimer effect
 * engine below.
 */
static void dd_lg4ff_update_slot(struct dd_lg4ff_slot *slot, struct dd_lg4ff_effect_parameters *parameters)
{
	u8 original_cmd[7];
	int d1;
	int d2;
	int k1;
	int k2;
	int s1;
	int s2;

	memcpy(original_cmd, slot->current_cmd, sizeof(original_cmd));

	if ((original_cmd[0] & 0xf) == 1) {
		original_cmd[0] = (original_cmd[0] & 0xf0) + 0xc;
	}

	if (slot->effect_type == FF_CONSTANT) {
		if (slot->cmd_op == 0) {
			slot->cmd_op = 1;
		} else {
			slot->cmd_op = 0xc;
		}
	} else {
		if (parameters->clip == 0 || slot->effect_type == 0) {
			slot->cmd_op = 3;
		} else if (slot->cmd_op == 3) {
			slot->cmd_op = 1;
		} else {
			slot->cmd_op = 0xc;
		}
	}

	slot->current_cmd[0] = (0x10 << slot->id) + slot->cmd_op;

	if (slot->cmd_op == 3) {
		slot->current_cmd[1] = 0;
		slot->current_cmd[2] = 0;
		slot->current_cmd[3] = 0;
		slot->current_cmd[4] = 0;
		slot->current_cmd[5] = 0;
		slot->current_cmd[6] = 0;
	} else {
		switch (slot->effect_type) {
			case FF_CONSTANT:
				slot->current_cmd[1] = 0x00;
				slot->current_cmd[2] = 0;
				slot->current_cmd[3] = 0;
				slot->current_cmd[4] = 0;
				slot->current_cmd[5] = 0;
				slot->current_cmd[6] = 0;
				slot->current_cmd[2 + slot->id] = DD_LG4FF_TRANSLATE_FORCE(parameters->level);
				break;
			case FF_SPRING:
				d1 = DD_LG4FF_SCALE_VALUE_U16(((parameters->d1) + 0x8000) & 0xffff, 11);
				d2 = DD_LG4FF_SCALE_VALUE_U16(((parameters->d2) + 0x8000) & 0xffff, 11);
				s1 = parameters->k1 < 0;
				s2 = parameters->k2 < 0;
				k1 = abs(parameters->k1);
				k2 = abs(parameters->k2);
				if (k1 < 2048) {
					d1 = 0;
				} else {
					k1 -= 2048;
				}
				if (k2 < 2048) {
					d2 = 2047;
				} else {
					k2 -= 2048;
				}
				slot->current_cmd[1] = 0x0b;
				slot->current_cmd[2] = d1 >> 3;
				slot->current_cmd[3] = d2 >> 3;
				slot->current_cmd[4] = (DD_LG4FF_SCALE_COEFF(k2, 4) << 4) + DD_LG4FF_SCALE_COEFF(k1, 4);
				slot->current_cmd[5] = ((d2 & 7) << 5) + ((d1 & 7) << 1) + (s2 << 4) + s1;
				slot->current_cmd[6] = DD_LG4FF_SCALE_VALUE_U16(parameters->clip, 8);
				break;
			case FF_DAMPER:
				s1 = parameters->k1 < 0;
				s2 = parameters->k2 < 0;
				slot->current_cmd[1] = 0x0c;
				slot->current_cmd[2] = DD_LG4FF_SCALE_COEFF(parameters->k1, 4);
				slot->current_cmd[3] = s1;
				slot->current_cmd[4] = DD_LG4FF_SCALE_COEFF(parameters->k2, 4);
				slot->current_cmd[5] = s2;
				slot->current_cmd[6] = DD_LG4FF_SCALE_VALUE_U16(parameters->clip, 8);
				break;
			case FF_FRICTION:
				s1 = parameters->k1 < 0;
				s2 = parameters->k2 < 0;
				slot->current_cmd[1] = 0x0e;
				slot->current_cmd[2] = DD_LG4FF_SCALE_COEFF(parameters->k1, 8);
				slot->current_cmd[3] = DD_LG4FF_SCALE_COEFF(parameters->k2, 8);
				slot->current_cmd[4] = DD_LG4FF_SCALE_VALUE_U16(parameters->clip, 8);
				slot->current_cmd[5] = (s2 << 4) + s1;
				slot->current_cmd[6] = 0;
				break;
		}
	}

	if (memcmp(original_cmd, slot->current_cmd, sizeof(original_cmd))) {
		slot->is_updated = 1;
	}
}

/*
 * Per-effect-type force math, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:620-741). All __always_inline; called from the timer tick
 * (dd_lg4ff_timer, below) that drives these once per effect.
 */
static __always_inline int dd_lg4ff_calculate_constant(struct dd_lg4ff_effect_state *state)
{
	int level_sign;
	int level = state->effect.u.constant.level;
	int d, t;

	if (state->time_playing < state->envelope->attack_length) {
		level_sign = level < 0 ? -1 : 1;
		d = level - level_sign * state->envelope->attack_level;
		level = level_sign * state->envelope->attack_level + d * state->time_playing / state->envelope->attack_length;
	} else if (state->effect.replay.length) {
		t = state->time_playing - state->effect.replay.length + state->envelope->fade_length;
		if (t > 0) {
			level_sign = level < 0 ? -1 : 1;
			d = level - level_sign * state->envelope->fade_level;
			level = level - d * t / state->envelope->fade_length;
		}
	}

	return state->direction_gain * level / 0x7fff;
}

static __always_inline int dd_lg4ff_calculate_ramp(struct dd_lg4ff_effect_state *state)
{
	struct ff_ramp_effect *ramp = &state->effect.u.ramp;
	int level_sign;
	int level = INT_MAX;
	int d, t;

	if (state->time_playing < state->envelope->attack_length) {
		level = ramp->start_level;
		level_sign =  level < 0 ? -1 : 1;
		t = state->envelope->attack_length - state->time_playing;
		d = level - level_sign * state->envelope->attack_level;
		level = level_sign * state->envelope->attack_level + d * t / state->envelope->attack_length;
	} else if (state->effect.replay.length && state->time_playing >= state->effect.replay.length - state->envelope->fade_length) {
		level = ramp->end_level;
		level_sign = level < 0 ? -1 : 1;
		t = state->time_playing - state->effect.replay.length + state->envelope->fade_length;
		d = level_sign * state->envelope->fade_level - level;
		level = level - d * t / state->envelope->fade_length;
	} else {
		t = state->time_playing - state->envelope->attack_length;
		level = ramp->start_level + ((t * state->slope) >> 16);
	}

	return state->direction_gain * level / 0x7fff;
}

static __always_inline int dd_lg4ff_calculate_periodic(struct dd_lg4ff_effect_state *state)
{
	struct ff_periodic_effect *periodic = &state->effect.u.periodic;
	int magnitude = periodic->magnitude;
	int magnitude_sign = magnitude < 0 ? -1 : 1;
	int level = periodic->offset;
	int d, t;

	if (state->time_playing < state->envelope->attack_length) {
		d = magnitude - magnitude_sign * state->envelope->attack_level;
		magnitude = magnitude_sign * state->envelope->attack_level + d * state->time_playing / state->envelope->attack_length;
	} else if (state->effect.replay.length) {
		t = state->time_playing - state->effect.replay.length + state->envelope->fade_length;
		if (t > 0) {
			d = magnitude - magnitude_sign * state->envelope->fade_level;
			magnitude = magnitude - d * t / state->envelope->fade_length;
		}
	}

	switch (periodic->waveform) {
		case FF_SINE:
			level += fixp_sin16(state->phase) * magnitude / 0x7fff;
			break;
		case FF_SQUARE:
			level += (state->phase < 180 ? 1 : -1) * magnitude;
			break;
		case FF_TRIANGLE:
			level += abs(state->phase * magnitude * 2 / 360 - magnitude) * 2 - magnitude;
			break;
		case FF_SAW_UP:
			level += state->phase * magnitude * 2 / 360 - magnitude;
			break;
		case FF_SAW_DOWN:
			level += magnitude - state->phase * magnitude * 2 / 360;
			break;
	}

	return state->direction_gain * level / 0x7fff;
}

static __always_inline void dd_lg4ff_calculate_spring(struct dd_lg4ff_effect_state *state, struct dd_lg4ff_effect_parameters *parameters)
{
	struct ff_condition_effect *condition = &state->effect.u.condition[0];

	parameters->d1 = ((int)condition->center) - condition->deadband / 2;
	parameters->d2 = ((int)condition->center) + condition->deadband / 2;
	parameters->k1 = condition->left_coeff;
	parameters->k2 = condition->right_coeff;
	parameters->clip = (unsigned)condition->right_saturation;
}

static __always_inline void dd_lg4ff_calculate_resistance(struct dd_lg4ff_effect_state *state, struct dd_lg4ff_effect_parameters *parameters)
{
	struct ff_condition_effect *condition = &state->effect.u.condition[0];

	parameters->k1 = condition->left_coeff;
	parameters->k2 = condition->right_coeff;
	parameters->clip = (unsigned)condition->right_saturation;
}

static __always_inline struct ff_envelope *dd_lg4ff_effect_envelope(struct ff_effect *effect)
{
	switch (effect->type) {
		case FF_CONSTANT:
			return &effect->u.constant.envelope;
		case FF_RAMP:
			return &effect->u.ramp.envelope;
		case FF_PERIODIC:
			return &effect->u.periodic.envelope;
	}

	return NULL;
}

/*
 * Effect scheduling state machine, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:743-795). Advances start/play/stop timestamps and the
 * playing/updating flags off the FF core's ff_effect fields; called once
 * per effect from the timer tick (dd_lg4ff_timer, below).
 */
static __always_inline void dd_lg4ff_update_state(struct dd_lg4ff_effect_state *state, const unsigned long now)
{
	struct ff_effect *effect = &state->effect;
	unsigned long phase_time;

	if (!__test_and_set_bit(DD_LG4FF_FF_EFFECT_ALLSET, &state->flags)) {
		state->play_at = state->start_at + effect->replay.delay;
		if (!test_bit(DD_LG4FF_FF_EFFECT_UPDATING, &state->flags)) {
			state->updated_at = state->play_at;
		}
		state->direction_gain = fixp_sin16(effect->direction * 360 / 0x10000);
		if (effect->type == FF_PERIODIC) {
			state->phase_adj = effect->u.periodic.phase * 360 / effect->u.periodic.period;
		}
		if (effect->replay.length) {
			state->stop_at = state->play_at + effect->replay.length;
		}
	}

	if (__test_and_clear_bit(DD_LG4FF_FF_EFFECT_UPDATING, &state->flags)) {
		__clear_bit(DD_LG4FF_FF_EFFECT_PLAYING, &state->flags);
		state->play_at = state->updated_at + effect->replay.delay;
		state->direction_gain = fixp_sin16(effect->direction * 360 / 0x10000);
		if (effect->replay.length) {
			state->stop_at = state->updated_at + effect->replay.length;
		}
		if (effect->type == FF_PERIODIC) {
			state->phase_adj = state->phase;
		}
	}

	state->envelope = dd_lg4ff_effect_envelope(effect);

	state->slope = 0;
	if (effect->type == FF_RAMP && effect->replay.length) {
		state->slope = ((effect->u.ramp.end_level - effect->u.ramp.start_level) << 16) / (effect->replay.length - state->envelope->attack_length - state->envelope->fade_length);
	}

	if (!test_bit(DD_LG4FF_FF_EFFECT_PLAYING, &state->flags) && time_after_eq(now,
				state->play_at) && (effect->replay.length == 0 ||
					time_before(now, state->stop_at))) {
		__set_bit(DD_LG4FF_FF_EFFECT_PLAYING, &state->flags);
	}

	if (test_bit(DD_LG4FF_FF_EFFECT_PLAYING, &state->flags)) {
		state->time_playing = DD_LG4FF_TIME_DIFF(now, state->play_at);
		if (effect->type == FF_PERIODIC) {
			phase_time = DD_LG4FF_TIME_DIFF(now, state->updated_at);
			state->phase = (phase_time % effect->u.periodic.period) * 360 / effect->u.periodic.period;
			state->phase += state->phase_adj % 360;
		}
	}
}

/*
 * Partial mirror of struct usbhid_device from the kernel's
 * drivers/hid/usbhid/usbhid.h, trimmed to the fields dd_lg4ff_timer() below
 * needs: outhead/outtail, the USB output-report FIFO indices used to detect
 * a stalled SET_REPORT queue. That header is kernel-internal and is not
 * exported by kernel-devel on several distributions (see the hid_to_usb_dev
 * note in hid-logitech-hidpp.c for the same class of problem with a
 * different symbol from it), so entry->hid->driver_data is read through
 * this local, offset-compatible mirror instead of including it. Field
 * order and types must track upstream exactly up to and including outtail;
 * checked against drivers/hid/usbhid/usbhid.h as shipped in Linux 7.1.
 */
struct dd_lg4ff_usbhid_device {
	struct hid_device *hid;
	struct usb_interface *intf;
	int ifnum;
	unsigned int bufsize;
	struct urb *urbin;
	char *inbuf;
	dma_addr_t inbuf_dma;
	struct urb *urbctrl;
	struct usb_ctrlrequest *cr;
	struct hid_control_fifo ctrl[HID_CONTROL_FIFO_SIZE];
	unsigned char ctrlhead, ctrltail;
	char *ctrlbuf;
	dma_addr_t ctrlbuf_dma;
	unsigned long last_ctrl;
	struct urb *urbout;
	struct hid_output_fifo out[HID_CONTROL_FIFO_SIZE];
	unsigned char outhead, outtail;
};

/*
 * hrtimer effect engine, ported from new-lg4ff (hid-lg4ff.c:797-968). Sums
 * CONSTANT/RAMP/PERIODIC into slot 0 and condition effects (SPRING/DAMPER/
 * FRICTION/INERTIA) into slots 1-3, applies master/wheel gain and the
 * spring/damper/friction level scalers, then pushes any slot whose command
 * changed out over SET_REPORT. The timer_mode back-off below is load-bearing:
 * without it a stalled USB output queue gets more SET_REPORT commands piled
 * onto it every tick, which only makes the stall worse.
 *
 * new-lg4ff's LED calibration output (its CONFIG_LEDS_CLASS block, gated on
 * the ffb_leds param) is intentionally not ported here: it needs
 * dd_lg4ff_set_leds(), which does not exist yet, and ffb_leds/profile are
 * not among this task's module params.
 */
static __always_inline int dd_lg4ff_timer(struct dd_lg4ff_device_entry *entry)
{
	struct dd_lg4ff_usbhid_device *usbhid = entry->hid->driver_data;
	struct dd_lg4ff_slot *slot;
	struct dd_lg4ff_effect_state *state;
	struct dd_lg4ff_effect_parameters parameters[4];
	unsigned long jiffies_now = jiffies;
	unsigned long now = DD_LG4FF_JIFFIES2MS(jiffies_now);
	unsigned long flags;
	unsigned gain;
	int current_period;
	int count;
	int effect_id;
	int i;
	int ffb_level;

	if (dd_lg4ff_timer_mode > 0 && usbhid->outhead != usbhid->outtail) {
		current_period = dd_lg4ff_timer_msecs;
		if (dd_lg4ff_timer_mode == 1) {
			dd_lg4ff_timer_msecs *= 2;
			hid_info(entry->hid, "Commands stacking up, increasing timer period to %d ms.", dd_lg4ff_timer_msecs);
		} else {
			DD_LG4FF_DEBUG("Commands stacking up, delaying timer.");
		}
		return current_period;
	}

	memset(parameters, 0, sizeof(parameters));

	gain = (unsigned)entry->wdata.master_gain * entry->wdata.gain / 0xffff;

	spin_lock_irqsave(&entry->timer_lock, flags);

	count = entry->effects_used;

	for (effect_id = 0; effect_id < DD_LG4FF_MAX_EFFECTS; effect_id++) {

		if (!count) {
			break;
		}

		state = &entry->states[effect_id];

		if (!test_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags)) {
			continue;
		}

		count--;

		if (test_bit(DD_LG4FF_FF_EFFECT_ALLSET, &state->flags)) {
			if (state->effect.replay.length && time_after_eq(now, state->stop_at)) {
				DD_LG4FF_STOP_EFFECT(state);
				if (!--state->count) {
					entry->effects_used--;
					continue;
				}
				__set_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags);
				state->start_at = state->stop_at;
			}
		}

		dd_lg4ff_update_state(state, now);

		if (!test_bit(DD_LG4FF_FF_EFFECT_PLAYING, &state->flags)) {
			continue;
		}

		switch (state->effect.type) {
			case FF_CONSTANT:
				parameters[0].level += dd_lg4ff_calculate_constant(state);
				break;
			case FF_RAMP:
				parameters[0].level += dd_lg4ff_calculate_ramp(state);
				break;
			case FF_PERIODIC:
				parameters[0].level += dd_lg4ff_calculate_periodic(state);
				break;
			case FF_SPRING:
				if (state->slot != 0) {
					dd_lg4ff_calculate_spring(state, &parameters[state->slot]);
				}
				break;
			case FF_DAMPER:
			case FF_FRICTION:
			case FF_INERTIA:
				if (state->slot != 0) {
					dd_lg4ff_calculate_resistance(state, &parameters[state->slot]);
				}
		}
	}

	spin_unlock_irqrestore(&entry->timer_lock, flags);

	parameters[0].level = (long)parameters[0].level * gain / 0xffff;

	ffb_level = abs(parameters[0].level);
	for (i = 1; i < 4; i++) {
		parameters[i].k1 = (long)parameters[i].k1 * gain / 0xffff;
		parameters[i].k2 = (long)parameters[i].k2 * gain / 0xffff;
		switch (entry->slots[i].effect_type) {
			case FF_SPRING:
				parameters[i].clip = parameters[i].clip * dd_lg4ff_spring_level / 100;
				break;
			case FF_DAMPER:
				parameters[i].clip = parameters[i].clip * dd_lg4ff_damper_level / 100;
				break;
			case FF_FRICTION:
				parameters[i].clip = parameters[i].clip * dd_lg4ff_friction_level / 100;
				break;
		}
		parameters[i].clip = parameters[i].clip * gain / 0xffff;
		ffb_level += parameters[i].clip * 0x7fff / 0xffff;
	}
	if (ffb_level > entry->peak_ffb_level) {
		entry->peak_ffb_level = ffb_level;
	}

	for (i = 0; i < 4; i++) {
		slot = &entry->slots[i];
		dd_lg4ff_update_slot(slot, &parameters[i]);
		if (slot->is_updated) {
			dd_lg4ff_send_cmd(entry, slot->current_cmd);
			slot->is_updated = 0;
		}
	}

	return 0;
}

/*
 * hrtimer callback wrapper, ported from new-lg4ff (hid-lg4ff.c:970-994).
 * Re-arms at the back-off period dd_lg4ff_timer() just returned, or at the
 * normal tick period while effects are still playing, or stops the timer
 * once nothing is left to play. Not yet assigned to entry->hrtimer.function:
 * that wiring lands with dd_lg4ff_init() in a later task.
 */
static enum hrtimer_restart __maybe_unused dd_lg4ff_timer_hires(struct hrtimer *t)
{
	struct dd_lg4ff_device_entry *entry = container_of(t, struct dd_lg4ff_device_entry, hrtimer);
	int delay_timer;
	int overruns;

	delay_timer = dd_lg4ff_timer(entry);

	if (delay_timer) {
		hrtimer_forward_now(&entry->hrtimer, ms_to_ktime(delay_timer));
		return HRTIMER_RESTART;
	}

	if (entry->effects_used) {
		overruns = hrtimer_forward_now(&entry->hrtimer, ms_to_ktime(dd_lg4ff_timer_msecs));
		overruns--;
		if (unlikely(overruns > 0))
			DD_LG4FF_DEBUG("Overruns: %d", overruns);
		return HRTIMER_RESTART;
	}

	DD_LG4FF_DEBUG("Stop timer.");
	return HRTIMER_NORESTART;
}

/*
 * Slot/loop-mode initializer, ported from new-lg4ff (hid-lg4ff.c:996-1019).
 * Sends the 0x0d fixed-loop-mode command, then resets and re-sends all four
 * slots empty. Not yet called: its caller, dd_lg4ff_init(), arrives in a
 * later task.
 */
static void __maybe_unused dd_lg4ff_init_slots(struct dd_lg4ff_device_entry *entry)
{
	struct dd_lg4ff_effect_parameters parameters;
	u8 cmd[8] = {0};
	int i;

	/* Set/unset fixed loop mode */
	cmd[0] = 0x0d;
	cmd[1] = dd_lg4ff_fixed_loop ? 1 : 0;
	dd_lg4ff_send_cmd(entry, cmd);

	memset(&entry->states, 0, sizeof(entry->states));
	memset(&entry->slots, 0, sizeof(entry->slots));
	memset(&parameters, 0, sizeof(parameters));

	entry->slots[0].effect_type = FF_CONSTANT;

	for (i = 0; i < 4; i++) {
		entry->slots[i].id = i;
		dd_lg4ff_update_slot(&entry->slots[i], &parameters);
		dd_lg4ff_send_cmd(entry, entry->slots[i].current_cmd);
		entry->slots[i].is_updated = 0;
	}
}

/*
 * Ported from new-lg4ff (hid-lg4ff.c:1021-1027): cmd[0]=0xf3 tells the wheel
 * to drop whatever it is currently playing. Not yet called; wired up
 * alongside dd_lg4ff_init_slots() in a later task.
 */
static void __maybe_unused dd_lg4ff_stop_effects(struct dd_lg4ff_device_entry *entry)
{
	u8 cmd[7] = {0};

	cmd[0] = 0xf3;
	dd_lg4ff_send_cmd(entry, cmd);
}

/*
 * ff->upload callback, ported from new-lg4ff (hid-lg4ff.c:1029-1064). Pure
 * bookkeeping: stores the ff_effect into entry->states[id] and marks it
 * updating if it was already playing. No hardware I/O. Not yet wired to
 * an input_dev's ff_device: that assignment lands with dd_lg4ff_init() in
 * a later task.
 */
static int __maybe_unused dd_lg4ff_upload_effect(struct input_dev *dev, struct ff_effect *effect, struct ff_effect *old)
{
	struct hid_device *hid = input_get_drvdata(dev);
	struct dd_lg4ff_device_entry *entry;
	struct dd_lg4ff_effect_state *state;
	unsigned long now = DD_LG4FF_JIFFIES2MS(jiffies);
	unsigned long flags;

	entry = dd_lg4ff_get_entry(hid);
	if (entry == NULL) {
		return -EINVAL;
	}

	if (effect->type == FF_PERIODIC && effect->u.periodic.period == 0) {
		return -EINVAL;
	}

	state = &entry->states[effect->id];

	if (test_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags) && effect->type != state->effect.type) {
		return -EINVAL;
	}

	spin_lock_irqsave(&entry->timer_lock, flags);

	state->effect = *effect;

	if (test_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags)) {
		__set_bit(DD_LG4FF_FF_EFFECT_UPDATING, &state->flags);
		state->updated_at = now;
	}

	spin_unlock_irqrestore(&entry->timer_lock, flags);

	return 0;
}

/*
 * ff->playback callback, ported from new-lg4ff (hid-lg4ff.c:1066-1131).
 * Starts the hrtimer on the first effect and stops it when the last one
 * ends; allocates a condition slot (1-3) for SPRING/DAMPER/FRICTION/INERTIA
 * on start and frees it on stop. INERTIA and FRICTION on a wheel lacking
 * DD_LG4FF_CAP_FRICTION are cast to DAMPER, matching what the Windows driver
 * does for these toy-strength wheels. Not yet wired to an input_dev's
 * ff_device: that assignment lands with dd_lg4ff_init() in a later task.
 */
static int __maybe_unused dd_lg4ff_play_effect(struct input_dev *dev, int effect_id, int value)
{
	struct hid_device *hid = input_get_drvdata(dev);
	struct dd_lg4ff_device_entry *entry;
	struct dd_lg4ff_effect_state *state;
	unsigned long now = DD_LG4FF_JIFFIES2MS(jiffies);
	unsigned long flags;
	int i;

	entry = dd_lg4ff_get_entry(hid);
	if (entry == NULL) {
		return -EINVAL;
	}

	state = &entry->states[effect_id];

	spin_lock_irqsave(&entry->timer_lock, flags);

	if (value > 0) {
		if (test_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags)) {
			DD_LG4FF_STOP_EFFECT(state);
		} else {
			entry->effects_used++;
			if (!hrtimer_active(&entry->hrtimer)) {
				hrtimer_start(&entry->hrtimer, ms_to_ktime(dd_lg4ff_timer_msecs), HRTIMER_MODE_REL);
				DD_LG4FF_DEBUG("Start timer.");
			}
			if ((state->effect.type == FF_SPRING || state->effect.type == FF_DAMPER
					|| state->effect.type == FF_FRICTION || state->effect.type == FF_INERTIA)
					&& state->slot == 0) {
				/* Find a free slot */
				for (i = 1; i < 4 && entry->slots[i].effect_type != 0; i++)
					;
				if (i < 4) {
					state->slot = i;
					entry->slots[i].effect_type = state->effect.type;

					/* Cast unsupported effect types to "damper": this is what the Windows
					 * driver does.
					 * This is not physically plausible, but we are working with toy-strength
					 * wheels that won't let you feel more than "big value = wheel stuck" */
					if (state->effect.type == FF_INERTIA
							|| (state->effect.type == FF_FRICTION && !(entry->wdata.capabilities & DD_LG4FF_CAP_FRICTION))) {
						entry->slots[i].effect_type = FF_DAMPER;
					}
				}
			}
		}
		__set_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags);
		state->start_at = now;
		state->count = value;
	} else {
		if (test_bit(DD_LG4FF_FF_EFFECT_STARTED, &state->flags)) {
			DD_LG4FF_STOP_EFFECT(state);
			entry->effects_used--;
			if (state->slot) {
				entry->slots[state->slot].effect_type = 0;
				state->slot = 0;
			}
		}
	}

	spin_unlock_irqrestore(&entry->timer_lock, flags);

	return 0;
}

/*
 * Per-device state initializer, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:1255-1283). Fills wdata's product/range/capabilities fields
 * from the wheel table row (dd_lg4ff_devices[]) and, for a multimode wheel,
 * layers on the alternate-mode bitmask and the real_tag/real_name pointers
 * used by mode switching. Not yet called: its caller, dd_lg4ff_init(),
 * arrives in a later task.
 */
static void __maybe_unused dd_lg4ff_init_wheel_data(struct dd_lg4ff_wheel_data * const wdata, const struct dd_lg4ff_wheel *wheel,
				  const struct dd_lg4ff_multimode_wheel *mmode_wheel,
				  const u16 real_product_id)
{
	u32 alternate_modes = 0;
	const char *real_tag = NULL;
	const char *real_name = NULL;

	if (mmode_wheel) {
		alternate_modes = mmode_wheel->alternate_modes;
		real_tag = mmode_wheel->real_tag;
		real_name = mmode_wheel->real_name;
	}

	{
		struct dd_lg4ff_wheel_data t_wdata =  { .product_id = wheel->product_id,
						     .real_product_id = real_product_id,
						     .combine = 0,
						     .min_range = wheel->min_range,
						     .max_range = wheel->max_range,
						     .set_range = wheel->set_range,
						     .alternate_modes = alternate_modes,
						     .real_tag = real_tag,
						     .real_name = real_name,
						     .capabilities = wheel->capabilities };

		memcpy(wdata, &t_wdata, sizeof(t_wdata));
	}
}

/*
 * Default autocentering command sender, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:1287-1350). Compatible with every wheel we carry (the G923
 * family); the Formula Force EX variant (hid-lg4ff.c:1353-1376) is dropped,
 * matching the trimmed device table. Not yet wired to an input_dev's
 * ff_device: that assignment lands with dd_lg4ff_init() in a later task.
 */
static void __maybe_unused dd_lg4ff_set_autocenter_default(struct input_dev *dev, u16 magnitude)
{
	struct hid_device *hid = input_get_drvdata(dev);
	u8 cmd[7];
	u32 expand_a, expand_b;
	struct dd_lg4ff_device_entry *entry;

	entry = dd_lg4ff_get_entry(hid);
	if (entry == NULL) {
		return;
	}

	entry->wdata.autocenter = magnitude;

	/* De-activate Auto-Center */
	if (magnitude == 0) {
		cmd[0] = 0xf5;
		cmd[1] = 0x00;
		cmd[2] = 0x00;
		cmd[3] = 0x00;
		cmd[4] = 0x00;
		cmd[5] = 0x00;
		cmd[6] = 0x00;
		dd_lg4ff_send_cmd(entry, cmd);
		return;
	}

	if (magnitude <= 0xaaaa) {
		expand_a = 0x0c * magnitude;
		expand_b = 0x80 * magnitude;
	} else {
		expand_a = (0x0c * 0xaaaa) + 0x06 * (magnitude - 0xaaaa);
		expand_b = (0x80 * 0xaaaa) + 0xff * (magnitude - 0xaaaa);
	}

	/* Adjust for non-MOMO wheels */
	switch (entry->wdata.product_id) {
	case USB_DEVICE_ID_LOGITECH_MOMO_WHEEL:
	case USB_DEVICE_ID_LOGITECH_MOMO_WHEEL2:
		break;
	default:
		expand_a = expand_a >> 1;
		break;
	}

	cmd[0] = 0xfe;
	cmd[1] = 0x0d;
	cmd[2] = expand_a / 0xaaaa;
	cmd[3] = expand_a / 0xaaaa;
	cmd[4] = expand_b / 0xaaaa;
	cmd[5] = 0x00;
	cmd[6] = 0x00;
	dd_lg4ff_send_cmd(entry, cmd);

	/* Activate Auto-Center */
	cmd[0] = 0x14;
	cmd[1] = 0x00;
	cmd[2] = 0x00;
	cmd[3] = 0x00;
	cmd[4] = 0x00;
	cmd[5] = 0x00;
	cmd[6] = 0x00;
	dd_lg4ff_send_cmd(entry, cmd);
}

/*
 * Range-set command sender for the G25/G27/DFGT/G923 family, ported
 * verbatim from new-lg4ff (hid-lg4ff.c:1379-1398). The Driving Force Pro
 * variant (hid-lg4ff.c:1401-1455) is dropped: no wheel in the trimmed
 * device table needs it. Wired into dd_lg4ff_devices[]'s G923 row above.
 */
static void dd_lg4ff_set_range_g25(struct hid_device *hid, u16 range)
{
	struct dd_lg4ff_device_entry *entry;
	u8 cmd[7];

	entry = dd_lg4ff_get_entry(hid);
	if (entry == NULL) {
		return;
	}

	DD_LG4FF_DEBUG("G25/G27/DFGT: setting range to %u", range);

	cmd[0] = 0xf8;
	cmd[1] = 0x81;
	cmd[2] = range & 0x00ff;
	cmd[3] = (range & 0xff00) >> 8;
	cmd[4] = 0x00;
	cmd[5] = 0x00;
	cmd[6] = 0x00;
	dd_lg4ff_send_cmd(entry, cmd);
}

/*
 * ff->set_gain callback, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:1457-1468). Just stores the gain; dd_lg4ff_timer() (above)
 * is what folds it into the force math on the next tick. Not yet wired to
 * an input_dev's ff_device: that assignment lands with dd_lg4ff_init() in
 * a later task.
 */
static void __maybe_unused dd_lg4ff_set_gain(struct input_dev *dev, u16 gain)
{
	struct hid_device *hid = input_get_drvdata(dev);
	struct dd_lg4ff_device_entry *entry;

	entry = dd_lg4ff_get_entry(hid);
	if (entry == NULL) {
		return;
	}

	entry->wdata.gain = gain;
}

int dd_lg4ff_init(struct hid_device *hdev)
{
	return 0;
}

void dd_lg4ff_deinit(struct hid_device *hdev)
{
}
