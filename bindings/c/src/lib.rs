//! C ABI for mmreg: `libmmreg.so`, `libmmreg.a` (declared in `include/mmreg.h`), also loaded by
//! the Python `ctypes` binding in `bindings/python`.
//!
//! Safety and panic policy:
//! - Clippy lints `unwrap_used`, `expect_used`, `indexing_slicing`, and `panic` are denied across
//!   this module so no C path can panic when `panic = "abort"` is active in release builds.
//! - Every `extern "C"` entry point wraps its body in `std::panic::catch_unwind` for builds that
//!   unwind.
//! - Errors are recorded per thread and retrieved via [`mmreg_last_error`] and
//!   [`mmreg_last_error_code`]; nothing is ever written to `stderr`.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::Mutex;
use std::time::Duration;

use mmreg_core::{uio, Device, DeviceBuilder, MemoryMapper, MmregError, OwnedLockGuard, Width};

/// Open the device read-only (`O_RDONLY` + `PROT_READ`).
pub const MMREG_O_RDONLY: u32 = 1 << 0;
/// Skip blueprint and hardware pre-flight checks (`--force`).
pub const MMREG_O_FORCE: u32 = 1 << 1;
/// Back the device with an in-memory buffer for off-target C/Python unit tests (requires `MMREG_O_FORCE`).
pub const MMREG_O_MOCK: u32 = 1 << 2;
/// Mask of known low flag bits; bits `8..=15` encode the UIO region index (`MMREG_REGION(n)`).
const KNOWN_FLAGS_MASK: u32 = MMREG_O_RDONLY | MMREG_O_FORCE | MMREG_O_MOCK | 0x0000_FF00;
/// Largest in-memory window `MMREG_O_MOCK` allocates, so a bad `size` fails instead of aborting
/// on allocation failure.
const MOCK_MAX_SIZE: usize = 16 << 20;

/// NUL-terminated crate version returned by [`mmreg_version`].
static VERSION_CSTR: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

static EMPTY_CSTR: [u8; 1] = [0];

struct ThreadError {
    code: CString,
    message: CString,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<ThreadError>> = const { RefCell::new(None) };
}

fn sanitize_c_string(s: &str) -> CString {
    let bytes: Vec<u8> = s
        .as_bytes()
        .iter()
        .copied()
        .map(|b| if b == 0 { b' ' } else { b })
        .collect();
    CString::new(bytes).unwrap_or_default()
}

fn set_error(code: &str, message: &str) {
    let err = ThreadError {
        code: sanitize_c_string(code),
        message: sanitize_c_string(message),
    };
    // `try_with` fails only while this thread's TLS is being torn down; the error is dropped then.
    let _ = LAST_ERROR.try_with(|slot| {
        *slot.borrow_mut() = Some(err);
    });
}

fn set_mmreg_error(err: &MmregError) {
    set_error(err.code(), &err.to_string());
}

fn set_panic_error() {
    set_error(
        "E_INTERNAL_PANIC",
        "[E_INTERNAL_PANIC] Unexpected panic caught at the C ABI boundary.",
    );
}

/// In-memory byte-addressable mapper used when `MMREG_O_MOCK` is passed for off-target tests.
struct CapiMockMapper {
    mem: Mutex<Vec<u8>>,
    mapped: bool,
}

impl CapiMockMapper {
    fn boxed(size: usize) -> Box<dyn MemoryMapper> {
        Box::new(CapiMockMapper {
            mem: Mutex::new(vec![0; size]),
            mapped: false,
        })
    }
}

impl MemoryMapper for CapiMockMapper {
    fn map(&mut self, _address: u64, size: usize) -> Result<(), MmregError> {
        let mut mem = self
            .mem
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if mem.len() < size {
            mem.resize(size, 0);
        }
        self.mapped = true;
        Ok(())
    }

    fn unmap(&mut self) {
        self.mapped = false;
    }

    fn is_mapped(&self) -> bool {
        self.mapped
    }

    fn read(&self, offset: u32, width: Width) -> u64 {
        let mem = self
            .mem
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let off = offset as usize;
        let Some(slice) = mem.get(off..off.saturating_add(width.bytes())) else {
            return 0;
        };
        let mut buf = [0u8; 8];
        if let Some(dst) = buf.get_mut(..slice.len()) {
            dst.copy_from_slice(slice);
        }
        u64::from_le_bytes(buf)
    }

    fn write(&self, offset: u32, width: Width, value: u64) {
        let mut mem = self
            .mem
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let off = offset as usize;
        let bytes = value.to_le_bytes();
        if let (Some(dst), Some(src)) = (
            mem.get_mut(off..off.saturating_add(width.bytes())),
            bytes.get(..width.bytes()),
        ) {
            dst.copy_from_slice(src);
        }
    }
}

/// Opaque C handle representing an open [`Device`] and any active sequence locks.
#[allow(non_camel_case_types)]
pub struct mmreg_dev {
    // Declared before `device` so held sequence locks are released before the window is unmapped.
    guards: Mutex<Vec<OwnedLockGuard>>,
    device: Device,
    warnings: Vec<CString>,
}

impl mmreg_dev {
    fn new(device: Device) -> Self {
        let warnings = device
            .warnings()
            .iter()
            .map(|w| sanitize_c_string(w))
            .collect();
        mmreg_dev {
            guards: Mutex::new(Vec::new()),
            device,
            warnings,
        }
    }
}

/// Decodes `(read_only, force, mock, region_index)` from `flags`.
fn parse_flags(flags: u32) -> Result<(bool, bool, bool, usize), ()> {
    if flags & !KNOWN_FLAGS_MASK != 0 {
        set_error(
            "E_INVALID_ARG",
            &format!(
                "[E_INVALID_ARG] Unknown flag bits in 0x{flags:08X}. \
                 Remediation: Use MMREG_O_RDONLY, MMREG_O_FORCE, MMREG_O_MOCK, and MMREG_REGION(n)."
            ),
        );
        return Err(());
    }
    let read_only = (flags & MMREG_O_RDONLY) != 0;
    let force = (flags & MMREG_O_FORCE) != 0;
    let mock = (flags & MMREG_O_MOCK) != 0;
    let region_index = ((flags >> 8) & 0xFF) as usize;
    if mock && !force {
        set_error(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] MMREG_O_MOCK requires MMREG_O_FORCE.",
        );
        return Err(());
    }
    Ok((read_only, force, mock, region_index))
}

/// Converts a C string pointer into `&str`, recording `[E_INVALID_ARG]` on NULL or invalid UTF-8.
///
/// # Safety
/// `ptr` must either be NULL or point to a valid NUL-terminated C string.
unsafe fn cstr_to_str<'a>(ptr: *const c_char, label: &str) -> Result<&'a str, ()> {
    if ptr.is_null() {
        set_error(
            "E_INVALID_ARG",
            &format!("[E_INVALID_ARG] NULL '{label}' pointer passed to mmreg."),
        );
        return Err(());
    }
    // SAFETY: Caller guarantees non-null `ptr` points to a NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| {
        set_error(
            "E_INVALID_ARG",
            &format!("[E_INVALID_ARG] Parameter '{label}' is not valid UTF-8."),
        );
    })
}

/// Converts a raw `*const mmreg_dev` into `&mmreg_dev`.
///
/// # Safety
/// `dev` must be NULL or a valid handle returned by `mmreg_open_*`.
unsafe fn ref_dev<'a>(dev: *const mmreg_dev) -> Result<&'a mmreg_dev, ()> {
    // SAFETY: Caller guarantees `dev` is either NULL or a valid `mmreg_dev` allocation.
    unsafe { dev.as_ref() }.ok_or_else(|| {
        set_error(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] NULL mmreg_dev pointer passed to mmreg.",
        );
    })
}

fn open_with_builder(mut builder: DeviceBuilder, flags: u32, mock_size: usize) -> *mut mmreg_dev {
    let Ok((read_only, force, mock, region_index)) = parse_flags(flags) else {
        return ptr::null_mut();
    };
    builder = builder
        .region(region_index)
        .with_read_only(read_only)
        .with_force(force);
    if mock {
        if mock_size > MOCK_MAX_SIZE {
            set_error(
                "E_INVALID_ARG",
                &format!(
                    "[E_INVALID_ARG] MMREG_O_MOCK window of {mock_size} bytes exceeds the                      {MOCK_MAX_SIZE}-byte limit."
                ),
            );
            return ptr::null_mut();
        }
        builder = builder.with_custom_mapper(CapiMockMapper::boxed(mock_size.max(4096)));
    }
    match builder.open() {
        Ok(dev) => Box::into_raw(Box::new(mmreg_dev::new(dev))),
        Err(err) => {
            set_mmreg_error(&err);
            ptr::null_mut()
        }
    }
}

/// Returns the last error message (`"[E_CODE] ..."`), or `""` if no error is set on this thread.
///
/// The returned pointer is valid until the next `mmreg_*` call on the same thread.
#[no_mangle]
pub extern "C" fn mmreg_last_error() -> *const c_char {
    LAST_ERROR
        .try_with(|slot| match &*slot.borrow() {
            Some(err) => err.message.as_ptr(),
            None => EMPTY_CSTR.as_ptr().cast::<c_char>(),
        })
        .unwrap_or(EMPTY_CSTR.as_ptr().cast::<c_char>())
}

/// Returns the machine-readable error code (`"E_UIO_NOT_FOUND"`, etc.) for the last error on this
/// thread, or `""` if no error is set.
#[no_mangle]
pub extern "C" fn mmreg_last_error_code() -> *const c_char {
    LAST_ERROR
        .try_with(|slot| match &*slot.borrow() {
            Some(err) => err.code.as_ptr(),
            None => EMPTY_CSTR.as_ptr().cast::<c_char>(),
        })
        .unwrap_or(EMPTY_CSTR.as_ptr().cast::<c_char>())
}

/// Clears the calling thread's last error state.
#[no_mangle]
pub extern "C" fn mmreg_clear_error() {
    let _ = LAST_ERROR.try_with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// Returns the library version (`"MAJOR.MINOR.PATCH"`) as a static NUL-terminated string, so C
/// and Python callers can check the loaded `libmmreg` against the `MMREG_VERSION_*` they built with.
#[no_mangle]
pub extern "C" fn mmreg_version() -> *const c_char {
    VERSION_CSTR.as_ptr().cast::<c_char>()
}

/// Resolves a UIO device `name` (`/sys/class/uio/uio*/name`) to its `/dev/uioN` path and writes
/// the NUL-terminated path into `buf` (of capacity `len` bytes).
///
/// Returns `0` on success or `-1` on error (setting [`mmreg_last_error`]).
///
/// # Safety
/// `name` must be a valid NUL-terminated C string, and `buf` must point to a writable buffer of at
/// least `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn mmreg_find_uio(
    name: *const c_char,
    buf: *mut c_char,
    len: usize,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(name_str) = (unsafe { cstr_to_str(name, "name") }) else {
            return -1;
        };
        if buf.is_null() {
            set_error(
                "E_INVALID_ARG",
                "[E_INVALID_ARG] NULL 'buf' pointer passed to mmreg_find_uio.",
            );
            return -1;
        }
        match uio::find(name_str) {
            Ok(path) => {
                let bytes = path.as_bytes();
                if bytes.len().saturating_add(1) > len {
                    set_error(
                        "E_BUFFER_TOO_SMALL",
                        &format!(
                            "[E_BUFFER_TOO_SMALL] Buffer of {len} bytes is too small for UIO path '{}' ({} bytes needed).",
                            path,
                            bytes.len().saturating_add(1)
                        ),
                    );
                    return -1;
                }
                // SAFETY: `buf` is non-null and `len >= bytes.len() + 1`.
                unsafe {
                    ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), bytes.len());
                    *buf.add(bytes.len()) = 0;
                }
                0
            }
            Err(err) => {
                set_mmreg_error(&err);
                -1
            }
        }
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Opens a UIO device by its `/sys/class/uio/uio*/name` attribute.
///
/// Returns a non-null `mmreg_dev *` on success, or `NULL` on error (inspect with
/// [`mmreg_last_error`]).
///
/// # Safety
/// `name` must be NULL or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mmreg_open_uio(name: *const c_char, flags: u32) -> *mut mmreg_dev {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(name_str) = (unsafe { cstr_to_str(name, "name") }) else {
            return ptr::null_mut();
        };
        open_with_builder(Device::uio(name_str), flags, 4096)
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        ptr::null_mut()
    })
}

/// Opens a UIO device by its device node path (for example `"/dev/uio0"`).
///
/// Returns a non-null `mmreg_dev *` on success, or `NULL` on error.
///
/// # Safety
/// `path` must be NULL or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mmreg_open_uio_path(path: *const c_char, flags: u32) -> *mut mmreg_dev {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(path_str) = (unsafe { cstr_to_str(path, "path") }) else {
            return ptr::null_mut();
        };
        open_with_builder(Device::uio_path(path_str), flags, 4096)
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        ptr::null_mut()
    })
}

/// Opens a physical address window `[base, base + size)` via a covering UIO region or `/dev/mem`
/// (requires the `debug` Cargo feature).
///
/// Returns a non-null `mmreg_dev *` on success, or `NULL` on error.
#[no_mangle]
pub extern "C" fn mmreg_open_addr(base: u64, size: usize, flags: u32) -> *mut mmreg_dev {
    catch_unwind(AssertUnwindSafe(|| {
        open_with_builder(Device::addr(base, size), flags, size)
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        ptr::null_mut()
    })
}

/// Closes and frees `dev` (unmapping the register window and releasing any held locks).
///
/// Passing `NULL` is a safe no-op.
///
/// # Safety
/// `dev` must be `NULL` or a pointer returned by `mmreg_open_*` that has not yet been closed.
#[no_mangle]
pub unsafe extern "C" fn mmreg_close(dev: *mut mmreg_dev) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !dev.is_null() {
            // SAFETY: `dev` was allocated with `Box::into_raw` in `open_with_builder`.
            drop(unsafe { Box::from_raw(dev) });
        }
    }));
}

macro_rules! impl_c_read {
    ($fn_name:ident, $method:ident, $ty:ty) => {
        /// Reads a register at byte `offset` into `*out`. Returns `0` on success, `-1` on error.
        ///
        /// # Safety
        /// `dev` must be a valid `mmreg_dev` handle and `out` must be a valid writable pointer.
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(
            dev: *const mmreg_dev,
            offset: u32,
            out: *mut $ty,
        ) -> c_int {
            catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: Forwarded from C caller contract.
                let Ok(handle) = (unsafe { ref_dev(dev) }) else {
                    return -1;
                };
                if out.is_null() {
                    set_error(
                        "E_INVALID_ARG",
                        "[E_INVALID_ARG] NULL 'out' pointer passed to mmreg_rd*.",
                    );
                    return -1;
                }
                match handle.device.$method(offset) {
                    Ok(val) => {
                        // SAFETY: `out` was checked for non-null and is aligned by C caller contract.
                        unsafe { ptr::write(out, val) };
                        0
                    }
                    Err(err) => {
                        set_mmreg_error(&err);
                        -1
                    }
                }
            }))
            .unwrap_or_else(|_| {
                set_panic_error();
                -1
            })
        }
    };
}

macro_rules! impl_c_write {
    ($fn_name:ident, $method:ident, $ty:ty) => {
        /// Writes `value` to the register at byte `offset`. Returns `0` on success, `-1` on error.
        ///
        /// # Safety
        /// `dev` must be a valid `mmreg_dev` handle.
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(dev: *const mmreg_dev, offset: u32, value: $ty) -> c_int {
            catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: Forwarded from C caller contract.
                let Ok(handle) = (unsafe { ref_dev(dev) }) else {
                    return -1;
                };
                match handle.device.$method(offset, value) {
                    Ok(()) => 0,
                    Err(err) => {
                        set_mmreg_error(&err);
                        -1
                    }
                }
            }))
            .unwrap_or_else(|_| {
                set_panic_error();
                -1
            })
        }
    };
}

impl_c_read!(mmreg_rd8, read_u8, u8);
impl_c_read!(mmreg_rd16, read_u16, u16);
impl_c_read!(mmreg_rd32, read_u32, u32);
impl_c_read!(mmreg_rd64, read_u64, u64);

impl_c_write!(mmreg_wr8, write_u8, u8);
impl_c_write!(mmreg_wr16, write_u16, u16);
impl_c_write!(mmreg_wr32, write_u32, u32);
impl_c_write!(mmreg_wr64, write_u64, u64);

/// Atomically updates bits in `mask` at byte `offset` (`(old & !mask) | (value & mask)`) under a
/// single lock. Writes the new 32-bit register value to `*out_new` when `out_new != NULL`.
///
/// Returns `0` on success or `-1` on error.
///
/// # Safety
/// `dev` must be a valid `mmreg_dev` handle; `out_new` may be `NULL` or a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn mmreg_update32(
    dev: *const mmreg_dev,
    offset: u32,
    mask: u32,
    value: u32,
    out_new: *mut u32,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(handle) = (unsafe { ref_dev(dev) }) else {
            return -1;
        };
        match handle.device.update_u32(offset, mask, value) {
            Ok(updated) => {
                if !out_new.is_null() {
                    // SAFETY: `out_new` is non-null.
                    unsafe { ptr::write(out_new, updated) };
                }
                0
            }
            Err(err) => {
                set_mmreg_error(&err);
                -1
            }
        }
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Acquires a reentrant sequence lock on `dev`, held until a matching [`mmreg_unlock`] call on the
/// same thread (or [`mmreg_close`]).
///
/// Returns `0` on success or `-1` on error.
///
/// # Safety
/// `dev` must be a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_lock(dev: *const mmreg_dev) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(handle) = (unsafe { ref_dev(dev) }) else {
            return -1;
        };
        match handle.device.lock() {
            Ok(guard) => {
                let owned = guard.into_owned();
                let mut stack = handle
                    .guards
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                stack.push(owned);
                0
            }
            Err(err) => {
                set_mmreg_error(&err);
                -1
            }
        }
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Releases one sequence lock acquired by [`mmreg_lock`] on the calling thread.
///
/// Returns `0` on success or `-1` if no sequence lock is held by the calling thread.
///
/// # Safety
/// `dev` must be a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_unlock(dev: *const mmreg_dev) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(handle) = (unsafe { ref_dev(dev) }) else {
            return -1;
        };
        let popped = {
            let mut stack = handle
                .guards
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(last) = stack.last() else {
                set_error(
                    "E_LOCK_ACQUIRE",
                    "[E_LOCK_ACQUIRE] mmreg_unlock called without a matching mmreg_lock.",
                );
                return -1;
            };
            if !last.is_owned_by_current_thread() {
                set_error(
                    "E_LOCK_ACQUIRE",
                    "[E_LOCK_ACQUIRE] mmreg_unlock called from a thread that does not hold the lock.",
                );
                return -1;
            }
            stack.pop()
        };
        drop(popped);
        0
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Unmasks (re-enables) the interrupt line on a UIO-backed `dev` by writing `1u32` to `/dev/uioN`.
///
/// Returns `0` on success or `-1` on error.
///
/// # Safety
/// `dev` must be a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_irq_unmask(dev: *const mmreg_dev) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(handle) = (unsafe { ref_dev(dev) }) else {
            return -1;
        };
        match handle.device.irq_unmask() {
            Ok(()) => 0,
            Err(err) => {
                set_mmreg_error(&err);
                -1
            }
        }
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Waits up to `timeout_ms` milliseconds for a UIO interrupt on `dev`.
///
/// Returns:
/// - `1` when an interrupt occurred (writing the cumulative kernel interrupt count to `*out_count`
///   if `out_count != NULL`),
/// - `0` when the timeout elapsed with no interrupt,
/// - `-1` on error (inspect with [`mmreg_last_error`]).
///
/// # Safety
/// `dev` must be a valid `mmreg_dev` handle; `out_count` may be `NULL` or a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn mmreg_irq_wait(
    dev: *const mmreg_dev,
    timeout_ms: u64,
    out_count: *mut u32,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        let Ok(handle) = (unsafe { ref_dev(dev) }) else {
            return -1;
        };
        match handle.device.irq_wait(Duration::from_millis(timeout_ms)) {
            Ok(Some(count)) => {
                if !out_count.is_null() {
                    // SAFETY: `out_count` is non-null.
                    unsafe { ptr::write(out_count, count) };
                }
                1
            }
            Ok(None) => 0,
            Err(err) => {
                set_mmreg_error(&err);
                -1
            }
        }
    }))
    .unwrap_or_else(|_| {
        set_panic_error();
        -1
    })
}

/// Returns the physical base address of `dev` (or `0` if `dev` is `NULL`).
///
/// # Safety
/// `dev` must be `NULL` or a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_base_address(dev: *const mmreg_dev) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        unsafe { dev.as_ref() }.map_or(0, |d| d.device.base_address())
    }))
    .unwrap_or(0)
}

/// Returns the mapped window size of `dev` in bytes (or `0` if `dev` is `NULL`).
///
/// # Safety
/// `dev` must be `NULL` or a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_size(dev: *const mmreg_dev) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        unsafe { dev.as_ref() }.map_or(0, |d| d.device.size())
    }))
    .unwrap_or(0)
}

/// Returns the number of non-fatal hardware pre-flight warnings recorded when `dev` was opened.
///
/// # Safety
/// `dev` must be `NULL` or a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_warning_count(dev: *const mmreg_dev) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        unsafe { dev.as_ref() }.map_or(0, |d| d.warnings.len())
    }))
    .unwrap_or(0)
}

/// Returns the `index`-th pre-flight warning string for `dev`, or `NULL` if `index` is out of
/// range or `dev` is `NULL`.
///
/// # Safety
/// `dev` must be `NULL` or a valid `mmreg_dev` handle.
#[no_mangle]
pub unsafe extern "C" fn mmreg_warning(dev: *const mmreg_dev, index: usize) -> *const c_char {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Forwarded from C caller contract.
        unsafe { dev.as_ref() }
            .and_then(|d| d.warnings.get(index))
            .map_or(ptr::null(), |s| s.as_ptr())
    }))
    .unwrap_or(ptr::null())
}

#[cfg(test)]
mod interop_tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_capi_mock_device_read_write_update_lock_and_errors() {
        mmreg_clear_error();
        // Initial error strings are empty, not NULL.
        let initial_err = unsafe { CStr::from_ptr(mmreg_last_error()) }
            .to_str()
            .unwrap();
        let initial_code = unsafe { CStr::from_ptr(mmreg_last_error_code()) }
            .to_str()
            .unwrap();
        assert_eq!(initial_err, "");
        assert_eq!(initial_code, "");

        // Rejects unknown flags or MMREG_O_MOCK without MMREG_O_FORCE.
        assert!(mmreg_open_addr(0x4000_0000, 64, 0x8000_0000).is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(mmreg_last_error_code()) }
                .to_str()
                .unwrap(),
            "E_INVALID_ARG"
        );
        assert!(mmreg_open_addr(0x4000_0000, 64, MMREG_O_MOCK).is_null());

        let dev = mmreg_open_addr(0x4000_0000, 64, MMREG_O_FORCE | MMREG_O_MOCK);
        assert!(!dev.is_null());
        assert_eq!(unsafe { mmreg_base_address(dev) }, 0x4000_0000);
        assert_eq!(unsafe { mmreg_size(dev) }, 64);
        assert_eq!(unsafe { mmreg_warning_count(dev) }, 0);
        assert!(unsafe { mmreg_warning(dev, 0) }.is_null());

        unsafe {
            assert_eq!(mmreg_wr8(dev, 0, 0x12), 0);
            let mut v8 = 0u8;
            assert_eq!(mmreg_rd8(dev, 0, &mut v8), 0);
            assert_eq!(v8, 0x12);

            assert_eq!(mmreg_wr16(dev, 2, 0x3456), 0);
            let mut v16 = 0u16;
            assert_eq!(mmreg_rd16(dev, 2, &mut v16), 0);
            assert_eq!(v16, 0x3456);

            assert_eq!(mmreg_wr32(dev, 4, 0x1122_3344), 0);
            let mut updated = 0u32;
            assert_eq!(
                mmreg_update32(dev, 4, 0x00FF_0000, 0x00AA_0000, &mut updated),
                0
            );
            assert_eq!(updated, 0x11AA_3344);
            let mut v32 = 0u32;
            assert_eq!(mmreg_rd32(dev, 4, &mut v32), 0);
            assert_eq!(v32, 0x11AA_3344);

            assert_eq!(mmreg_wr64(dev, 8, 0x0102_0304_0506_0708), 0);
            let mut v64 = 0u64;
            assert_eq!(mmreg_rd64(dev, 8, &mut v64), 0);
            assert_eq!(v64, 0x0102_0304_0506_0708);

            // Reentrant sequence locking
            assert_eq!(mmreg_lock(dev), 0);
            assert_eq!(mmreg_lock(dev), 0);
            assert_eq!(mmreg_wr32(dev, 16, 0xDEAD_BEEF), 0);
            assert_eq!(mmreg_unlock(dev), 0);
            assert_eq!(mmreg_unlock(dev), 0);
            // Extra unlock fails with E_LOCK_ACQUIRE
            assert_eq!(mmreg_unlock(dev), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_LOCK_ACQUIRE"
            );

            // Alignment and bounds errors
            assert_eq!(mmreg_rd32(dev, 2, &mut v32), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_UNALIGNED_ACCESS"
            );
            assert_eq!(mmreg_rd32(dev, 64, &mut v32), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_OUT_OF_BOUNDS"
            );

            // IRQ operations on non-UIO device return E_UIO_REQUIRED
            assert_eq!(mmreg_irq_unmask(dev), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_UIO_REQUIRED"
            );
            assert_eq!(mmreg_irq_wait(dev, 1, ptr::null_mut()), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_UIO_REQUIRED"
            );

            mmreg_close(dev);
            mmreg_close(ptr::null_mut());
        }
    }

    #[test]
    fn test_capi_read_only_and_uio_lookup_errors() {
        unsafe {
            let name = CString::new("mock_ro_uio").unwrap();
            let ro = mmreg_open_uio(name.as_ptr(), MMREG_O_RDONLY | MMREG_O_FORCE | MMREG_O_MOCK);
            assert!(!ro.is_null());
            assert_eq!(mmreg_wr32(ro, 0, 1), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_READ_ONLY"
            );
            mmreg_close(ro);

            let missing = CString::new("nonexistent_uio_xyz").unwrap();
            let mut buf = [0 as c_char; 64];
            assert_eq!(mmreg_find_uio(missing.as_ptr(), buf.as_mut_ptr(), 64), -1);
            assert_eq!(
                CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
                "E_UIO_NOT_FOUND"
            );
        }
    }

    #[test]
    fn test_mmreg_header_matches_exported_capi_symbols() {
        let header = include_str!("../include/mmreg.h");
        let src = include_str!("lib.rs");

        // Every `pub extern "C" fn mmreg_*`, `pub unsafe extern "C" fn mmreg_*`, and
        // `impl_c_read!(mmreg_*)` / `impl_c_write!(mmreg_*)` in `src/lib.rs` must be declared in
        // `include/mmreg.h`.
        let mut exported = Vec::new();
        for line in src.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed
                .strip_prefix("pub extern \"C\" fn ")
                .or_else(|| trimmed.strip_prefix("pub unsafe extern \"C\" fn "))
            {
                let name = rest.split('(').next().unwrap().trim();
                if !name.starts_with('$') {
                    exported.push(name);
                }
            } else if let Some(rest) = trimmed
                .strip_prefix("impl_c_read!(")
                .or_else(|| trimmed.strip_prefix("impl_c_write!("))
            {
                let name = rest.split(',').next().unwrap().trim();
                exported.push(name);
            }
        }
        assert_eq!(exported.len(), 26);
        for name in exported {
            assert!(
                header.contains(&format!("{name}(")),
                "Symbol `{name}` is exported by src/lib.rs but missing from include/mmreg.h"
            );
        }
        assert!(header.contains("#define MMREG_FIELD(v, mask, shift)"));

        // `MMREG_VERSION_*` in the header must match Cargo.toml.
        for (macro_name, value) in [
            ("MMREG_VERSION_MAJOR", env!("CARGO_PKG_VERSION_MAJOR")),
            ("MMREG_VERSION_MINOR", env!("CARGO_PKG_VERSION_MINOR")),
            ("MMREG_VERSION_PATCH", env!("CARGO_PKG_VERSION_PATCH")),
        ] {
            assert!(
                header.contains(&format!(
                    "#define {macro_name} {value}
"
                )),
                "include/mmreg.h `{macro_name}` does not match Cargo.toml version {}",
                env!("CARGO_PKG_VERSION")
            );
        }
        let version = unsafe { CStr::from_ptr(mmreg_version()) }.to_str().unwrap();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_capi_mock_size_limit() {
        assert!(mmreg_open_addr(
            0x4000_0000,
            MOCK_MAX_SIZE + 4096,
            MMREG_O_FORCE | MMREG_O_MOCK
        )
        .is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(mmreg_last_error_code()) }
                .to_str()
                .unwrap(),
            "E_INVALID_ARG"
        );
    }
}
