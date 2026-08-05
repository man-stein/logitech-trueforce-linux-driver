# TrueForce SDK: verified ABI notes

What the shipped `trueforce_sdk_x64.dll` actually expects, taken from its own
machine code rather than from any header. Written after three signatures in
`userspace/libtrueforce/include/trueforce.h` turned out to be wrong, one of
which made `tools/tf-range-proxy.c` incapable of working and cost a tester
several rounds of testing (issue #27).

**Read this before adding to `libtrueforce` or the proxy.** The header in
that tree is not authoritative and never was.

## How to check a signature

Find the RVA in `sdk/trueforce_1_3_11/exports_x64.txt`, then:

```bash
D=sdk/trueforce_1_3_11/trueforce_sdk_x64.dll
A=$(python3 -c "print(hex(0x180000000 + 0x18620))")   # image base + RVA
x86_64-w64-mingw32-objdump -d "$D" --start-address="$A" \
  --stop-address="$(python3 -c "print(hex(int('$A',16)+0x40))")"
```

Windows x64 passes integer and pointer arguments in RCX, RDX, R8, R9, and
floating point in XMM0-3 by position. So:

- a `test %rcx,%rcx` followed by a `mov $0x80000001,%eax` return means the
  first argument is a pointer that must not be null
- a `movsd %xmm1,...` means the second argument is a double
- a write through a saved register (`mov %eax,(%rbx)`) means an out parameter,
  and the width of that write is the type

`0x80000001` is the library's own bad-parameter status. Success is 0.

Calling one of these cold to see what it does will not work: they dereference
internal state that only exists after the SDK has been opened, and crash on a
null context. Verified by trying it.

## Verified

| Function | Signature | How |
|---|---|---|
| `logiWheelGetOperatingRangeDegrees` | `int f(int index, double *out)` | RCX index, RDX null-checked out pointer, `0x80000001` when null |
| `logiWheelGetOperatingRangeRadians` | `int f(int index, double *out)` | same shape |
| `logiWheelGetOperatingRangeBoundsDegrees` | `int f(int index, double *lo, double *hi)` | RCX, RDX, R8; RDX null-checked |
| `logiTrueForceSetGainTF` | `int f(int index, double gain)` | `movsd %xmm1` = second argument is a double |
| `logiTrueForceAvailable` | first argument is a **pointer**, not an index | `test %rcx,%rcx` then `0x80000001` |

The legacy Steering Wheel SDK is a different library with different
conventions, and its equivalent writes an `int`, not a `double`:

| Function | Signature |
|---|---|
| `LogiGetOperatingRange` (`logi_steering_wheel_x64.dll`) | `bool f(int index, int *range)` |

## Not established

- What `GetOperatingRangeBounds` means. The proxy answers `90..2700`, the
  wheel's minimum and maximum settable range, which matches the protocol
  documentation and the library's own `ANGULAR_RANGE_MIN`/`MAX` strings.
  `libtrueforce` instead answered `-range/2 .. +range/2`. Both are
  self-consistent and only one can be right; it cannot be resolved by
  disassembly alone because the function needs an initialised session.
- Every signature in `libtrueforce` not listed above. Three of the three
  checked were wrong, so the rest should be assumed unverified rather than
  assumed correct.

## Which SDK a game uses

Grep the game binary. Assetto Corsa Competizione resolves 56 symbols from
the TrueForce SDK, including all four rotation getters, and none at all from
the legacy Steering Wheel SDK:

```bash
strings -n 6 AC2-Win64-Shipping.exe | grep -E "^(logi|Logi|dll)[A-Za-z]" | sort -u
```

It also looks up four symbols the library has never exported (`dllVersion`
and three viscosity calls). Those lookups fail on Windows too, so a proxy
should match the real library exactly rather than inventing them.
