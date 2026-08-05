# Game setup, per game and per wheel

**Generated file. Do not edit.** It is rendered from the
compatibility registry in
`userspace/logi-wheel/crates/logi-wheel-core/src/games.rs` by
`tests/game_setup_doc.rs`, which fails if this file drifts from it.
The settings app resolves your own installed games against that same
registry, so what you read here is what the app will tell you.

What a game needs depends on the wheel as well as the game. The
direct-drive wheels answer Logitech's TrueForce SDK, so a sim with
built-in TrueForce can reach them through the staged SDK DLLs. The
G923 does not: its force feedback is the older classic protocol,
and `PROTON_ENABLE_HIDRAW=1` on that wheel diverts the game to raw
HID reports it cannot drive feedback through, costing you the force
feedback you already had. That is why the columns differ.

Launch options go in Steam under the game's Properties. Paste them
exactly, `%command%` included: it is the placeholder Steam replaces
with the game itself, so without it the line replaces the game
instead of wrapping it.

## Recipes

| Game | Runs on Linux | Force feedback | On RS50 / G PRO | On G923 |
|---|---|---|---|---|
| American Truck Simulator | Native Linux | Native FFB | Nothing to do | Nothing to do |
| Assetto Corsa (original) | Proton | Native FFB | Nothing to do | Nothing to do |
| Assetto Corsa Competizione | Proton | TrueForce shim | Install the shim<br>`PROTON_ENABLE_HIDRAW=1 %command%` | Nothing to do<br>no TrueForce on this wheel; leave `PROTON_ENABLE_HIDRAW` unset |
| Assetto Corsa EVO (early access) | Proton | TrueForce shim | Install the shim<br>`PROTON_ENABLE_HIDRAW=1 %command%` | Nothing to do<br>no TrueForce on this wheel; leave `PROTON_ENABLE_HIDRAW` unset |
| Assetto Corsa Rally (early access) * | Proton | Native FFB | Nothing to do | Nothing to do |
| Automobilista 2 | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| BeamNG.drive * | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| CarX Drift Racing Online | Proton | Native FFB | Nothing to do | Nothing to do |
| Dakar Desert Rally * | Proton | Native FFB | Nothing to do | Nothing to do |
| DiRT 4 | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| DiRT Rally 2.0 | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| EA Sports F1 (F1 22-25) * | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| EA Sports WRC | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| Euro Truck Simulator 2 | Native Linux | Native FFB | Nothing to do | Nothing to do |
| Forza Horizon 5 | Not on Linux | Not on Linux | - | - |
| Forza Motorsport (2023) | Not on Linux | Not on Linux | - | - |
| Gran Turismo 7 | Not on Linux | Not on Linux | - | - |
| GRID (2019) | Proton | Native FFB | Nothing to do | Nothing to do |
| GRID Legends | Proton | Native FFB | Nothing to do | Nothing to do |
| iRacing | Proton | logi-ffb | Launch via logi-ffb<br>`logi-ffb %command%` | Launch via logi-ffb<br>`logi-ffb %command%` |
| KartKraft * | Proton | Native FFB | Nothing to do | Nothing to do |
| Le Mans Ultimate | Proton | logi-ffb | Launch via logi-ffb<br>`logi-ffb %command%` | Launch via logi-ffb<br>`logi-ffb %command%` |
| Project CARS 2 | Proton | Native FFB | Turn on simulated TrueForce | Turn on simulated TrueForce |
| RaceRoom Racing Experience | Proton | logi-ffb | Launch via logi-ffb<br>`logi-ffb %command%` | Launch via logi-ffb<br>`logi-ffb %command%` |
| Rennsport * | Proton | Native FFB | Nothing to do | Nothing to do |
| rFactor 2 | Proton | logi-ffb | Launch via logi-ffb<br>`logi-ffb %command%` | Launch via logi-ffb<br>`logi-ffb %command%` |
| Richard Burns Rally * | Proton | Native FFB | Nothing to do | Nothing to do |
| Wreckfest | Proton | Native FFB | Nothing to do | Nothing to do |

Rows marked `*` are not confirmed on this driver yet: expected or
documented rather than tested end to end.

## Simulated TrueForce

Games with no TrueForce of their own can still have engine haptics
and rev lights, synthesized by `logi-tf-sim` from the game's own UDP
telemetry. This works on every supported wheel, including the G923:
it is ordinary force feedback driven from telemetry, not the SDK.
Turn the game's telemetry output on in its own settings, then enable
the game in the app's Setup page.

| Game | Simulated TrueForce |
|---|---|
| American Truck Simulator | possible, needs a telemetry parser first |
| Assetto Corsa (original) | no usable telemetry |
| Assetto Corsa Competizione | not needed: the game has real TrueForce |
| Assetto Corsa EVO (early access) | not needed: the game has real TrueForce |
| Assetto Corsa Rally (early access) | no usable telemetry |
| Automobilista 2 | supported today |
| BeamNG.drive | supported today |
| CarX Drift Racing Online | no usable telemetry |
| Dakar Desert Rally | no usable telemetry |
| DiRT 4 | supported today |
| DiRT Rally 2.0 | supported today |
| EA Sports F1 (F1 22-25) | supported today |
| EA Sports WRC | supported today |
| Euro Truck Simulator 2 | possible, needs a telemetry parser first |
| GRID (2019) | possible, needs a telemetry parser first |
| GRID Legends | possible, needs a telemetry parser first |
| iRacing | possible, needs a telemetry parser first |
| KartKraft | possible, needs a telemetry parser first |
| Le Mans Ultimate | possible, needs a telemetry parser first |
| Project CARS 2 | supported today |
| RaceRoom Racing Experience | possible, needs a telemetry parser first |
| Rennsport | no usable telemetry |
| rFactor 2 | possible, needs a telemetry parser first |
| Richard Burns Rally | possible, needs a telemetry parser first |
| Wreckfest | no usable telemetry |

## What each recipe means

- **Install the shim.** Stage Logitech's signed SDK DLLs into the game's Proton prefix, from the app's Setup page or `tools/install-tf-shim.sh`. Install the TrueForce shim; set PROTON_ENABLE_HIDRAW=1; turn Steam Input off.
- **On a wheel with no SDK TrueForce.** Force feedback works as it is. SDK TrueForce is not available on this wheel, so skip the shim and leave PROTON_ENABLE_HIDRAW unset; setting it costs you force feedback. Turn Steam Input off.
- **Launch via logi-ffb.** Set PROTON_ENABLE_HIDRAW=0, or launch with logi-ffb %command%; Steam Input off.
- **Nothing to do.** The wheel is an ordinary Linux force feedback device and the game drives it directly.

## Confidence

- **verified** (3 titles): confirmed end to end by this project
- **documented** (18 titles): documented by the vendor or a reliable community source
- **expected** (3 titles): expected to work, not confirmed
- **unknown** (4 titles): genuinely unknown
