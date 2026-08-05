# Windows-side helpers, built with a mingw cross compiler.
#
# These are not part of the kernel module or the Rust workspace; they are the
# pieces that have to run inside a Wine/Proton prefix alongside the game.
#
#   tf-range-proxy.dll  answers the SDK's rotation question from sysfs and
#                       forwards everything else to Logitech's own library
#   tf-pipe-probe.exe   listens on the pipe the SDK looks for, for diagnosis
#
# Requires: mingw-w64-gcc (Arch: pacman -S mingw-w64-gcc)

CC_WIN := x86_64-w64-mingw32-gcc
CFLAGS_WIN := -O2 -Wall

all: tf-range-proxy.dll tf-pipe-probe.exe

tf-range-proxy.dll: tf-range-proxy.c tf-range-proxy.def
	$(CC_WIN) $(CFLAGS_WIN) -shared -o $@ tf-range-proxy.c tf-range-proxy.def

tf-pipe-probe.exe: tf-pipe-probe.c
	$(CC_WIN) $(CFLAGS_WIN) -o $@ $<

clean:
	rm -f tf-range-proxy.dll tf-pipe-probe.exe

.PHONY: all clean
