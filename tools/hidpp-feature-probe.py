#!/usr/bin/env python3
"""Ask a G923 which HID++ features it has, on every hidraw node it owns.

Read-only: this sends feature-lookup questions and reads the answers.
Nothing here writes a setting, lights an LED, or produces force.

Why both nodes are tried: HID++ answers on exactly one of a wheel's hidraw
interfaces, and which one differs between wheel editions. Asking all of them
finds it without anyone having to guess.

Why 0x8123 is in the list: it is the force-feedback feature the Xbox G923 is
known to expose, so it acts as a control. If 0x8123 is found, the probe and
the node are working and a "not supported" for anything else is a real
answer. If NOTHING is found on any node, the probe is at fault, not the
wheel.

    sudo python3 g923-hidpp-probe.py
"""
import glob
import os
import select
import sys

SHORT, LONG = 0x10, 0x11
DEV_INDEX = 0xFF            # wired device
ROOT_INDEX = 0x00           # the root feature, always index 0
FN_GET_FEATURE = 0x00
SW_ID = 0x0A                # our software id
TIMEOUT_S = 0.5

# Names are Logitech's own, from their published HID++ 2.0 feature registry
# (mirrored in Solaar's hidpp20_constants.py), not this project's guesses.
# 0x807A is the RPM indicator specifically, which is what makes it the right
# page to ask about rev lights; general LIGHTSYNC RGB is 0x8070/0x8071 and is
# asked here too in case that is where this edition keeps its rim lighting.
FEATURES = [
    (0x8123, "FORCE_FEEDBACK (known present on the Xbox G923: the CONTROL)"),
    (0x807A, "RPM_INDICATOR      <-- the rev display"),
    (0x807B, "RPM_LED_PATTERN    <-- the rev display's colours/pattern"),
    (0x8070, "COLOR_LED_EFFECTS  <-- general LIGHTSYNC"),
    (0x8071, "RGB_EFFECTS        <-- general LIGHTSYNC"),
    (0x8040, "BRIGHTNESS_CONTROL"),
    (0x8138, "OPERATING_RANGE"),
    (0x8139, "TRUE_FORCE"),
    (0x0003, "DEVICE_INFORMATION"),
]


def wheel_nodes():
    """Every hidraw node belonging to a Logitech wheel, with its usage page."""
    found = []
    for node in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        dev = os.path.realpath(os.path.join(node, "device"))
        try:
            with open(os.path.join(dev, "uevent")) as fh:
                uevent = fh.read()
        except OSError:
            continue
        if "046D" not in uevent.upper():
            continue
        try:
            with open(os.path.join(dev, "report_descriptor"), "rb") as fh:
                head = fh.read(3)
        except OSError:
            head = b""
        pid = ""
        for line in uevent.splitlines():
            if line.startswith("HID_ID="):
                pid = line.strip().split(":")[-1][-4:]
        found.append((f"/dev/{os.path.basename(node)}", pid, head.hex(" ")))
    return found


def ask(fd, feature_id):
    """Root.getFeature(feature_id) -> index, or None if unsupported."""
    req = bytes([SHORT, DEV_INDEX, ROOT_INDEX, FN_GET_FEATURE | SW_ID,
                 (feature_id >> 8) & 0xFF, feature_id & 0xFF, 0x00])
    try:
        os.write(fd, req)
    except OSError as exc:
        return None, f"write failed: {exc}"

    # The wheel also emits input reports; read until the matching reply
    # arrives or the budget runs out.
    deadline = select.select
    import time
    end = time.monotonic() + TIMEOUT_S
    while time.monotonic() < end:
        ready, _, _ = deadline([fd], [], [], max(0.0, end - time.monotonic()))
        if not ready:
            break
        try:
            resp = os.read(fd, 64)
        except OSError:
            break
        if len(resp) < 5 or resp[0] not in (SHORT, LONG):
            continue
        if resp[1] != DEV_INDEX or resp[2] != ROOT_INDEX:
            continue
        if resp[3] != (FN_GET_FEATURE | SW_ID):
            continue
        index = resp[4]
        return (index if index else None), None
    return None, "no reply"


def main():
    nodes = wheel_nodes()
    if not nodes:
        sys.exit("no Logitech hidraw nodes found")

    print("Logitech hidraw nodes on this machine:")
    for path, pid, head in nodes:
        print(f"  {path}   pid={pid}   descriptor starts {head}")
    print()

    any_answer = False
    for path, pid, _ in nodes:
        try:
            fd = os.open(path, os.O_RDWR)
        except OSError as exc:
            print(f"{path} (pid {pid}): cannot open ({exc}); try sudo")
            continue
        print(f"{path} (pid {pid}):")
        replied = False
        for fid, what in FEATURES:
            index, err = ask(fd, fid)
            if index is not None:
                replied = any_answer = True
                print(f"    0x{fid:04X}  index 0x{index:02X}   {what}")
            elif err is None:
                replied = True
                print(f"    0x{fid:04X}  not supported   {what}")
        if not replied:
            print("    no HID++ replies at all (this node does not speak HID++)")
        os.close(fd)
        print()

    if not any_answer:
        print("Nothing answered on any node. That points at the probe or at")
        print("permissions, NOT at the wheel. Please paste the whole output.")
    else:
        print("The line that matters is 0x807A. If it has an index, the rev")
        print("lights have a route and the next step is to light them.")


if __name__ == "__main__":
    main()
