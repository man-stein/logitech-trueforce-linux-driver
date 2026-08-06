# Simulated TrueForce in Euro Truck Simulator 2 and American Truck Simulator

These two publish telemetry through their own plugin interface rather than
over UDP, so `logi-tf-sim` cannot see them the way it sees Automobilista 2 or
DiRT Rally. `logi-tf-scs` is a small plugin that runs inside the game and
forwards engine speed, throttle and gear to the daemon, which then drives the
wheel exactly as it does for the UDP titles.

It is a native Linux plugin. No Wine, no Proton, nothing to cross-compile.

## Install

Download `liblogi_tf_scs-<version>.so` from the
[latest release](https://github.com/mescon/logitech-trueforce-linux-driver/releases/latest)
and rename it to `liblogi_tf_scs.so`, or build it yourself:

```bash
cargo build --release -p logi-tf-scs
```

Then drop it into the game's plugin directory.

Euro Truck Simulator 2:

```bash
mkdir -p ~/.steam/steam/steamapps/common/"Euro Truck Simulator 2"/bin/linux_x64/plugins
cp userspace/logi-wheel/target/release/liblogi_tf_scs.so \
   ~/.steam/steam/steamapps/common/"Euro Truck Simulator 2"/bin/linux_x64/plugins/
```

American Truck Simulator:

```bash
mkdir -p ~/.steam/steam/steamapps/common/"American Truck Simulator"/bin/linux_x64/plugins
cp userspace/logi-wheel/target/release/liblogi_tf_scs.so \
   ~/.steam/steam/steamapps/common/"American Truck Simulator"/bin/linux_x64/plugins/
```

Create the `plugins` directory if it is not there; the game only looks for
it, it does not ship one. If your Steam library lives on another drive,
`./tools/setup.sh doctor` prints the library roots it found.

Then turn the game on in the app's Setup page, under Simulated TrueForce, the
same switch every other supported title uses. Euro Truck Simulator 2 and
American Truck Simulator have separate switches and separate intensities.

## Check it is working

The game logs its plugins at startup. With a session loaded:

```bash
grep -i "logi-tf-scs" ~/.local/share/"Euro Truck Simulator 2"/game.log.txt
```

A line mentioning `logi-tf-scs` means the game loaded it. If the wheel is
still quiet after that, the daemon is the next place to look:

```bash
logi-tf-sim --status
```

## What it sends, and what it does not

Engine rpm, the engine's redline, throttle position and selected gear. That
is what the engine-note synthesizer needs and nothing more.

It does not send position, speed, damage, cargo, navigation or anything else
the SCS telemetry interface exposes, and it does not read anything back from
the game. It opens one UDP socket to `127.0.0.1` and writes to it.

If `logi-tf-sim` is not running, the packets go nowhere and the game is
unaffected. Every callback is wrapped so that a fault in the plugin cannot
take the game down with it.

## Port

The daemon listens on UDP 20780 by default. If you changed `port.relay` in
`tf-sim.conf`, tell the plugin the same number:

```bash
LOGI_TF_SIM_RELAY_PORT=20999 steam steam://rungameid/227300
```

## Status

The plugin is written against the official SCS SDK 1.14 headers, with every
struct layout pinned by tests, but it has **not yet been confirmed in a
running game**. That is why the compatibility table marks these two titles
provisional. If you run either of them, a note on the issue tracker saying
whether the engine note appeared is the thing that moves them to verified.
