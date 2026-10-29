# `mmreg` User Guide

Documentation for the `mmreg` CLI and Rust library.

1. **[About `mmreg`](01-about.md)**: why raw `/dev/mem` tools like `devmem2` hang systems, and the
   four layers of protection `mmreg` adds.
2. **[Installation & Setup](02-installation.md)**: `cargo install`, the minimal build, cross
   compilation, Yocto / PetaLinux recipes, and UIO, `/dev/mem` and lock-directory permissions.
3. **[CLI Usage](03-cli-usage.md)**: commands, number formats, `--validate` and `--force`.
4. **[Library Usage](04-library-usage.md)**: one-line helpers, `Interface` / `Register` /
   `SubRegister`, `MapperType`, safety profiles and `MmregError`.
5. **[Safety & Hardware Pre-Flight](05-safety-and-preflight.md)**: blueprint validation, zombie
   hardware checks, and the safety-profile format.
6. **[Troubleshooting & Error Reference](06-troubleshooting.md)**: every `[E_*]` code and its fix.
