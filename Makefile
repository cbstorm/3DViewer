# Build and package 3D Viewer for Linux, Windows and macOS.
# Run `make help` for the list of targets.

APP     := viewer3d
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
CARGO   ?= cargo
DIST    := dist

ifeq ($(OS),Windows_NT)
  HOST := windows
else ifeq ($(shell uname -s),Darwin)
  HOST := macos
else
  HOST := linux
endif

LINUX_TARGET := x86_64-unknown-linux-gnu
MACOS_TARGETS := aarch64-apple-darwin x86_64-apple-darwin
# Native MSVC build on Windows; MinGW cross-build everywhere else.
ifeq ($(HOST),windows)
  WINDOWS_TARGET := x86_64-pc-windows-msvc
else
  WINDOWS_TARGET := x86_64-pc-windows-gnu
  MINGW_LINKER   ?= x86_64-w64-mingw32-gcc
endif

# macOS: oldest supported release, and where to find MoltenVK for the bundle
# (Vulkan SDK first, then Homebrew's molten-vk).
export MACOSX_DEPLOYMENT_TARGET ?= 11.0
MOLTENVK ?= $(firstword $(wildcard \
	$(VULKAN_SDK)/lib/libMoltenVK.dylib \
	$(shell brew --prefix molten-vk 2>/dev/null)/lib/libMoltenVK.dylib))

DOCKER_IMAGE := $(APP)-windows-build

.PHONY: help build run test lint check-targets linux deb windows windows-docker macos dist clean

help:
	@echo "3D Viewer $(VERSION) - host: $(HOST)"
	@echo
	@echo "Development:"
	@echo "  make build           debug build for this machine"
	@echo "  make run [MODEL=f]   run a release build, optionally opening a model"
	@echo "  make test            unit tests"
	@echo "  make lint            rustfmt check + clippy"
	@echo "  make check-targets   type-check for Linux, Windows and macOS (no linking)"
	@echo
	@echo "Release packages (written to $(DIST)/):"
	@echo "  make linux           $(LINUX_TARGET) tarball            (Linux host)"
	@echo "  make deb             Debian/Ubuntu .deb with desktop entry and icon (Linux host)"
	@echo "  make windows         $(WINDOWS_TARGET) zip"
	@echo "                       (Windows host, or MinGW: apt install mingw-w64)"
	@echo "  make windows-docker  Windows zip built in Docker, no MinGW install needed"
	@echo "  make macos           universal .app bundle with MoltenVK  (macOS host)"
	@echo "  make dist            every package this host can build"
	@echo "  make clean           remove build output and packages"

build:
	$(CARGO) build

run:
	$(CARGO) run --release -- $(MODEL)

test:
	$(CARGO) test

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings

check-targets:
	rustup target add $(LINUX_TARGET) $(WINDOWS_TARGET) $(MACOS_TARGETS)
	$(CARGO) check --release --target $(LINUX_TARGET)
	$(CARGO) check --release --target $(WINDOWS_TARGET)
	$(foreach t,$(MACOS_TARGETS),$(CARGO) check --release --target $(t) &&) true

# ---------------------------------------------------------------- Linux

LINUX_PKG := $(APP)-$(VERSION)-linux-x86_64
LINUX_BIN := target/$(LINUX_TARGET)/release/$(APP)

.PHONY: linux-release
linux-release:
ifneq ($(HOST),linux)
	$(error Linux packages must be built on Linux)
endif
	rustup target add $(LINUX_TARGET)
	$(CARGO) build --release --locked --target $(LINUX_TARGET)

linux: linux-release
	rm -rf $(DIST)/$(LINUX_PKG)
	mkdir -p $(DIST)/$(LINUX_PKG)
	cp $(LINUX_BIN) README.md $(DIST)/$(LINUX_PKG)/
	tar -C $(DIST) -czf $(DIST)/$(LINUX_PKG).tar.gz $(LINUX_PKG)
	@echo "Built $(DIST)/$(LINUX_PKG).tar.gz"

# Debian/Ubuntu package: binary, desktop entry and hicolor icon. The libc6
# minimum comes from the newest GLIBC symbol version the binary uses.
DEB_ARCH   := amd64
DEB_PKG    := $(APP)_$(VERSION)_$(DEB_ARCH)
DEB_ROOT   := $(DIST)/$(DEB_PKG)
MAINTAINER ?= $(shell git config user.name) <$(shell git config user.email)>

deb: linux-release
	rm -rf $(DEB_ROOT) $(DEB_ROOT).deb
	install -Dm755 $(LINUX_BIN) $(DEB_ROOT)/usr/bin/$(APP)
	install -Dm644 packaging/linux/$(APP).desktop $(DEB_ROOT)/usr/share/applications/$(APP).desktop
	install -Dm644 packaging/linux/$(APP).svg $(DEB_ROOT)/usr/share/icons/hicolor/scalable/apps/$(APP).svg
	install -Dm644 README.md $(DEB_ROOT)/usr/share/doc/$(APP)/README.md
	mkdir -p $(DEB_ROOT)/DEBIAN
	glibc=$$(objdump -T $(LINUX_BIN) | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -n 1); \
	size=$$(du -sk $(DEB_ROOT)/usr | cut -f1); \
	sed -e 's/@VERSION@/$(VERSION)/' -e 's/@ARCH@/$(DEB_ARCH)/' \
		-e 's/@MAINTAINER@/$(MAINTAINER)/' -e "s/@SIZE@/$$size/" -e "s/@GLIBC@/$$glibc/" \
		packaging/linux/control > $(DEB_ROOT)/DEBIAN/control
	dpkg-deb --root-owner-group --build $(DEB_ROOT) $(DEB_ROOT).deb
	@echo "Built $(DEB_ROOT).deb"

# ---------------------------------------------------------------- Windows

WINDOWS_PKG := $(APP)-$(VERSION)-windows-x86_64
WINDOWS_EXE  = target/$(WINDOWS_TARGET)/release/$(APP).exe

windows:
ifneq ($(HOST),windows)
	@command -v $(MINGW_LINKER) >/dev/null || { \
		echo "error: $(MINGW_LINKER) not found."; \
		echo "Install MinGW-w64 (Debian/Ubuntu: sudo apt install mingw-w64," \
			"macOS: brew install mingw-w64) or use 'make windows-docker'."; \
		exit 1; }
endif
	rustup target add $(WINDOWS_TARGET)
	CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$(MINGW_LINKER) \
		$(CARGO) build --release --locked --target $(WINDOWS_TARGET)
	$(MAKE) --no-print-directory package-windows WINDOWS_EXE=$(WINDOWS_EXE)

# Builds in a container as the current user, so the output isn't root-owned.
# A separate target dir keeps container and host artifacts apart.
windows-docker:
	docker build -t $(DOCKER_IMAGE) packaging/windows
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR)":/src -w /src \
		-e CARGO_HOME=/src/target/docker/cargo-home \
		-e CARGO_TARGET_DIR=/src/target/docker \
		$(DOCKER_IMAGE) \
		cargo build --release --locked --target x86_64-pc-windows-gnu
	$(MAKE) --no-print-directory package-windows \
		WINDOWS_EXE=target/docker/x86_64-pc-windows-gnu/release/$(APP).exe

.PHONY: package-windows
package-windows:
	rm -rf $(DIST)/$(WINDOWS_PKG) $(DIST)/$(WINDOWS_PKG).zip
	mkdir -p $(DIST)/$(WINDOWS_PKG)
	cp $(WINDOWS_EXE) README.md $(DIST)/$(WINDOWS_PKG)/
ifeq ($(HOST),windows)
	cd $(DIST) && tar -a -cf $(WINDOWS_PKG).zip $(WINDOWS_PKG)
else
	cd $(DIST) && zip -qr $(WINDOWS_PKG).zip $(WINDOWS_PKG)
endif
	@echo "Built $(DIST)/$(WINDOWS_PKG).zip"

# ---------------------------------------------------------------- macOS

MACOS_PKG := $(APP)-$(VERSION)-macos-universal
MACOS_APP := $(DIST)/3D Viewer.app

macos:
ifneq ($(HOST),macos)
	$(error macOS packages must be built on macOS: winit links Apple frameworks that need the macOS SDK)
endif
	rustup target add $(MACOS_TARGETS)
	$(foreach t,$(MACOS_TARGETS),$(CARGO) build --release --locked --target $(t) &&) true
	rm -rf "$(MACOS_APP)" $(DIST)/$(MACOS_PKG).zip
	mkdir -p "$(MACOS_APP)/Contents/MacOS" "$(MACOS_APP)/Contents/Frameworks"
	lipo -create -output "$(MACOS_APP)/Contents/MacOS/$(APP)" \
		$(foreach t,$(MACOS_TARGETS),target/$(t)/release/$(APP))
	sed 's/@VERSION@/$(VERSION)/g' packaging/macos/Info.plist > "$(MACOS_APP)/Contents/Info.plist"
ifneq ($(MOLTENVK),)
	cp "$(MOLTENVK)" "$(MACOS_APP)/Contents/Frameworks/"
else
	@echo "warning: libMoltenVK.dylib not found (set VULKAN_SDK or MOLTENVK=/path/to/libMoltenVK.dylib);"
	@echo "         the app will need a system-wide Vulkan installation."
endif
	codesign --force --deep --sign - "$(MACOS_APP)"
	cd $(DIST) && ditto -c -k --keepParent "3D Viewer.app" $(MACOS_PKG).zip
	@echo "Built $(MACOS_APP) and $(DIST)/$(MACOS_PKG).zip"

# ---------------------------------------------------------------- all

ifeq ($(HOST),linux)
dist: linux deb windows
else ifeq ($(HOST),macos)
dist: macos windows
else
dist: windows
endif

clean:
	$(CARGO) clean
	rm -rf $(DIST)
