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

# Lint both the default build and the minimal (`--no-default-features`) build
clippy:
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-targets --no-default-features -- -D warnings

lint: fmt-check clippy

# Unit and doc tests for both feature sets
test:
    cargo test
    cargo test --no-default-features

# Line coverage, failing under 80%
coverage:
    cargo llvm-cov --all-targets --fail-under-lines 80

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

# Release builds of every target in targets.txt with `cross`
[unix]
build-targets:
    #!/usr/bin/env sh
    set -eu
    for target in $(tr -d '\r' < targets.txt | grep -v '^#' | grep -v '^$'); do
        echo "=== $target"
        cross build --release --target "$target"
    done

[windows]
build-targets:
    Get-Content targets.txt | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" -and -not $_.StartsWith("#") } | ForEach-Object { Write-Host "=== $_"; cross build --release --target $_; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } }

clean:
    cargo clean
