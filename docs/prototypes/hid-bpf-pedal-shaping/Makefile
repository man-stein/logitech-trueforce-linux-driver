CLANG ?= clang
BPFTOOL ?= bpftool
CC ?= gcc
ARCH := $(shell uname -m | sed 's/x86_64/x86/')

all: pedals_poc.bpf.o pedals.bpf.o load_poc load_pedals

pedals.bpf.o: pedals.bpf.c pedal_shaping.h vmlinux.h

vmlinux.h:
	$(BPFTOOL) btf dump file /sys/kernel/btf/vmlinux format c > $@

%.bpf.o: %.bpf.c vmlinux.h
	$(CLANG) -g -O2 -target bpf -D__TARGET_ARCH_$(ARCH) -I. -c $< -o $@

%.skel.h: %.bpf.o
	$(BPFTOOL) gen skeleton $< > $@

load_poc: load_poc.c pedals_poc.skel.h
	$(CC) -O2 load_poc.c -o $@ -lbpf

load_pedals: load_pedals.c pedals.skel.h
	$(CC) -O2 load_pedals.c -o $@ -lbpf

clean:
	rm -f *.bpf.o *.skel.h load_poc load_pedals
