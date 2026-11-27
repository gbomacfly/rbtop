PREFIX ?= /usr/local

build:
	cargo build --release

# portable: works with GNU install (Linux) and BSD install (macOS, no -D there)
install: build
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m755 target/release/rbtop $(DESTDIR)$(PREFIX)/bin/rbtop
	install -d $(DESTDIR)$(PREFIX)/share/rbtop/themes
	install -m644 themes/*.theme $(DESTDIR)$(PREFIX)/share/rbtop/themes/

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/rbtop
	rm -rf $(DESTDIR)$(PREFIX)/share/rbtop

.PHONY: build install uninstall
