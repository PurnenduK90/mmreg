# Task runner for mmreg (local and CI). `just` runs the full CI pipeline.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default: ci

# Everything CI runs except cross builds
ci: fmt-check clippy test coverage doc package-check

# CI plus release builds of every target in targets.txt (needs Docker + cross)
ci-all: ci build-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Lint the minimal (`--no-default-features`), default and full (`--all-features`) builds, plus the
# C ABI crate (bindings/c)
clippy:
    cargo clippy --all-targets --no-default-features -- -D warnings
    cargo clippy --all-targets -- -D warnings
    cargo clippy --workspace --all-targets --all-features -- -D warnings

lint: fmt-check clippy

# Unit and doc tests for the minimal, default and full builds, and the C ABI crate. Its C interop
# test compiles bindings/c/examples/test_capi.c against target/debug/libmmreg.a, and fails instead
# of skipping when `cc` or `libmmreg.a` is missing.
[unix]
test:
    cargo test --no-default-features
    cargo test
    cargo build -p mmreg-capi
    MMREG_REQUIRE_C_TESTS=1 cargo test --workspace --all-features

# The C interop test is Unix-only, so Windows needs no MMREG_REQUIRE_C_TESTS
[windows]
test:
    cargo test --no-default-features
    cargo test
    cargo test --workspace --all-features

# Python binding tests against target/debug/libmmreg.so
[unix]
test-python:
    cargo build -p mmreg-capi
    MMREG_LIB_PATH=target/debug/libmmreg.so PYTHONPATH=bindings/python python3 -m unittest discover -s bindings/python/tests -v

# Line coverage, failing under 80%
coverage:
    cargo llvm-cov --workspace --all-targets --all-features --fail-under-lines 80

# API docs with warnings denied (catches broken intra-doc links)
[unix]
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

[windows]
doc:
    $env:RUSTDOCFLAGS="-D warnings"; cargo doc --no-deps

# Files that would be published to crates.io
package-check:
    cargo package --list --allow-dirty

# Release builds of every target in targets.txt with `cross`: bring-up tools, so with `debug`
[unix]
build-targets:
    #!/usr/bin/env sh
    set -eu
    for target in $(tr -d '\r' < targets.txt | grep -v '^#' | grep -v '^$'); do
        echo "=== $target"
        cross build --release --features debug --target "$target"
    done

[windows]
build-targets:
    Get-Content targets.txt | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" -and -not $_.StartsWith("#") } | ForEach-Object { Write-Host "=== $_"; cross build --release --features debug --target $_; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } }

clean:
    cargo clean
