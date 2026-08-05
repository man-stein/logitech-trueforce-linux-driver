#!/usr/bin/env bash
#
# One-command setup and diagnosis for the logitech-trueforce-linux-driver.
#
#   sudo ./tools/setup.sh            Full setup: DKMS module + udev rule +
#                                    module load (migrating off any old
#                                    full-fork install),
#                                    then (if the SDK DLLs are staged) the
#                                    TrueForce shim into every Steam prefix
#                                    as the invoking user.
#   ./tools/setup.sh doctor          Diagnose every layer, change nothing.
#                                    Run as your normal user.
#   ./tools/setup.sh shim            Only the TrueForce shim step (as user).
#
# The full setup is idempotent: run it again after `git pull` or a kernel
# update and it converges.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Left behind by the old full-fork install; this scoped build must NOT
# blacklist hid-logitech-hidpp (that would strip the in-tree driver from
# the user's Logitech mice/keyboards). Removed during migration below.
OLD_BLACKLIST_FILE="/etc/modprobe.d/blacklist-hid-logitech-hidpp.conf"
UDEV_DST="/etc/udev/rules.d/70-logitech-trueforce.rules"
UDEV_FFB_DST="/etc/udev/rules.d/71-logi-ffb-uhid.rules"
UDEV_G923_DST="/etc/udev/rules.d/72-logitech-g923-rebind.rules"
UDEV_G923_XBOX_DST="/etc/udev/rules.d/73-logitech-g923-xbox-modeswitch.rules"
MODPROBE_DST="/etc/modprobe.d/hid-logitech-dd.conf"
MODESWITCH_DST="/usr/bin/logi-g923-modeswitch"
# Direct-drive wheels, then the G923 editions. doctor was written before the
# G923 was supported and checked only the first three, so every G923 owner was
# told "no wheel detected" with the wheel plugged in and working, and the
# driver-health section was skipped as a consequence (issue #27).
WHEEL_PIDS_DD="c276 c272 c268"
WHEEL_PIDS_G923="c266 c267 c26e"
WHEEL_PID_G923_CONSOLE="c26d"
WHEEL_PIDS="$WHEEL_PIDS_DD $WHEEL_PIDS_G923"
# Steam appids of the Logitech-SDK sims for launch-option checks:
#   ACC, AC EVO, AC, AMS2, Le Mans Ultimate, rFactor 2
# G HUB revises the SDK and the version is a directory name, so never assume
# one: a current install ships 1_3_12 and 9_1_1, and hardcoding the older
# pair made those invisible with no explanation (issue #54).
TF_PFX_GLOB="drive_c/Program Files/Logi/Trueforce/*"
SDK_SIM_APPIDS="805550 3058630 244210 1066890 2399420 365960"

pass=0; warn=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
wrn()  { printf '  \033[33mWARN\033[0m %s\n' "$1"; warn=$((warn+1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
say()  { printf '\033[1m%s\033[0m\n' "$1"; }

# The direct-drive wheels expose wheel_*; the G923 exposes the classic set
# (range, gain, autocenter) and no wheel_* at all. Look for either.
find_wheel_sysfs() {
	ls -d /sys/class/hidraw/*/device/wheel_range 2>/dev/null | head -1 | xargs -r dirname
}

# Whether a G923 of any edition is on USB, including the Xbox one still in
# console mode. Keyed on USB rather than on our sysfs, because the case the
# rebind rule exists for is precisely the one where the in-tree driver won
# and the wheel has none of our attributes to find.
g923_on_usb() {
	local re
	re="$(echo "$WHEEL_PIDS_G923 $WHEEL_PID_G923_CONSOLE" | tr ' ' '|')"
	lsusb 2>/dev/null | grep -qiE "046d:($re)"
}

find_g923_sysfs() {
	local d
	for d in /sys/class/hidraw/*/device/range; do
		[ -e "$d" ] || continue
		# wheel_range means a direct-drive wheel, which the caller above
		# already handles; this is only for the classic-only wheels.
		[ -e "$(dirname "$d")/wheel_range" ] && continue
		dirname "$d"
		return
	done
}

steam_roots() {
	local u_home
	u_home="$(getent passwd "${SUDO_USER:-$USER}" | cut -d: -f6)"
	for d in "$u_home/.steam/steam" "$u_home/.local/share/Steam"; do
		[ -d "$d/steamapps" ] && echo "$d"
	done | sort -u
}

# ---------------------------------------------------------------- doctor --
doctor() {
	say "logitech-trueforce-linux-driver doctor"
	echo

	say "[1/7] Kernel module"
	if [ -d /sys/module/hid_logitech_dd ]; then
		local loaded_ver repo_ver
		loaded_ver="$(cat /sys/module/hid_logitech_dd/version 2>/dev/null || echo unknown)"
		ok "hid_logitech_dd is loaded (version: $loaded_ver)"
		# Running module vs the source it came from. Pulling without
		# rebuilding leaves an old driver in memory and every symptom
		# belongs to code nobody is reading any more, which is a
		# spectacularly good way to waste an afternoon.
		if [ -d "$REPO_ROOT/.git" ]; then
			repo_ver="$(git -C "$REPO_ROOT" describe --tags --always 2>/dev/null)"
			if [ -n "$repo_ver" ] && [ "$loaded_ver" != "$repo_ver" ]; then
				wrn "the loaded module is $loaded_ver but this checkout is $repo_ver - rebuild so you are testing the code you have (run: sudo ./tools/setup.sh)"
			elif [ -n "$repo_ver" ]; then
				ok "module matches this checkout ($repo_ver)"
			fi
		fi
	else
		bad "hid_logitech_dd is not loaded (run: sudo ./tools/setup.sh)"
	fi
	# App versions, when the tools are on PATH; bug reports want these.
	for tool in logi-wheel logi-wheel-gui logi-ffb logi-tf-sim; do
		if command -v "$tool" >/dev/null 2>&1; then
			ok "$tool on PATH ($("$tool" --version 2>/dev/null || echo "version flag unsupported"))"
		fi
	done
	# No `grep -q` here: under `set -o pipefail`, -q exits on the first
	# match (our module sorts first in dkms output), dkms catches SIGPIPE
	# mid-print and the successful pipeline reports failure. Reading the
	# full stream avoids the race.
	if dkms status 2>/dev/null | grep '^logitech-trueforce.*installed' >/dev/null; then
		ok "DKMS package installed (survives kernel updates)"
	else
		wrn "no DKMS install found - a manually insmod'ed module will not survive a reboot or kernel update (run: sudo ./tools/setup.sh)"
	fi
	if [ -f "$OLD_BLACKLIST_FILE" ]; then
		wrn "stale blacklist from the old full-fork install present ($OLD_BLACKLIST_FILE) - it strips the in-tree driver from your other Logitech devices; remove it (run: sudo ./tools/setup.sh)"
	fi
	if dkms status 2>/dev/null | grep '^hid-logitech-hidpp.*installed' >/dev/null; then
		wrn "old full-fork DKMS package 'hid-logitech-hidpp' still installed - it shadowed the in-tree driver for all Logitech devices; remove it (run: sudo ./tools/setup.sh)"
	fi

	echo
	say "[2/7] Wheel"
	local usbline
	local pid_re console_line
	pid_re="$(echo "$WHEEL_PIDS" | tr ' ' '|')"
	usbline="$(lsusb 2>/dev/null | grep -iE "046d:($pid_re)")"
	console_line="$(lsusb 2>/dev/null | grep -iE "046d:$WHEEL_PID_G923_CONSOLE")"
	if [ -n "$usbline" ]; then
		# One line per wheel. More than one is normal here (a G923 and a
		# direct-drive base together), and printing the list as a single
		# value left every wheel after the first unlabelled.
		while IFS= read -r l; do
			[ -n "$l" ] && ok "wheel on USB: ${l#*ID }"
		done <<< "$usbline"
	elif [ -n "$console_line" ]; then
		# Not "no wheel": a G923 Xbox that never left console mode. Saying
		# nothing was found sends the owner looking for the wrong fault.
		bad "G923 Xbox edition is in console mode ($WHEEL_PID_G923_CONSOLE) and unusable until it switches to $WHEEL_PIDS_G923; install usb_modeswitch and replug (see [4] for the rule and helper)"
	else
		wrn "no wheel detected on USB (plug it in and re-run doctor; everything below that needs the wheel is skipped)"
	fi

	local bound_generic=0 bound_ours=0
	local pid_up
	for pid_up in $(echo "$WHEEL_PIDS" | tr 'a-z ' 'A-Z\n'); do
	for d in /sys/bus/hid/devices/0003:046D:${pid_up}.*; do
		[ -e "$d" ] || continue
		case "$(basename "$(readlink -f "$d/driver" 2>/dev/null)")" in
			logitech-dd) bound_ours=$((bound_ours+1));;
			hid-generic) bound_generic=$((bound_generic+1));;
		esac
	done
	done
	if [ "$bound_ours" -gt 0 ] && [ "$bound_generic" -eq 0 ]; then
		ok "all $bound_ours wheel interfaces bound to our driver"
	elif [ "$bound_generic" -gt 0 ]; then
		bad "$bound_generic wheel interface(s) stuck on hid-generic (run: sudo ./tools/rebind-wheel.sh)"
	fi

	echo
	say "[3/7] Driver health"
	local W G
	W="$(find_wheel_sysfs)"
	G="$(find_g923_sysfs)"
	if [ -n "$W" ]; then
		ok "wheel_* sysfs present ($W)"
		local fw
		fw="$(cat "$W/wheel_firmware" 2>/dev/null | tr '\n' ' ')"
		[ -n "$fw" ] && ok "firmware: $fw" || wrn "wheel_firmware unreadable"
		ok "range=$(cat "$W/wheel_range" 2>/dev/null) strength=$(cat "$W/wheel_strength" 2>/dev/null)% mode=$(cat "$W/wheel_mode" 2>/dev/null)"
		# The G923's equivalent was reported here and this one was not,
		# which left direct-drive owners with no way to see whether the
		# 90-degree healing was on.
		case "$(cat "$W/wheel_range_restore" 2>/dev/null)" in
			1) ok "wheel_range_restore on (puts the range back if a game moves it)";;
			0) wrn "wheel_range_restore off - a game that collapses your rotation to 90 degrees will stay that way (echo 1 > $W/wheel_range_restore)";;
		esac
	fi
	if [ -n "$G" ]; then
		# A G923 has no wheel_* files at all. Reporting their absence as a
		# fault told owners their driver was not bound when it was. Checked
		# independently of the block above so a rig with both wheels gets
		# both reported rather than whichever was found first.
		ok "G923 classic sysfs present ($G)"
		local g_range g_restore
		g_range="$(cat "$G/range" 2>/dev/null)"
		[ -n "$g_range" ] && ok "range=$g_range" || wrn "range unreadable"
		g_restore="$(cat "$G/range_restore" 2>/dev/null)"
		case "$g_restore" in
			1) ok "range_restore on (puts the range back if a game moves it)";;
			0) wrn "range_restore off (echo 1 > $G/range_restore to re-enable)";;
		esac
	fi
	if [ -z "$W" ] && [ -z "$G" ]; then
		[ -n "$usbline" ] && bad "wheel on USB but no sysfs attributes - driver not bound (see [2])" \
			|| wrn "skipped (no wheel)"
	fi

	echo
	say "[4/7] Permissions (udev)"
	# Distro packages install rules under /usr/lib/udev/rules.d; setup.sh
	# uses /etc/udev/rules.d. Either location counts as installed.
	if [ -f "$UDEV_DST" ] || [ -f "/usr/lib/udev/rules.d/70-logitech-trueforce.rules" ]; then
		ok "udev rule installed"
	else
		wrn "udev rule missing - settings need sudo (run: sudo ./tools/setup.sh)"
	fi
	if [ -f "$UDEV_FFB_DST" ] || [ -f "/usr/lib/udev/rules.d/71-logi-ffb-uhid.rules" ]; then
		ok "logi-ffb uhid udev rule installed"
	else
		wrn "logi-ffb uhid udev rule missing - logi-ffb needs sudo for /dev/uhid (run: sudo ./tools/setup.sh)"
	fi
	# Only reported on a machine that has a G923. Printed unconditionally
	# these read as claims about the reader's hardware: an RS50 owner saw
	# "G923 (c266/c267/c26e) rebind rule installed" in a report about his
	# own wheel and reasonably concluded doctor had misidentified it (#54).
	if g923_on_usb; then
		if [ -f "$UDEV_G923_DST" ] || [ -f "/usr/lib/udev/rules.d/72-logitech-g923-rebind.rules" ]; then
			ok "G923 (c266/c267/c26e) rebind rule installed"
		else
			wrn "G923 rebind rule missing - the in-tree driver may keep winning the bind race on c266/c267/c26e (run: sudo ./tools/setup.sh)"
		fi
		if [ -f "$UDEV_G923_XBOX_DST" ] || [ -f "/usr/lib/udev/rules.d/73-logitech-g923-xbox-modeswitch.rules" ]; then
			ok "G923 Xbox edition (c26d) mode-switch rule installed"
		else
			wrn "G923 Xbox mode-switch rule missing - the Xbox edition will not switch out of console mode (run: sudo ./tools/setup.sh)"
		fi
		# Checked separately from the rule above: the rule dispatches this
		# through systemd-run with the output discarded, so a missing helper
		# leaves no trace anywhere and simply looks like a wheel that never
		# enumerates (issue #27). Inside this guard because its warning
		# refers to "the rule above", which is only printed here.
		if [ -x "$MODESWITCH_DST" ]; then
			ok "G923 Xbox mode-switch helper installed"
		else
			wrn "G923 Xbox mode-switch helper missing ($MODESWITCH_DST) - the rule above cannot do anything without it, and the Xbox edition will look like a dead wheel (run: sudo ./tools/setup.sh)"
		fi
	fi
	if [ -f "$MODPROBE_DST" ]; then
		ok "hid-logitech-dd modprobe.d config installed"
	else
		wrn "hid-logitech-dd modprobe.d config missing (run: sudo ./tools/setup.sh)"
	fi
	if [ -n "$W" ]; then
		if [ -w "$W/wheel_range" ] && [ -w "$W/range" ]; then
			ok "settings writable as $USER"
		else
			wrn "settings not writable as $USER - replug the wheel so the udev rule reapplies (it makes the wheel settings writable with no group setup)"
		fi
	fi

	echo
	say "[5/7] TrueForce SDK DLLs (only needed for TrueForce in Proton sims)"
	local dll_missing=0
	for f in "sdk/Logi/Trueforce/*/trueforce_sdk_x64.dll" \
		 "sdk/Logi/Trueforce/*/trueforce_sdk_x86.dll" \
		 "sdk/Logi/wheel_sdk/*/logi_steering_wheel_x64.dll" \
		 "sdk/Logi/wheel_sdk/*/logi_steering_wheel_x86.dll"; do
		ls "$REPO_ROOT"/$f >/dev/null 2>&1 || dll_missing=$((dll_missing+1))
	done
	if [ "$dll_missing" -eq 0 ]; then
		local tf_ver
		tf_ver="$(ls -1 "$REPO_ROOT"/sdk/Logi/Trueforce 2>/dev/null | grep -E '^[0-9_]+$' | sort -V | tail -1)"
		ok "all four SDK DLLs staged in the repo${tf_ver:+ (Trueforce $tf_ver)}"
	else
		wrn "$dll_missing of 4 SDK DLLs not staged (see the wiki's Force-feedback-in-games page; standard FFB works without them)"
	fi

	echo
	say "[6/7] Steam prefixes (shim)"
	local roots found_pfx=0 shimmed=0
	roots="$(steam_roots)"
	if [ -z "$roots" ]; then
		wrn "no Steam installation found for $USER"
	else
		# Only the sims that actually load Logitech's SDK need the shim.
		# Counting every Proton prefix meant a warning that scaled with a
		# person's library rather than with anything wrong: one real report
		# read "shim in 50 of 52 prefixes", which reads like a fault and is
		# in fact 50 shims more than that person needed. A missing shim in
		# some unrelated game's prefix is not a problem to solve.
		while IFS= read -r root; do
			for appid in $SDK_SIM_APPIDS; do
				pfx="$root/steamapps/compatdata/$appid/pfx"
				[ -d "$pfx" ] || continue
				found_pfx=$((found_pfx+1))
				ls "$pfx"/drive_c/Program\ Files/Logi/Trueforce/*/trueforce_sdk_x64.dll >/dev/null 2>&1 && shimmed=$((shimmed+1))
			done
		done <<< "$roots"
		if [ "$found_pfx" -gt 0 ] && [ "$shimmed" -eq "$found_pfx" ]; then
			ok "TrueForce shim present in all $found_pfx installed SDK sim(s)"
		elif [ "$shimmed" -gt 0 ]; then
			wrn "shim in $shimmed of $found_pfx installed SDK sim(s) (run: ./tools/setup.sh shim)"
		elif [ "$found_pfx" -gt 0 ]; then
			wrn "shim not installed in any of the $found_pfx installed SDK sim(s) (run: ./tools/setup.sh shim)"
		fi

		# If the rotation shim is installed, Logitech's library has to be
		# beside it under the name its forwards resolve through. Without
		# that, the four calls the shim answers itself keep working and
		# the fifty-four it forwards do not, which is a wheel that steers
		# to full lock and produces no force at all (issue #27).
		local proxied=0 orphaned=0
		while IFS= read -r root; do
			for appid in $SDK_SIM_APPIDS; do
				local d; d=$(ls -d "$root/steamapps/compatdata/$appid/pfx/"$TF_PFX_GLOB 2>/dev/null | tail -1)
				[ -n "$d" ] && [ -f "$d/trueforce_sdk_x64.dll" ] || continue
				# -a: it is a binary, and without it grep declines to match.
				# The string only appears in our proxy (it is the forward
				# target); Logitech's own library has no mention of it.
				grep -aq "trueforce_real" "$d/trueforce_sdk_x64.dll" 2>/dev/null || continue
				proxied=$((proxied+1))
				[ -f "$d/trueforce_real.dll" ] || orphaned=$((orphaned+1))
			done
		done <<< "$roots"
		if [ "$orphaned" -gt 0 ]; then
			bad "rotation shim installed in $orphaned prefix(es) without Logitech's library beside it - those games get no force feedback (re-run: ./tools/install-tf-shim.sh --all-steam --range-proxy)"
		elif [ "$proxied" -gt 0 ]; then
			ok "rotation shim installed in $proxied SDK sim(s), with Logitech's library beside it"
		fi
	fi

	echo
	say "[7/7] Per-game launch options (PROTON_ENABLE_HIDRAW=1)"
	local checked=0
	local appid
	for appid in $SDK_SIM_APPIDS; do
		local installed=0 has_opt=0
		while IFS= read -r root; do
			[ -d "$root/steamapps/compatdata/$appid" ] && installed=1
			for cfg in "$root"/userdata/*/config/localconfig.vdf; do
				[ -f "$cfg" ] || continue
				# Read LaunchOptions from the app's OWN block. Anchoring
				# on the first line mentioning the id anywhere was wrong
				# twice over: an appid appears several times in a
				# localconfig (six, in one real file), and if the block
				# it lands on has no LaunchOptions the scan runs on and
				# reports the NEXT app's. Measured against a real config
				# that got two of three wrong, both false negatives, so
				# it told owners to set a variable they had already set.
				if awk -v id="\"$appid\"" '
					$0 ~ "^[ \t]*" id "[ \t]*$" { cand = 1; depth = 0; seen = 0; next }
					cand {
						o = gsub(/\{/, "{"); c = gsub(/\}/, "}")
						if (o) seen = 1
						depth += o - c
						if (/"LaunchOptions"/) { print; exit }
						if (seen && depth <= 0) cand = 0
					}' "$cfg" | grep -q 'PROTON_ENABLE_HIDRAW=1'; then
					has_opt=1
				fi
			done
		done <<< "$(steam_roots)"
		[ "$installed" -eq 1 ] || continue
		checked=$((checked+1))
		if [ "$has_opt" -eq 1 ]; then
			ok "appid $appid has PROTON_ENABLE_HIDRAW=1"
		else
			wrn "appid $appid: PROTON_ENABLE_HIDRAW=1 not found in launch options (needed for TrueForce; set it in Steam > Properties)"
		fi
	done
	[ "$checked" -eq 0 ] && wrn "no known SDK sims found installed (nothing to check)"

	echo
	say "Summary: $pass pass, $warn warn, $fail fail"
	[ "$fail" -eq 0 ] || return 1
	return 0
}

# ----------------------------------------------------------------- setup --
do_shim() {
	if [ "$EUID" -eq 0 ]; then
		if [ -n "${SUDO_USER:-}" ]; then
			runuser -u "$SUDO_USER" -- "$REPO_ROOT/tools/install-tf-shim.sh" --all-steam
		else
			echo "shim must run as the user owning the Steam prefixes; run: ./tools/setup.sh shim (no sudo)"
			return 1
		fi
	else
		"$REPO_ROOT/tools/install-tf-shim.sh" --all-steam
	fi
}

setup() {
	if [ "$EUID" -ne 0 ]; then
		echo "error: full setup needs root (sudo $0). For diagnosis only: $0 doctor" >&2
		exit 1
	fi

	say "[1/5] Kernel module (DKMS) + udev rule"
	"$REPO_ROOT/tools/dkms-update.sh" || exit 1

	say "[2/5] Migrating off any old full-fork install"
	# The old build shipped its module as hid-logitech-hidpp - the SAME
	# name as the in-tree driver - so DKMS DISPLACED the genuine in-tree
	# module (backing it up under .../original_module/) and the installer
	# blacklisted it. This scoped build ships as hid-logitech-dd and claims
	# only the wheels, so fully undo the old state: drop the blacklist,
	# remove the old DKMS package, RESTORE the displaced in-tree module, and
	# delete the fork's leftover .ko. Skipping the restore would leave the
	# stale fork as the only hid-logitech-hidpp on disk, so mice/keyboards
	# would keep loading it instead of the maintained in-tree driver.
	local migrated=0 dkms_base=/var/lib/dkms/hid-logitech-hidpp
	if [ -f "$OLD_BLACKLIST_FILE" ]; then
		rm -f "$OLD_BLACKLIST_FILE"
		echo "  removed stale blacklist $OLD_BLACKLIST_FILE"
		migrated=1
	fi
	if dkms status 2>/dev/null | grep -q '^hid-logitech-hidpp' \
	   || [ -d "$dkms_base" ] \
	   || ls /usr/lib/modules/*/updates/dkms/hid-logitech-hidpp.ko* >/dev/null 2>&1; then
		# Best-effort clean removal (restores the original when the source
		# is still intact); tolerate an already-broken state.
		dkms remove -m hid-logitech-hidpp -v 1.0 --all >/dev/null 2>&1 || true
		# Restore any displaced in-tree module from DKMS's own backup.
		if [ -d "$dkms_base/original_module" ]; then
			local kdir k om dst
			for kdir in "$dkms_base"/original_module/*/; do
				[ -d "$kdir" ] || continue
				k=$(basename "$kdir")
				om=$(ls "$kdir"*/hid-logitech-hidpp.ko* 2>/dev/null | head -1)
				dst=/usr/lib/modules/$k/kernel/drivers/hid
				if [ -n "$om" ] && [ -d "$dst" ]; then
					cp -f "$om" "$dst/"
					echo "  restored in-tree hid-logitech-hidpp for $k"
				fi
			done
		fi
		# Drop the fork's installed module and DKMS state for good.
		rm -f /usr/lib/modules/*/updates/dkms/hid-logitech-hidpp.ko* 2>/dev/null || true
		rm -rf "$dkms_base" /usr/src/hid-logitech-hidpp-*
		echo "  removed old full-fork DKMS package hid-logitech-hidpp"
		migrated=1
	fi
	modprobe -r hid-logitech-hidpp 2>/dev/null || true
	if [ "$migrated" -eq 1 ]; then
		depmod -a
		if modprobe -n hid-logitech-hidpp >/dev/null 2>&1; then
			echo "  in-tree hid-logitech-hidpp restored for your other Logitech devices"
		else
			wrn "in-tree hid-logitech-hidpp missing after migration - reinstall your kernel package (e.g. sudo pacman -S linux) to restore it for non-wheel Logitech devices"
		fi
	else
		echo "  nothing to migrate (clean install)"
	fi

	say "[3/5] Loading the module"
	modprobe -r hid-logitech-dd 2>/dev/null || true
	if modprobe hid-logitech-dd; then
		echo "  loaded"
	else
		echo "  modprobe failed - check dmesg" >&2
	fi
	# claim the wheel if it is currently sitting on hid-generic
	"$REPO_ROOT/tools/rebind-wheel.sh" >/dev/null 2>&1 || true

	say "[4/5] TrueForce shim (Steam prefixes)"
	if ls "$REPO_ROOT"/sdk/Logi/Trueforce/*/trueforce_sdk_x64.dll >/dev/null 2>&1; then
		do_shim || true
	else
		echo "  SDK DLLs not staged - skipped (standard FFB works without them;"
		echo "  see the wiki's Force-feedback-in-games page for TrueForce)"
	fi

	say "[5/5] Doctor"
	# diagnosis runs best as the real user (permission checks)
	if [ -n "${SUDO_USER:-}" ]; then
		runuser -u "$SUDO_USER" -- "$REPO_ROOT/tools/setup.sh" doctor || true
	else
		doctor || true
	fi

	echo
	say "Remaining manual steps (per game, in Steam):"
	echo "  1. Properties > Launch Options:  PROTON_ENABLE_HIDRAW=1 %command%"
	echo "  2. Properties > Controller:     Disable Steam Input"
	echo "  (both needed for TrueForce; see the wiki's Force-feedback-in-games page)"
}

case "${1:-setup}" in
	doctor) doctor ;;
	shim)   do_shim ;;
	setup)  setup ;;
	*) echo "usage: sudo $0 [setup] | $0 doctor | $0 shim" >&2; exit 2 ;;
esac
