APP_NAME    := Lexime
BUNDLE_ID   := dev.sendsh.inputmethod.Lexime
BUILD_DIR   := build
APP_BUNDLE  := $(BUILD_DIR)/$(APP_NAME).app
CONTENTS    := $(APP_BUNDLE)/Contents
MACOS_DIR   := $(CONTENTS)/MacOS
RES_DIR     := $(CONTENTS)/Resources
INSTALL_DIR := $(HOME)/Library/Input Methods

SWIFT_FILES := $(wildcard Sources/*.swift)

ENGINE_DIR  := engine
ENGINE_LIB  := build/liblex_engine.a

MACOS_MIN   := 13.0

SWIFTC_FLAGS := -O -import-objc-header Sources/Bridging-Header.h -Xcc -I.
LINK_FLAGS   := -Lbuild -llex_engine

.PHONY: build install reload log clean icon

build: $(MACOS_DIR)/$(APP_NAME)

$(ENGINE_LIB): $(wildcard $(ENGINE_DIR)/src/*.rs) $(ENGINE_DIR)/Cargo.toml
	cd $(ENGINE_DIR) && cargo build --release --target x86_64-apple-darwin
	cd $(ENGINE_DIR) && cargo build --release --target aarch64-apple-darwin
	@mkdir -p build
	lipo -create \
	  $(ENGINE_DIR)/target/x86_64-apple-darwin/release/liblex_engine.a \
	  $(ENGINE_DIR)/target/aarch64-apple-darwin/release/liblex_engine.a \
	  -output $(ENGINE_LIB)

$(MACOS_DIR)/$(APP_NAME): $(SWIFT_FILES) $(ENGINE_LIB) Info.plist Resources/icon.tiff
	@mkdir -p $(MACOS_DIR) $(RES_DIR)
	swiftc $(SWIFTC_FLAGS) $(LINK_FLAGS) -target x86_64-apple-macosx$(MACOS_MIN) \
	  $(SWIFT_FILES) -o $(MACOS_DIR)/$(APP_NAME)-x86_64
	swiftc $(SWIFTC_FLAGS) $(LINK_FLAGS) -target arm64-apple-macosx$(MACOS_MIN) \
	  $(SWIFT_FILES) -o $(MACOS_DIR)/$(APP_NAME)-arm64
	lipo -create \
	  $(MACOS_DIR)/$(APP_NAME)-x86_64 \
	  $(MACOS_DIR)/$(APP_NAME)-arm64 \
	  -output $(MACOS_DIR)/$(APP_NAME)
	@rm $(MACOS_DIR)/$(APP_NAME)-x86_64 $(MACOS_DIR)/$(APP_NAME)-arm64
	cp Info.plist $(CONTENTS)/Info.plist
	cp Resources/icon.tiff $(RES_DIR)/icon.tiff
	# TODO: codesign with Lexime.entitlements for distribution builds
	@echo "Build complete: $(APP_BUNDLE)"

install: build
	@mkdir -p "$(INSTALL_DIR)"
	rm -rf "$(INSTALL_DIR)/$(APP_NAME).app"
	cp -R $(APP_BUNDLE) "$(INSTALL_DIR)/$(APP_NAME).app"
	@echo "Installed to $(INSTALL_DIR)/$(APP_NAME).app"

reload:
	pkill -x $(APP_NAME) || true
	@echo "Sent kill signal to $(APP_NAME) (macOS will auto-restart it)"

log:
	log stream --predicate 'process == "Lexime"' --style compact

icon:
	bash scripts/icon.sh

clean:
	rm -rf $(BUILD_DIR)
	cd $(ENGINE_DIR) && cargo clean
	@echo "Clean complete"
