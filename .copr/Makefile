# COPR "Make SRPM" build entrypoint.
#
# COPR (Settings -> Build method: "make srpm") clones this repo and runs:
#     make -f .copr/Makefile srpm outdir=<dir>
# We build the source tarball from the checkout and emit an *akmod* SRPM
# (the spec builds akmod-only when `kernels` is undefined). COPR then rebuilds
# that SRPM per chroot into akmod-logitech-trueforce, which rebuilds the module
# on each user's kernel via the akmods service. No per-kernel build on COPR.

SPEC   := packaging/akmods/logitech-trueforce-kmod.spec
# The git tag first, the spec's literal only as a fallback. The spec is
# stamped by publish-release.yml at release time and otherwise carries
# whatever it was last committed with, which was 0.16.0 while HEAD was
# 0.27.x: a locally built SRPM came out labelled older than what users
# already had, so dnf would not upgrade to it.
VER    := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
ifeq ($(VER),)
VER    := $(shell sed -n 's/^%global[[:space:]]*upstream_ver[[:space:]]*//p' $(SPEC))
endif
PREFIX := logitech-trueforce-linux-driver-$(VER)
SRCDIR := $(HOME)/rpmbuild/SOURCES

srpm:
	dnf -y install rpm-build kmodtool kernel-rpm-macros git >/dev/null
	mkdir -p $(SRCDIR)
	git archive --prefix=$(PREFIX)/ -o $(SRCDIR)/logitech-trueforce-kmod-$(VER).tar.gz HEAD
	rpmbuild -bs $(SPEC) \
		--define "_sourcedir $(SRCDIR)" \
		--define "_srcrpmdir $(outdir)"
