# 5thPlanet homebrew test disc — libyaul build.
#
# Requires the libyaul toolchain (SH-2 GCC) with $YAUL_INSTALL_ROOT set; see
# README.md for install options (Docker is easiest). `make` produces a
# self-booting CD image under build/ whose IP.BIN carries the SEGA SEGASATURN
# header. The default output build/saturn-tests.cue is what the headless
# harness (crates/saturn/tests/homebrew_disc.rs) loads.
#
# This Makefile follows the current libyaul layout (share/build.pre.mk +
# build.post.mk). If your installed yaul differs, mirror a libyaul example/
# Makefile or StrikerX3/saturn-tests; the C in src/ is SDK-agnostic.

ifeq ($(strip $(YAUL_INSTALL_ROOT)),)
  $(error YAUL_INSTALL_ROOT is not set — install libyaul and point it at the install root. See README.md)
endif

# libyaul preamble (compiler/flags/rules).
include $(YAUL_INSTALL_ROOT)/share/build.pre.mk

# --- project ---------------------------------------------------------------
SH_PROGRAM := saturn-tests
SH_SRCS    := src/main.c

# Self-boot CD image metadata (homebrew IP.BIN). Royalty-free strings only.
IP_VERSION      := V1.000
IP_RELEASE_DATE := 20260621
IP_AREAS        := JTUBKAEL
IP_PERIPHERALS  := JAMKST
IP_TITLE        := 5thPlanet Test Disc
IP_MASTER_STACK_ADDR := 0x06004000
IP_SLAVE_STACK_ADDR  := 0x06001000
IP_1ST_READ_ADDR     := 0x06004000

# Build artifacts land in build/.
BUILD_ROOT := build
BUILD      := build

# libyaul postamble: pulls in the $(SH_PROGRAM).{elf,bin}, the IP.BIN, and the
# CD-image (.iso/.cue) targets.
include $(YAUL_INSTALL_ROOT)/share/build.post.mk

.PHONY: all
all: $(SH_PROGRAM).cue

# Convenience: list what a successful build drops.
.PHONY: artifacts
artifacts:
	@ls -l $(BUILD)/ 2>/dev/null || echo "nothing built yet — run make"
