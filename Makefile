# Build, check, install and package Agent Review.
#
#   make check                         format, lint, test and validate data files
#   make install PREFIX=~/.local       install for the current user (shows up in app search)
#   sudo make install                  install system-wide into /usr/local
#   make rpm / make deb                build a Fedora / Ubuntu package

APP_ID   := io.github.ralphnajm.AgentReview
BIN      := agent-review
PREFIX   ?= /usr/local
DESTDIR  ?=
CARGO    ?= cargo

BINDIR   := $(DESTDIR)$(PREFIX)/bin
DATADIR  := $(DESTDIR)$(PREFIX)/share

.PHONY: all build release run test fmt fmt-check lint validate check install uninstall rpm deb clean

all: release

build:
	$(CARGO) build

release:
	$(CARGO) build --release --locked

run:
	$(CARGO) run -- $(ARGS)

test:
	$(CARGO) test --locked

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

lint:
	$(CARGO) clippy --all-targets --locked -- -D warnings

validate:
	desktop-file-validate data/$(APP_ID).desktop
	appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

check: fmt-check lint test validate

install: release
	install -Dm755 target/release/$(BIN) $(BINDIR)/$(BIN)
	install -Dm644 data/$(APP_ID).desktop $(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg $(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	@if [ -z "$(DESTDIR)" ]; then \
		gtk-update-icon-cache -qtf $(DATADIR)/icons/hicolor 2>/dev/null || true; \
		update-desktop-database -q $(DATADIR)/applications 2>/dev/null || true; \
	fi

uninstall:
	rm -f $(BINDIR)/$(BIN)
	rm -f $(DATADIR)/applications/$(APP_ID).desktop
	rm -f $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	rm -f $(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg

# Requires `cargo install cargo-generate-rpm`.
rpm: release
	$(CARGO) generate-rpm

# Requires `cargo install cargo-deb`.
deb: release
	$(CARGO) deb --no-build

clean:
	$(CARGO) clean
