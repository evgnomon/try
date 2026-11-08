PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
BASHRCD ?= $(HOME)/.bashrc.d
CARGO  ?= cargo

# On Linux, build a static musl binary for the host architecture.
# Override with TARGET=<triple>, or TARGET= for the host's default target.
ifeq ($(shell uname -s),Linux)
TARGET ?= $(shell uname -m)-unknown-linux-musl
endif

ifneq ($(TARGET),)
TARGET_FLAG := --target $(TARGET)
BIN := target/$(TARGET)/release/try
else
BIN := target/release/try
endif

.PHONY: all build test install uninstall clean

all: build

build:
	$(CARGO) build --release --locked $(TARGET_FLAG)

test:
	$(CARGO) test --locked $(TARGET_FLAG)

$(BIN): build

install: $(BIN)
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/try
	install -Dm644 bashrc.d/try.sh $(DESTDIR)$(BASHRCD)/try.sh

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/try $(DESTDIR)$(BASHRCD)/try.sh

clean:
	$(CARGO) clean
