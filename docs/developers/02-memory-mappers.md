# 2. Memory Mapper Backends

All physical memory access goes through the [`MemoryMapper`](../../src/memregs.rs) trait.

---

## The `MemoryMapper` Trait

```rust
pub trait MemoryMapper: Send {
    fn map(&mut self, address: u64, size: usize) -> Result<(), MmregError>;
    fn unmap(&mut self);
    fn is_mapped(&self) -> bool;
    fn read_u32(&self, offset: u32) -> u32;
    fn write_u32(&self, offset: u32, value: u32);
}
```

`offset` is relative to the `address` passed to `map`. `Interface` only calls `read_u32` /
`write_u32` while mapped, with `offset % 4 == 0` and `offset + 4 <= size`.

`OsMapper` implements it for a resolved `MapperType` (`DevMem` or `Uio`); the difference between
the two is only how the window is opened. The OS-facing part, `mmio::Mapping`, maps a device file
and unmaps it on drop.

---

## Window Arithmetic

### `/dev/mem`
- `mmap(2)` offsets must be page multiples: `page_base = address & !(page_size - 1)`.
- `Mapping` maps `(address - page_base) + size` bytes from `page_base` and adds
  `address - page_base` to every access.
- `/dev/mem` is opened `O_SYNC`, so the kernel maps the range uncached.
- On 32-bit glibc and Android, `off_t` is 32 bits; `mmap64` is used so physical addresses at or
  above 2 GiB (most ARM peripherals) can be mapped.

### UIO (`/dev/uioN`)
- Region `M` is selected with the mmap offset `M * page_size`.
- The kernel maps the region from the page holding its sysfs `addr`; `size` counts from `addr`.
  (`uio_pdrv_genirq` reports a page-aligned `addr` and the device's position within the page as
  `offset`; `mmreg` works from physical addresses, so it needs only `addr` and `size`.)
- For a target `address`: the range must satisfy `addr <= address` and
  `address + size <= addr + size_region`, and the access offset is
  `address - (addr & !(page_size - 1))`.
- `addr` and `size` are always read from `/sys/class/uio/uioN/maps/mapM/`, both for
  `MapperType::Auto` and for an explicit `MapperType::Uio { device_path, region_index }`; a range
  outside the region is `[E_UIO_ADDRESS]`.

---

## Custom or Mock Mappers

`Interface::with_custom_mapper` injects any `MemoryMapper`, which allows unit tests of register and
bitfield logic without root or hardware. A custom mapper is kept across `unmap()` and failed
`map()` calls. The mock in `interface::tests` (`MockMapper`) is a complete example backed by
`UnsafeCell<u32>` words.
