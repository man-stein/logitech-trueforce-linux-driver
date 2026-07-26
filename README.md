<img src="docs/images/logo.svg" align="right" width="120" alt="logi-dd logo"/>

# Logitech TrueForce Linux Driver

A Linux kernel driver and userspace tools for Logitech's direct-drive racing
wheels: the **RS50** and the **G PRO Racing Wheel**. It brings force feedback,
TrueForce haptics (native, and simulated from game telemetry for titles
without it), a live RPM rev-light display, LIGHTSYNC LED control, and
G HUB-equivalent wheel settings to Linux, including in Proton/Wine sims -
all managed from a desktop app (**logi-dd-gui**) or a terminal one
(**logi-dd**).

> Not a direct-drive wheel? The belt-driven **G920** is already served by the
> in-tree `hid-logitech-hidpp` driver and does not need this one. The **G923**
> (all editions) gets its own feature set from this driver instead - see
> [G923 support](#g923-support) below.

## What works

Force feedback, TrueForce haptics, LEDs, pedals, the RS Shifter & Handbrake, and
the full set of G HUB wheel settings all work. The RS50 is the development
hardware and is verified directly; the G PRO runs the same code path and is
expected to work, with a few items awaiting an owner's confirmation.

**Legend:** ✅ verified on hardware · 🟢 shares the verified code path, expected
to work · 🟡 needs a tester · `-` not applicable.

| Capability | RS50 | G PRO |
|---|:--:|:--:|
| Steering, pedals, buttons, D-pad | ✅ | 🟢 |
| Force feedback (full evdev effect suite) | ✅ | 🟢 |
| Force feedback in DirectInput sims (via `logi-ffb`) | 🟡 | 🟡 |
| TrueForce haptics (Proton + Logitech's signed SDK) | ✅ | 🟢 |
| Rotation range (90 to 2700°), strength, damping, filters | ✅ | 🟢 |
| Pedal response curves, sensitivity, deadzones, combined pedals | ✅ | 🟢 |
| RS Shifter & Handbrake (shift, digital + analog handbrake) | ✅ | 🟢 |
| LIGHTSYNC RGB LEDs (slots, colors, direction; edits apply live) | ✅ (faceplate strip) | 🟡 (rev lights) |
| RPM rev-light display (level fill, direction-aware) | ✅ | 🟡 |
| Simulated TrueForce from game telemetry (`logi-tf-sim`) | ✅ (sweep-verified) | 🟢 |
| Centre calibration, mode / profile switching, computer-side profiles | ✅ | 🟢 |

USB IDs covered: RS50 (`046d:c276` native, `046d:c272` compatibility mode),
G PRO Racing Wheel (`046d:c272` Xbox/PC, `046d:c268` PS/PC), and the G923
(`046d:c266`/`c267` PlayStation edition, `046d:c26d`/`c26e` Xbox edition -
see [G923 support](#g923-support)).

## What's included

Six pieces, all built from this repository:

- **The kernel driver** (`hid-logitech-dd`) is the core. It exposes force
  feedback through the standard Linux evdev interface and every wheel setting
  under `/sys/.../wheel_*`, and coexists with the in-tree Logitech driver
  everywhere else - no blanket module blacklisting. It also covers the G923
  (`c266`/`c267`/`c26e`) with a separate feature set; see
  [G923 support](#g923-support) below for how that differs.

- **logi-dd**, a terminal settings app: a native-Linux stand-in for the parts of
  G HUB that configure the wheel, with typed, validated edits and a G HUB-style
  curve editor. So you do not have to `echo` values into sysfs by hand.

- **logi-dd-gui**, the same settings surface as a desktop app (Slint): every
  wheel setting, a LIGHTSYNC editor with per-slot colors and animation
  direction (changes apply to the wheel immediately), per-game TrueForce shim
  and simulated-TrueForce management on a Setup page that finds your sims across
  Steam (Proton and native), Lutris and Heroic and lets you add one it does not
  recognise, computer-side profile presets, and an Info / Testing page with a
  live input tester (rotating wheel diagram, button and pedal readouts) and
  guarded, cancelable force simulations.

  ![logi-dd-gui settings](docs/images/logi-dd.png)

- **logi-ffb**, a DirectInput force-feedback proxy for Wine/Proton sims that lose
  force feedback on the `PROTON_ENABLE_HIDRAW=1` path (see below).

- **logi-tf-sim**, a background daemon that synthesizes TrueForce engine
  haptics from a game's own UDP telemetry, for titles with no native
  TrueForce - and feeds the same telemetry to the wheel's rev-light strip as
  a live RPM display. Auto-detects supported games (DiRT Rally 2.0 and the
  classic Codemasters format, Automobilista 2 / Project CARS 2, F1, BeamNG.drive
  and EA Sports WRC); enable and tune it per game from the Setup page.

- **libtrueforce**, a native-Linux C library reimplementing Logitech's TrueForce
  SDK, for apps that want to drive TrueForce without Wine (a telemetry-driven
  haptic generator, for example). Optional; not needed for the Proton recipe.

The distribution packages install the driver plus the `logi-dd`, `logi-dd-gui`,
`logi-ffb` and `logi-tf-sim` tools; `libtrueforce` has its own build under
`userspace/libtrueforce/`.

## G923 support

The **G923** gets a separate feature set from this driver, distinct from the
direct-drive wheels above: it is belt-driven and speaks a different classic
protocol, not the RS50/G PRO's endpoint-based one.

- **PlayStation edition** (`046d:c266`/`c267`): a classic force-feedback
  engine ported from berarma's [new-lg4ff](https://github.com/berarma/new-lg4ff)
  drives constant force, spring/damper/friction/inertia, periodic and ramp
  effects, and autocenter, with an automatic PlayStation-to-PC mode switch.
  Settings use the classic `range`/`gain`/`autocenter`/`combine_pedals` sysfs
  names (Oversteer-compatible, not the `wheel_*` names above, since it is a
  different FFB engine), plus a read-only `ffb_output`. Rev lights (5
  mirrored LED pairs) are exposed as standard Linux LED devices
  (`::RPM1` to `::RPM5` under `/sys/class/leds`). Hardware-verified:
  constant force and autocenter feel correct in Assetto Corsa Competizione,
  and the LED sweep.
- **No launch options needed for force feedback**: unlike the SDK-aware
  recipe below, the G923 needs no `PROTON_ENABLE_HIDRAW` - just turn off
  Steam Input.
- **TrueForce is simulated, not native**: Logitech's SDK path does not work
  for the PlayStation G923 on Linux (the SDK DLL just delegates the haptics
  to G HUB, which Proton does not provide). `logi-tf-sim` streams the same
  telemetry-driven haptics used on the other wheels to the G923 instead, over
  the wheel's TrueForce interface (which this driver exposes as a hidraw
  node), mirroring live force feedback into
  the same stream so the two agree. Hardware-confirmed as vibration; the feel
  check under real game telemetry is still pending.
- **Xbox edition** (`046d:c26e` PC mode): force feedback routes through the
  same HID++ 0x8123 path as the G920. It boots into a console-only mode
  (`046d:c26d`) with no input node at all; installing `usb_modeswitch` (a
  recommended, not required, package) lets a udev rule switch it into PC mode
  automatically on plug-in. If it never switches, the out-of-tree `xone`
  driver may have claimed the device first. Unverified pending an
  Xbox-edition tester.
- A PID-scoped udev rule pre-empts a competing driver that wins the initial
  bind race for these three PIDs only (unbind, then bind this driver); every
  other Logitech device - G29/G27/DFGT/G920, mice, keyboards, receivers - is
  untouched. The one exception is berarma's new-lg4ff (`hid-logitech-new`),
  blacklisted outright since it otherwise races us for `c266`/`c267`.
- `logi-dd` and `logi-dd-gui` recognise the G923 and expose its four classic
  settings, with its own wheel image on the Info/Testing page.

## Install

Pick your distribution. The full step-by-step is on the
[**Installation**](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Installation)
wiki page, and the one-time TrueForce SDK setup is on
[**Force feedback in games**](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Force-Feedback-in-Games).

| Distribution | Install |
|---|---|
| Arch, CachyOS, Manjaro | `paru -S logi-dd-gui` (AUR, or your AUR helper; pulls `logi-dd` and the driver. Headless box: `paru -S logi-dd`) |
| Debian, Ubuntu, Mint, Pop!_OS | download the `.deb`s from [Releases](https://github.com/mescon/logitech-trueforce-linux-driver/releases), then `sudo apt install ./logitech-trueforce-dkms_*.deb ./logi-dd_*.deb ./logi-dd-gui_*.deb` (skip the gui one on a headless box) |
| Fedora, Nobara | COPR akmod: `sudo dnf copr enable mescon/logitech-trueforce && sudo dnf install akmod-logitech-trueforce logi-dd-gui` (headless box: `logi-dd` instead of `logi-dd-gui`) |
| openSUSE | OBS repo `home:mescon` (see the [Installation](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Installation) page) |
| From source (any distro) | `git clone` this repo, then `sudo ./tools/setup.sh` (DKMS build, udev rule, everything). `./tools/setup.sh doctor` health-checks it. |

The AUR and Debian packages are DKMS-based and rebuild automatically on kernel
upgrades. After installing, plug in the wheel and check `dmesg` for a line naming
your wheel model. The wheel settings are writable with no extra setup once the udev rule is
installed (it needs no group membership).

## Force feedback in games

- **Native and most Proton sims:** force feedback works out of the box; games see
  a standard Linux wheel. No setup beyond binding controls in game.

- **TrueForce haptics** (the high-frequency texture layer, on top of normal FFB)
  in SDK-aware sims needs Logitech's signed SDK DLLs staged into the game's Proton
  prefix, plus `PROTON_ENABLE_HIDRAW=1`. The one-time recipe is on the
  [Force feedback in games](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Force-Feedback-in-Games)
  wiki page. Verified end to end on **Assetto Corsa Competizione** and
  **Assetto Corsa EVO**. This recipe is for the RS50 and G PRO only: on the
  G923 the SDK path does not work and `PROTON_ENABLE_HIDRAW` must stay
  unset - see [G923 support](#g923-support).

- **DirectInput sims** (Le Mans Ultimate, for example) lose force feedback with
  `PROTON_ENABLE_HIDRAW=1` because the real wheel advertises no PID collection.
  The fix is to prepend **`logi-ffb`** to the launch command (`logi-ffb
  %command%` in Steam launch options): it presents a virtual wheel that does
  carry a PID collection, sets `PROTON_ENABLE_HIDRAW=1` on the game itself so
  Wine drives that PID collection directly, and forwards the effects to the
  real wheel. You do not set the hidraw variable by hand. The virtual wheel
  appears as "logi-ffb Virtual Wheel" (its own name and IDs, not the real
  wheel's), so a game may need a one-time manual binding to it. `logi-ffb` is
  hardware-validated but wants an in-game tester; if you have such a sim,
  reports are very welcome.

- **Simulated TrueForce** for games without native support: enable the game
  in Setup's "Simulated TrueForce" panel, switch on the game's own UDP
  telemetry setting, and `logi-tf-sim` synthesizes engine haptics from live
  RPM and throttle - and drives the rev LEDs to match. Intensity and felt
  rev rate (pitch) are tunable; a consent-gated test sweep lets you feel it
  without a game. Hardware-verified with synthetic sweeps; in-game reports
  welcome.

## Configuring the wheel

Run **logi-dd-gui** (or **logi-dd** in a terminal) and edit settings live:
rotation range, force-feedback strength and filters, TrueForce level, LIGHTSYNC
LEDs, profiles, and per-pedal / steering response curves through a G HUB-style
curve editor.

![logi-dd-gui curve editor](docs/images/logi-dd-curve-editor.png)

The Info / Testing page doubles as a live input tester (does this button
reach the computer?), and the Setup page manages the game helpers:

![logi-dd-gui Info / Testing](docs/images/logi-dd-info-testing.png)

![logi-dd-gui Setup](docs/images/logi-dd-setup.png)

```bash
cd userspace/logi-dd && cargo build --release
./target/release/logi-dd-gui    # desktop app; ./target/release/logi-dd for the TUI
```

**logi-dd is the recommended way to configure these wheels** - it is built for
them specifically and covers everything the driver exposes. Everything it sets
is also available as plain sysfs attributes under
`/sys/class/hidraw/hidrawX/device/wheel_*`, so you can script them directly; the
complete reference is in [**docs/SYSFS_API.md**](docs/SYSFS_API.md). If you
already run [Oversteer](https://github.com/berarma/oversteer) across a
collection of Logitech wheels, the driver also exposes its expected attribute
names, so it recognizes the basics here too.

## Verified game support

**Assetto Corsa Competizione** and **Assetto Corsa EVO** are verified end to end
under Proton: steering, full force feedback, and TrueForce at once (with
`PROTON_ENABLE_HIDRAW=1` and Steam Input disabled). Most other sims either work
out of the box with standard force feedback or need the `logi-ffb` proxy for
their DirectInput feedback; the full per-game table, and which needs what, is on
the [Force feedback in games](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Force-Feedback-in-Games)
wiki page.

A couple of game-side behaviors (rotation-range reset at session start, and
keeping hands clear during AC EVO map loads) are covered under
[Troubleshooting](#troubleshooting) below.

## Troubleshooting

- **No force feedback / no `wheel_*` files (`range`/`gain` on a G923; wheel
  stuck on `hid-generic`):** the
  module did not bind. Confirm it is loaded (`lsmod | grep hid_logitech_dd`),
  replug the wheel, and check `dmesg`. `./tools/setup.sh doctor` diagnoses this.
- **Force feedback pulls the wrong way** (a native game and a Wine/Proton game can
  want opposite signs): toggle **Invert constant force** in logi-dd (the
  `wheel_ffb_constant_sign` attribute).
- **A game stops seeing the wheel after a driver reload:** restart Steam fully;
  its device list goes stale across reloads.
- **Rotation snaps to 90° at session start:** some sims reset it via their own SDK
  path; the driver restores your range automatically within 20 seconds. Re-apply
  the game's own steering-lock setting so it stops pushing 90°.

More cases, with commands, are on the
[Troubleshooting](https://github.com/mescon/logitech-trueforce-linux-driver/wiki/Troubleshooting)
wiki page.

## Documentation

The [**project wiki**](https://github.com/mescon/logitech-trueforce-linux-driver/wiki)
is the friendliest place to start: a **Users** section (install, force feedback
in games, configuring the wheel, simulated TrueForce, troubleshooting) and a
**Developers** section (architecture, the sysfs API, the protocol
specification, libtrueforce, and the internals of `logi-ffb` and the
simulated-TrueForce daemon).

One reference is worth pinning to your installed version, so it stays in the
repo: the exact `wheel_*` attribute list for scripting in
[**docs/SYSFS_API.md**](docs/SYSFS_API.md). The protocol and button-mapping
references live under [`docs/`](docs/) as well.

## Contributing

Contributions are welcome: code, testing on hardware this project cannot reach
(a real G PRO, a DirectInput sim with `logi-ffb`), and USB captures of wheel
variants that are not yet fully supported. This driver is forked from
[JacKeTUs/hid-logitech-hidpp](https://github.com/JacKeTUs/hid-logitech-hidpp);
changes that apply to other Logitech devices are worth contributing upstream too.
Open an issue with your kernel version, distribution, and relevant `dmesg` output.

## License

- **Kernel driver** (`mainline/`), tooling, and everything else: **GPL-2.0-only**
  (see [`COPYING`](COPYING)).
- **libtrueforce** (`userspace/libtrueforce/`): **LGPL-2.1-or-later**, so native
  Linux apps may link it while changes to the library itself stay open.

Logitech's TrueForce SDK DLLs are not part of this project and are not
redistributed here; you supply them from your own G HUB installation.

## Acknowledgments

- Based on [JacKeTUs/hid-logitech-hidpp](https://github.com/JacKeTUs/hid-logitech-hidpp),
  which adds G PRO wheel support and improved force feedback.
- Upstream Linux [hid-logitech-hidpp](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-logitech-hidpp.c)
  by Benjamin Tissoires and contributors.
- [Oversteer](https://github.com/berarma/oversteer) by Bernat Arlandis, prior art
  for Linux wheel configuration; this driver exposes Oversteer-compatible
  attribute names.
- [new-lg4ff](https://github.com/berarma/new-lg4ff), also by Bernat Arlandis:
  source of the classic force-feedback engine ported into this driver's G923
  PlayStation-edition support.
- [TF4ALL](https://github.com/Mhytee/Trueforce-For-All) by Mhytee, a Windows
  SimHub plugin whose protocol analysis (issue #20) confirmed the G923 shares
  the RS50/G PRO TrueForce stream protocol.
