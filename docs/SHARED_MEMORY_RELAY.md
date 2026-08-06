# Simulated TrueForce for sims that publish to shared memory

Most sims broadcast telemetry over UDP, which `logi-tf-sim` reads directly.
A few do not: iRacing, RaceRoom, Assetto Corsa, rFactor 2 and Le Mans
Ultimate publish into a named Windows shared-memory section that only the
game's own SDK reads. Nothing on the Linux side can see it.

`logi-tf-relay` is a small Windows executable that runs inside the game's
Proton prefix, reads that section with the ordinary Win32 API (which Wine
implements), and forwards engine speed, redline, throttle and gear to the
daemon over localhost UDP.

## What works today

| Game | State |
|---|---|
| iRacing | Decoder written. Unconfirmed against a live session. |
| RaceRoom Racing Experience | Decoder written. Unconfirmed against a live session. |
| Assetto Corsa | Decoder written. Unconfirmed against a live session. |
| rFactor 2 | Needs a byte fixture first, see below. |
| Le Mans Ultimate | Needs a byte fixture first, see below. |

What separates these is not difficulty, it is whether the layout can be
trusted without a capture. Each of the first three earned that trust a
different way.

**iRacing's telemetry is self-describing.** The section starts with a small
header pointing at a table of variable descriptors, each carrying a
variable's name next to its offset and type, so the decoder looks up `RPM`,
`Throttle` and `Gear` by name at runtime. Nothing about where those values
live is guessed.

**RaceRoom's layout is published by the people who write it.** KW Studios
ship `r3e.h` at `github.com/sector3studios/r3e-api`, in the public domain,
and they own both ends of the interface with no plugin in between. The struct
is byte-packed, so no compiler can disagree about where a field sits, and it
opens with a major version number the decoder checks before reading anything
else.

**Assetto Corsa only needs the head of its struct.** Kunos have appended
fields to the physics block for a decade, but appending does not move what
came before, and throttle, gear and rpm have been in its first 32 bytes since
1.0. The redline comes from a second block where the offset is less obviously
safe, so the decoder verifies that block's layout in-band before trusting it.

**rFactor 2 and Le Mans Ultimate are the case the rule exists for.** Their
layout comes from a community plugin that has forks and versions, so the
struct the game publishes depends on which build the user installed. A wrong
offset yields numbers that look plausible and are wrong. So those two wait
for a real capture. That is a deliberate rule here, not an oversight.

None of the three written decoders has been confirmed against a running game
yet. Every read in them is bounds checked and range gated, and each drops a
sample it cannot vouch for rather than sending a wrong number, but a report
from someone who has actually run one is what moves it from "should work" to
"known to work".

## Build it

The relay is a Windows binary, so it needs the Windows target:

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --release -p logi-tf-relay --target x86_64-pc-windows-gnu
```

The result is `target/x86_64-pc-windows-gnu/release/logi-tf-relay.exe`.

If `cargo` says the target is unavailable, your distribution's Rust probably
cannot cross-compile; a rustup-managed toolchain can. The build needs no
Windows machine and no Wine.

## Run it

Run the relay in the same Proton prefix as the game, while the game is
running. The prefix is named after the game's Steam appid:

| Game | `--game` | Steam appid |
|---|---|---|
| iRacing | `iracing` | 266410 |
| RaceRoom Racing Experience | `raceroom` | 211500 |
| Assetto Corsa | `assetto` | 244210 |

```bash
WINEPREFIX=~/.steam/steam/steamapps/compatdata/244210/pfx \
  wine logi-tf-relay.exe --game assetto
```

Leave it running. It re-reads the section about 60 times a second and sends
what it finds to `logi-tf-sim`, which must also be running. Then turn the
game on in the app's Setup page under Simulated TrueForce.

None of these three needs anything switched on inside the game: unlike the
UDP titles, the shared memory is always published. If your Steam library is
on another drive, `./tools/setup.sh doctor` prints the roots it found.

If the daemon uses a non-default relay port, tell the relay too:

```bash
LOGI_TF_SIM_RELAY_PORT=20999 wine logi-tf-relay.exe --game iracing
```

## Capture a fixture (rFactor 2, Le Mans Ultimate)

This is the piece the project needs from testers, and it takes one run.

rFactor 2 and Le Mans Ultimate also need the community
`rF2SharedMemoryMapPlugin` in the game's `Plugins` directory; without it the
game publishes nothing at all.

With the game running and a session actually **live** (sitting in the menus
is not always enough):

```bash
WINEPREFIX=<the game's prefix> \
  wine logi-tf-relay.exe --game lmu --dump lmu-dump.bin
```

Attach `lmu-dump.bin` to an issue. That file is what the decoder gets written
and unit-tested against. Without it, any decoder would be a guess.

The dump contains vehicle telemetry for the session that was running. It
carries no account details, no keys and no personal data, but it does reflect
what you were driving at that moment.

## Troubleshooting

**"not readable yet"** repeated: the relay is running but the game is not
publishing. Check the game is actually in a session, and for rFactor 2 and Le
Mans Ultimate that the shared-memory plugin is installed.

**Nothing happens although the relay says it is streaming**: check
`logi-tf-sim` is running, that the game is switched on in the Setup page, and
that both agree on the port.

**The relay exits immediately on Linux**: that is the stub. It only does
anything inside a Wine prefix.
