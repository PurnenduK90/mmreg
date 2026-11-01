# Task runner for mmreg (local and CI). `just` runs the full CI pipeline.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default: ci

# Everything CI runs except cross builds
ci: fmt-check clippy test coverage doc package-check python-wheel-test

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

# The Python package from the source tree, loading target/debug/libmmreg.so
py_env := "MMREG_LIB_PATH=target/debug/libmmreg.so PYTHONPATH=bindings/python"

# Unit and doc tests for the minimal, default and full builds, and the C ABI crate, then the Python
# tests and the examples. The C interop tests compile bindings/c/examples/*.c against
# target/debug/libmmreg.a, and fail instead of skipping when `cc` or `libmmreg.a` is missing.
[unix]
test: && test-python examples
    cargo test --no-default-features
    cargo test
    cargo build -p mmreg-capi
    MMREG_REQUIRE_C_TESTS=1 cargo test --workspace --all-features

# The C interop test and the Python binding are Unix-only, so Windows needs no MMREG_REQUIRE_C_TESTS
[windows]
test: && examples
    cargo test --no-default-features
    cargo test
    cargo test --workspace --all-features

# Python binding tests against target/debug/libmmreg.so
[unix]
test-python:
    cargo build -p mmreg-capi
    {{py_env}} python3 -m unittest discover -s bindings/python/tests -v

# Runs the mock-device examples. The hardware examples (`uio_irq`) are only compiled: `cargo test`
# builds examples/*.rs and the C interop test bindings/c/examples/*.c.
[unix]
examples:
    cargo run --quiet --example mock_quickstart
    cargo build -p mmreg-capi
    {{py_env}} python3 bindings/python/examples/quickstart.py
    python3 -m py_compile bindings/python/examples/uio_irq.py

[windows]
examples:
    cargo run --quiet --example mock_quickstart

# Python wheel bundling a release libmmreg.so, in target/wheels/: `just python-wheel` for this
# machine's CPU, or `just python-wheel <triple>` for a glibc target built with `cross`
# (e.g. aarch64-unknown-linux-gnu, armv7-unknown-linux-gnueabihf, riscv64gc-unknown-linux-gnu).
[unix]
python-wheel target="":
    #!/usr/bin/env bash
    set -euo pipefail
    target="{{target}}"
    if [ -z "$target" ]; then
        cargo build --release -p mmreg-capi
        lib=target/release/libmmreg.so
        arch=$(uname -m)
    else
        case "$target" in
            *-linux-gnu*) ;;
            *) echo "python-wheel: needs a Linux glibc target (*-linux-gnu*), not $target" >&2; exit 1 ;;
        esac
        cross build --release -p mmreg-capi --target "$target"
        lib="target/$target/release/libmmreg.so"
        arch="${target%%-*}"
    fi
    case "$arch" in armv7*) arch=armv7l ;; riscv64*) arch=riscv64 ;; esac
    # Build from a staging copy so the source tree never holds a libmmreg.so for plain installs.
    stage=target/python-wheel
    rm -rf "$stage" && mkdir -p "$stage" target/wheels
    cp -R bindings/python/mmreg bindings/python/pyproject.toml bindings/python/README.md LICENSE.txt "$stage"/
    find "$stage" -name __pycache__ -prune -exec rm -rf {} +
    cp "$lib" "$stage/mmreg/libmmreg.so"
    python3 -m pip wheel --no-deps --wheel-dir target/wheels \
        --config-settings=--build-option=--plat-name=linux_$arch "$stage"

# Installs this machine's wheel into a fresh venv and runs the tests and the mock example against
# the installed package and its bundled library (no PYTHONPATH, no MMREG_LIB_PATH).
[unix]
python-wheel-test: python-wheel
    #!/usr/bin/env bash
    set -euo pipefail
    venv=target/python-venv
    rm -rf "$venv" && python3 -m venv "$venv"
    "$venv/bin/pip" install --quiet --force-reinstall target/wheels/mmreg-*-linux_$(uname -m).whl
    cd "$venv"
    env -u MMREG_LIB_PATH -u PYTHONPATH ./bin/python - <<'PY'
    import mmreg
    path = mmreg._get_lib()._name
    assert path.endswith("site-packages/mmreg/libmmreg.so"), path
    print("bundled library:", path)
    PY
    env -u MMREG_LIB_PATH -u PYTHONPATH ./bin/python -m unittest discover -s ../../bindings/python/tests
    env -u MMREG_LIB_PATH -u PYTHONPATH ./bin/python ../../bindings/python/examples/quickstart.py

[windows]
python-wheel-test:
    Write-Host "python-wheel-test: Linux only (libmmreg.so), skipped"

# Line coverage, failing under 80%
coverage:
    cargo llvm-cov --workspace --all-targets --all-features --fail-under-lines 80

# API docs with warnings denied (catches broken intra-doc links)
[unix]
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

[windows]
doc:
    $env:RUSTDOCFLAGS="-D warnings"; cargo doc --workspace --no-deps

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
