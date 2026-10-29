# `mmreg` Workspace & Hardware Safety Rules

When developing, testing, or executing `mmreg` commands in this workspace:

1. **Target Machine vs. Development Machine**:
   - `mmreg` reads and writes physical hardware registers on the **target embedded Linux machine**, not on the development host.
   - Before running any `mmreg read` / `mmreg write` command, verify target connectivity (e.g., `ssh <user>@<target-ip>`) or ask the user for target connection details, and use `sudo mmreg ...` on the target when `/dev/mem` or `/proc/iomem` access is required.
2. **Centralized Structured Error Codes (`src/error.rs`)**:
   - All error codes (`[E_*]`) and diagnostic formatting must live in `src/error.rs` (`MmregError`), including the inspected sysfs/procfs path and a `Remediation:` hint.
3. **Non-FPGA vs. FPGA Pre-Flight Isolation**:
   - Never block non-FPGA peripherals (standard SoC PS blocks under `/soc`, `/amba`, PCIe, x86) based on `/sys/class/fpga_manager/*/state`. Only enforce FPGA manager state when `is_fpga_backed_device` confirms the peripheral resides on the FPGA fabric (`amba_pl`, `fpga-region`, etc.).
4. **CI & Coverage Gate (`just ci`)**:
   - Before completing code changes, run `just ci` to verify formatting (`rustfmt`), lint cleanliness (`clippy -D warnings`, default and `--no-default-features`), unit + doc tests, and line coverage (`>80%` via `cargo llvm-cov`).
   - On Windows the Unix mapping code is not compiled: also run `cargo clippy` and `cargo test` on Linux (or WSL), and `cargo clippy --target armv7-unknown-linux-gnueabihf` for the 32-bit path.
5. **Footprint**:
   - `mmreg` competes with single-file `devmem2`: prefer `std` over new dependencies, keep `libc` the only mandatory one, and put optional functionality behind a cargo feature.
