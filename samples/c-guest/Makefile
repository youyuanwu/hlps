SHELL := /bin/sh
.DELETE_ON_ERROR:

CC := clang
CARGO ?= cargo
CARGO_BUILD_JOBS ?= 2
export CARGO_BUILD_JOBS
WORKSPACE := $(abspath ../..)
TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(abspath $(CARGO_TARGET_DIR)),$(WORKSPACE)/target)

SDK_URL := https://github.com/hyperlight-dev/hyperlight/releases/download/v0.19.0
LIB_SHA256 := f9925f4836aafcdd1aafdc618023d806a76f18e0eb74bfb9d11d985af26fd2d7
INCLUDE_SHA256 := dda27c8b731a2ad1b2952a48d138c83b7b8ea18801b1cf20e087570faf77b799
GUEST_CFLAGS := --target=x86_64-unknown-linux-none -U__linux__ \
	-fPIC -fno-stack-protector -fstack-clash-protection \
	-mstack-probe-size=4096 -mno-red-zone -nostdlibinc \
	-DHYPERLIGHT=1 -D__HYPERLIGHT__=1 -isystem sdk/include \
	-std=c11 -O2 -Wall -Wextra -Werror
GUEST_LDFLAGS := -fuse-ld=lld -nostdlib -pie -Wl,-e,entrypoint \
	-Wl,--no-dynamic-linker

.PHONY: all guest host run native run-native run-inprocess test test-vm test-native clean
all: guest host

sdk/library.tar.gz:
	mkdir -p sdk
	curl --fail --location --retry 3 "$(SDK_URL)/hyperlight-guest-c-api-linux.tar.gz" -o "$@.tmp"
	printf '%s  %s\n' "$(LIB_SHA256)" "$@.tmp" | sha256sum --check
	mv "$@.tmp" "$@"

sdk/include.tar.gz:
	mkdir -p sdk
	curl --fail --location --retry 3 "$(SDK_URL)/include.tar.gz" -o "$@.tmp"
	printf '%s  %s\n' "$(INCLUDE_SHA256)" "$@.tmp" | sha256sum --check
	mv "$@.tmp" "$@"

sdk/.ready: sdk/library.tar.gz sdk/include.tar.gz
	tar -xzf sdk/library.tar.gz -C sdk release/libhyperlight_guest_capi.a
	tar -xzf sdk/include.tar.gz -C sdk
	touch "$@"

build/guest: guest.c sdk/.ready
	mkdir -p build
	$(CC) $(GUEST_CFLAGS) $(CFLAGS) $(GUEST_LDFLAGS) -o "$@" guest.c \
		sdk/release/libhyperlight_guest_capi.a

guest: build/guest

host:
	$(CARGO) build --locked --manifest-path host/Cargo.toml

run: all
	"$(TARGET_DIR)/debug/hyperlight-c-guest-demo" "$(CURDIR)/build/guest"

native: build/native-include/.ready
	$(CARGO) build --locked --manifest-path native/Cargo.toml --bins

run-native: native
	"$(TARGET_DIR)/debug/hyperlight-c-guest-native-demo"

run-inprocess: native
	"$(TARGET_DIR)/debug/hyperlight-c-guest-inprocess-demo"

build/native-include/.ready: sdk/.ready
	mkdir -p build/native-include
	cp sdk/include/hyperlight_guest.h sdk/include/macro.h build/native-include/
	touch "$@"

build/guest-test: guest.c guest_test.c build/native-include/.ready
	mkdir -p build
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror \
		-I build/native-include guest.c guest_test.c -o "$@"

test: build/guest-test
	./build/guest-test

test-vm: guest
	$(CARGO) test --locked --manifest-path host/Cargo.toml -- --ignored

test-native: build/native-include/.ready
	$(CARGO) test --locked --manifest-path native/Cargo.toml

clean:
	rm -f build/guest build/guest-test
	$(CARGO) clean --manifest-path "$(WORKSPACE)/Cargo.toml"
