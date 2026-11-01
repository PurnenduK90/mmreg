# mmreg for Python

Safe, concurrent 8/16/32/64-bit memory-mapped register access on Linux (UIO and, in `debug`
builds, `/dev/mem`), with the same lock protocol and hardware pre-flight checks as the mmreg Rust
crate, C library and CLI. The package is pure Python (`ctypes`, no dependencies) over the
`libmmreg` C library, so target boards need no compiler.

## Install

**A wheel that bundles `libmmreg.so`**, for development machines and boards without the library.
Build it in a checkout of the [mmreg repository](https://github.com/PurnenduK90/mmreg) (needs
Rust, and [`cross`](https://github.com/cross-rs/cross) for other CPUs):

```sh
just python-wheel                                # this machine's CPU
just python-wheel aarch64-unknown-linux-gnu      # e.g. Zynq UltraScale+, i.MX8, Raspberry Pi 4/5
pip install target/wheels/mmreg-*-linux_aarch64.whl   # on the target
```

The wheel is tagged for one CPU (`linux_x86_64`, `linux_aarch64`, `linux_armv7l`,
`linux_riscv64`), so `pip` refuses it on another. `libmmreg.so` is built for glibc targets.

**The pure-Python package**, when `libmmreg.so` is installed separately (Yocto / Buildroot images,
see the [installation guide](https://github.com/PurnenduK90/mmreg/blob/main/docs/users/02-installation.md)):

```sh
pip install ./bindings/python
```

### Which `libmmreg` is loaded

The first match wins:

1. `MMREG_LIB_PATH` (full path to a `libmmreg.so`)
2. `libmmreg.so.1` / `libmmreg.so` next to the package (the bundled copy)
3. `target/release/` and `target/debug/` of the repository the package is imported from
4. `/usr/local/lib`, `/usr/lib`, `/usr/lib64`, then the system search path (`ctypes.util.find_library`)

The library is loaded on first use (`find_uio`, `Device.open_*`). If none is found, it cannot be
loaded (e.g. built for another CPU), or its major version differs from the package's, that call
raises `MmregError` `E_LIB_LOAD`.

## Example

Runs anywhere: `mock=True` backs the device with memory instead of hardware.

```python
from mmreg import Device

with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as d:
    d.write_u32(0x00, 0x1)
    d.update_u32(0x00, 0xF0, 0x5 << 4)          # locked read-modify-write

    d.define("CTRL", offset=0x00, fields={"EN": "0", "MODE": "7:4"})
    d.CTRL.EN = True
    print(hex(int(d.CTRL)), int(d.CTRL.MODE))  # 0x51 5
```

On hardware, open by UIO name: `Device.open_uio("axi_gpio")`. More in
[`examples/`](https://github.com/PurnenduK90/mmreg/tree/main/bindings/python/examples) and the
[Python section of the library guide](https://github.com/PurnenduK90/mmreg/blob/main/docs/users/04-library-usage.md#9-python-binding-bindingspythonmmreg).

## Errors

Every error is an `mmreg.MmregError` with a stable `.code` (`E_UIO_NOT_FOUND`, `E_INVALID_ARG`,
...), listed with fixes in the
[error reference](https://github.com/PurnenduK90/mmreg/blob/main/docs/users/06-troubleshooting.md).

## License

MIT
