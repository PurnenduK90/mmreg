# `mmreg` User Guide

Documentation for the `mmreg` CLI and its Rust, C and Python libraries.

1. **[About `mmreg`](01-about.md)**: why raw `/dev/mem` tools like `devmem2` hang systems, and the
   four layers of protection `mmreg` adds.
2. **[Installation & Setup](02-installation.md)**: `cargo install`, the minimal build, cross
   compilation, the Python package (`pip`, wheels bundling `libmmreg.so`), Yocto / PetaLinux
   recipes, and UIO, `/dev/mem` and lock-directory permissions.
3. **[CLI Usage](03-cli-usage.md)**: commands, number formats, `--validate` and `--force`.
4. **[Library Usage](04-library-usage.md)**: one-line helpers, `Interface` / `Register` /
   `SubRegister`, `MapperType`, safety profiles and `MmregError`, the access-type rules for
   generated code, the C ABI and the Python binding, and the runnable examples.
5. **[Safety & Hardware Pre-Flight](05-safety-and-preflight.md)**: blueprint validation, zombie
   hardware checks, and the safety-profile format.
6. **[Troubleshooting & Error Reference](06-troubleshooting.md)**: every `[E_*]` code (Rust, CLI, C and
   Python) and its fix.
7. **[Block Files](07-block-files.md)**: the `mmreg-block/1` JSON files the Python package uses for
   access by name in debugging and experiments (draft, planned for 1.3).
