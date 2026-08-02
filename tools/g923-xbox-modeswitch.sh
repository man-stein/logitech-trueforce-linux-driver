#!/bin/sh
# Switch a G923 Xbox edition out of console mode.
#
# The wheel enumerates as 046d:c26d speaking Xbox GIP, and has to be told to
# re-enumerate as 046d:c26e before it speaks HID++ and this driver can bind it.
#
# Two things this does that a bare usb_modeswitch call in a udev rule did not,
# both from a detailed report on a Legion Go running SteamOS (issue #52):
#
#   1. It releases whatever already holds the interfaces. In console mode
#      xbox-gip binds the wheel (xone does the same on other setups), and
#      usb_modeswitch cannot drive an interface another driver owns.
#
#   2. It is meant to be dispatched asynchronously, NOT from a udev RUN+=.
#      RUN+= runs inside the udev worker and holds the device lock for the
#      whole USB control transfer. On a machine whose built-in controllers are
#      internal USB devices, that wedged the USB stack hard enough to take the
#      desktop down and stop the machine booting while the wheel was attached.
#      73-logitech-g923-xbox-modeswitch.rules now dispatches this through
#      systemd-run --no-block so the worker returns immediately.
#
# Safe to run by hand, which is also the answer on a system without systemd:
#   sudo logi-g923-modeswitch
set -eu

VID=046d
PID=c26d
# Vendor mode-switch message, from the Windows driver's own sequence.
MSG=0f00010142

if ! command -v usb_modeswitch >/dev/null 2>&1; then
	echo "logi-g923-modeswitch: usb_modeswitch not installed" >&2
	exit 1
fi

# Release any driver bound to this wheel's interfaces. Failure is not fatal:
# an interface with no driver is already in the state we want.
for iface in /sys/bus/usb/devices/*:*.*; do
	[ -e "$iface/../idVendor" ] || continue
	v=$(cat "$iface/../idVendor" 2>/dev/null) || continue
	p=$(cat "$iface/../idProduct" 2>/dev/null) || continue
	[ "$v" = "$VID" ] || continue
	[ "$p" = "$PID" ] || continue
	[ -L "$iface/driver" ] || continue

	drv=$(basename "$(readlink -f "$iface/driver")")
	name=$(basename "$iface")
	echo "logi-g923-modeswitch: releasing $name from $drv" >&2
	echo "$name" > "/sys/bus/usb/drivers/$drv/unbind" 2>/dev/null || true
done

echo "logi-g923-modeswitch: switching $VID:$PID to PC mode" >&2
exec usb_modeswitch -v "$VID" -p "$PID" -M "$MSG" -C 0x03 -m 01 -r 81
