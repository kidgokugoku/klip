set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

test:
    cargo test -p klip-core -p klip-cli

build-cli:
    cargo build --release -p klip-cli

install-cli: build-cli
    mkdir -p ~/.local/bin
    cp target/release/klip ~/.local/bin/klip

build-frontend:
    cd apps/desktop && bun install && bun run build

dev-desktop: build-frontend
    cargo run -p klip-desktop

build-desktop: build-frontend
    cd apps/desktop && bunx @tauri-apps/cli build

check-win: build-frontend
    cargo xwin check --target x86_64-pc-windows-msvc --workspace

build-win-cli:
    cargo xwin build --release --target x86_64-pc-windows-msvc -p klip-cli
