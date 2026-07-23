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
#define DD_LG4FF_DEBUG(...) pr_debug("dd_lg4ff: " __VA_ARGS__)
#define DD_LG4FF_TIME_DIFF(a, b) ({ \
		typecheck(unsigned long, a); \
		typecheck(unsigned long, b); \
		((a) - (long)(b)); })

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

/*
 * 7-byte SET_REPORT command senders, ported verbatim from new-lg4ff
 * (hid-lg4ff.c:480-514). dd_lg4ff_send_cmd_with_id() forces the report's
 * id first; it is used only by the mode-switch sequence, wired up once
 * that lands. Neither sender has a caller yet: the timer/upload/play path
 * that calls them arrives in a later task.
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

static void __maybe_unused dd_lg4ff_send_cmd(struct dd_lg4ff_device_entry *entry, u8 *cmd)
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
 * FRICTION 0x0e; op3 stops the slot). No caller yet; arrives with the
 * update/play path in a later task.
 */
static void __maybe_unused dd_lg4ff_update_slot(struct dd_lg4ff_slot *slot, struct dd_lg4ff_effect_parameters *parameters)
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
 * (hid-lg4ff.c:620-741). All __always_inline, so the compiler does not
 * warn about them lacking a caller yet (the timer tick that drives these
 * arrives in a later task).
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
 * playing/updating flags off the FF core's ff_effect fields; the timer
 * that calls this on each tick arrives in a later task.
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

int dd_lg4ff_init(struct hid_device *hdev)
{
	return 0;
}

void dd_lg4ff_deinit(struct hid_device *hdev)
{
}
