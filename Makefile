PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
BASHRCD ?= $(HOME)/.bashrc.d
CARGO  ?= cargo
BIN    := target/release/try

.PHONY: all build install uninstall clean

all: build

build:
	$(CARGO) build --release --locked

$(BIN): build

install: $(BIN)
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/try
	install -Dm644 bashrc.d/try.sh $(DESTDIR)$(BASHRCD)/try.sh

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/try $(DESTDIR)$(BASHRCD)/try.sh

clean:
	$(CARGO) clean
