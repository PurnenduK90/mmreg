//! Cross-thread and two-process (C <-> Rust) tests of the C ABI.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::ffi::{CStr, CString};
use std::ptr;

use super::*;

#[test]
fn test_capi_cross_thread_unlock_rejected_and_null_guards() {
    mmreg_clear_error();

    unsafe {
        // NULL pointer handling across C ABI entry points
        assert!(mmreg_open_uio(ptr::null(), 0).is_null());
        assert_eq!(
            CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
            "E_INVALID_ARG"
        );
        assert!(mmreg_open_uio_path(ptr::null(), 0).is_null());
        assert_eq!(mmreg_find_uio(ptr::null(), ptr::null_mut(), 0), -1);

        let name = CString::new("axi_gpio").unwrap();
        assert_eq!(mmreg_find_uio(name.as_ptr(), ptr::null_mut(), 16), -1);

        let mut out32 = 0u32;
        assert_eq!(mmreg_rd32(ptr::null(), 0, &mut out32), -1);
        assert_eq!(mmreg_wr32(ptr::null(), 0, 1), -1);
        assert_eq!(mmreg_update32(ptr::null(), 0, 1, 1, ptr::null_mut()), -1);
        assert_eq!(mmreg_lock(ptr::null_mut()), -1);
        assert_eq!(mmreg_unlock(ptr::null_mut()), -1);
        assert_eq!(mmreg_irq_unmask(ptr::null()), -1);
        assert_eq!(mmreg_irq_wait(ptr::null(), 1, ptr::null_mut()), -1);
        assert_eq!(mmreg_base_address(ptr::null()), 0);
        assert_eq!(mmreg_size(ptr::null()), 0);
        assert_eq!(mmreg_warning_count(ptr::null()), 0);
        assert!(mmreg_warning(ptr::null(), 0).is_null());

        let path = CString::new("/dev/nonexistent_uio99").unwrap();
        let dev = mmreg_open_uio_path(path.as_ptr(), MMREG_O_FORCE | MMREG_O_MOCK);
        assert!(!dev.is_null());
        assert_eq!(mmreg_rd32(dev, 0, ptr::null_mut()), -1);
        assert_eq!(
            CStr::from_ptr(mmreg_last_error_code()).to_str().unwrap(),
            "E_INVALID_ARG"
        );

        // Lock on main thread; attempting to unlock from another thread must fail with E_LOCK_ACQUIRE.
        assert_eq!(mmreg_lock(dev), 0);
        let dev_addr = dev as usize;
        let rc = std::thread::spawn(move || {
            let d = dev_addr as *mut mmreg_dev;
            let res = mmreg_unlock(d);
            let code = CStr::from_ptr(mmreg_last_error_code())
                .to_str()
                .unwrap()
                .to_string();
            (res, code)
        })
        .join()
        .unwrap();
        assert_eq!(rc, (-1, "E_LOCK_ACQUIRE".to_string()));

        // Main thread still holds the lock and unlocks cleanly.
        assert_eq!(mmreg_unlock(dev), 0);
        mmreg_close(dev);
    }
}

/// Called when the C toolchain or `libmmreg.a` is missing: the test is skipped, unless
/// `MMREG_REQUIRE_C_TESTS` is set (as `just test` does), in which case it fails.
#[cfg(unix)]
fn missing_c_prerequisite(what: &str) {
    assert!(
        std::env::var_os("MMREG_REQUIRE_C_TESTS").is_none(),
        "{what} (MMREG_REQUIRE_C_TESTS is set)"
    );
}

#[cfg(unix)]
#[test]
fn test_c_staticlib_example_and_c_rust_lock_interop() {
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // The workspace target directory, two levels above this crate unless CARGO_TARGET_DIR is set.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target"));
    let staticlib = [
        target_dir.join("debug/libmmreg.a"),
        target_dir.join("release/libmmreg.a"),
    ]
    .into_iter()
    .find(|p| p.is_file());
    let Some(lib_path) = staticlib else {
        missing_c_prerequisite("libmmreg.a not found: run `cargo build -p mmreg-capi` first");
        return;
    };

    let out_bin = std::env::temp_dir().join(format!("mmreg_test_capi_{}", std::process::id()));
    let status = Command::new("cc")
        .arg("-O2")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-DNDEBUG")
        .arg("-I")
        .arg(manifest_dir.join("include"))
        .arg(manifest_dir.join("examples/test_capi.c"))
        .arg(&lib_path)
        .args([
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ])
        .arg("-o")
        .arg(&out_bin)
        .status();
    let Ok(cc_status) = status else {
        missing_c_prerequisite("C compiler `cc` not found");
        return;
    };
    assert!(
        cc_status.success(),
        "Failed to compile examples/test_capi.c"
    );

    // 1. Run the standalone C self-test binary (built with -DNDEBUG: its checks must still run).
    let run_status = Command::new(&out_bin).status().unwrap();
    assert!(run_status.success(), "C ABI self-test binary failed");

    // 2. Two-process C <-> Rust lock interop on physical page 0x7C00_0000: hold the page lock from
    //    Rust, spawn `test_capi lock 0x7C000000`, and check it prints "acquired" only after the
    //    Rust guard is released.
    let addr = 0x7C00_0000u64;
    let rust_dev = mmreg_open_addr(addr, 4096, MMREG_O_FORCE | MMREG_O_MOCK);
    assert!(!rust_dev.is_null());
    unsafe {
        assert_eq!(mmreg_lock(rust_dev), 0);
    }

    let mut c_child = Command::new(&out_bin)
        .arg("lock")
        .arg("0x7C000000")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = c_child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let timeout = Duration::from_secs(10);
    assert_eq!(rx.recv_timeout(timeout).unwrap(), "ready");
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "C process must block while the Rust process holds the page flock"
    );

    unsafe {
        assert_eq!(mmreg_unlock(rust_dev), 0);
        mmreg_close(rust_dev);
    }

    assert_eq!(rx.recv_timeout(timeout).unwrap(), "acquired");
    let exit = c_child.wait().unwrap();
    assert!(exit.success(), "C lock interop child process failed");
    let _ = std::fs::remove_file(&out_bin);
}
