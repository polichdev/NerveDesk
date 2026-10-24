# NerveDesk Developer Task Runner (justfile)
# Install 'just' via: cargo install just

# Default recipe: display available commands
default:
    @just --list

# Run all formatting and static checks
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Auto-format all Rust and Frontend code
fmt:
    cargo fmt --all

# Run local development GUI
dev:
    cmd /c "cd ui && npm run tauri -- dev"

# Build production frontend dist files
frontend:
    node ui/scripts/build-frontend.mjs

# Build production NSIS installer bundle
bundle:
    node ui/scripts/build-frontend.mjs
    cmd /c "cd ui && npm run tauri -- build"
