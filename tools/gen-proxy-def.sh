#!/bin/bash
# Regenerate tf-range-proxy.def from the real SDK's export table.
#
# Written because doing it by hand went wrong in a way that was invisible:
# a "logi[A-Za-z]+" pattern truncates every export ending in digits, so
# logiTrueForceSetTorqueTFint16/int32/int8 collapsed into one name that does
# not exist. The proxy then forwarded a symbol nobody exports and omitted
# three that games may well call. Nothing complains at build time.
set -eu
DLL="${1:-sdk/trueforce_1_3_11/trueforce_sdk_x64.dll}"
OUT="${2:-tools/tf-range-proxy.def}"
OVERRIDE="logiWheelGetOperatingRangeDegrees logiWheelGetOperatingRangeRadians
          logiWheelGetOperatingRangeBoundsDegrees logiWheelGetOperatingRangeBoundsRadians"
{
	echo "EXPORTS"
	echo "; Generated from the real DLL's Ordinal/Name Pointer table."
	echo "; All but the four rotation getters forward to Logitech's library."
	echo "; Regenerate with: tools/gen-proxy-def.sh"
	x86_64-w64-mingw32-objdump -p "$DLL" \
		| awk '/\[Ordinal\/Name Pointer\] Table/{f=1;next} f' \
		| grep -oE "logi[A-Za-z0-9]+" | sort -u \
		| while read -r fn; do
			if echo "$OVERRIDE" | grep -qw "$fn"; then echo "$fn"
			else echo "$fn=trueforce_real.$fn"; fi
		done
} > "$OUT"
echo "wrote $OUT ($(grep -c '=' "$OUT") forwarded, $(grep -vc '[=;]' "$OUT") implemented locally)"
