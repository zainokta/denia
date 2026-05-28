SHELL := /usr/bin/env bash

DENIA_BIN := /usr/local/bin/denia
RELEASE_BIN := target/release/denia

.PHONY: all build web rust install clean uninstall help

all: build

help:
	@echo "Targets:"
	@echo "  build       Build web SPA then cargo build --release --locked"
	@echo "  web         Build web/dist/client only"
	@echo "  rust        Run cargo build --release --locked only"
	@echo "  install     build + copy binary to $(DENIA_BIN) (requires root)"
	@echo "  clean       cargo clean + remove web/dist + web/node_modules"
	@echo "  uninstall   rm -f $(DENIA_BIN) (requires root)"

build: web rust

web:
	cd web && pnpm install --frozen-lockfile && pnpm build

rust:
	cargo build --release --locked

install: build
	install -Dm0755 $(RELEASE_BIN) $(DENIA_BIN)

clean:
	cargo clean
	rm -rf web/dist web/node_modules

uninstall:
	rm -f $(DENIA_BIN)
