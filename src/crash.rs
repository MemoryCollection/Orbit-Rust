//! 原生崩溃现场记录（`AddVectoredExceptionHandler`）。
//!
//! 目的：定位"无 Rust panic 日志、无退出标记"的进程消失——访问违规(0xC0000005)、
//! 栈溢出(0xC00000FD)、fail-fast(0xC0000409，WinRT 未处理异常的典型标志)都不会走
//! `std::panic::set_hook`。VEH 在异常第一时间触发，把异常码/地址/线程 id 追加写到
//! `orbit-crash.log`，然后返回 `EXCEPTION_CONTINUE_SEARCH`，完全不改变原有崩溃流程。
//!
//! handler 里只做"无锁 + 定长栈缓冲 + WriteFile"，不调用任何会分配或加锁的 Rust 代码，
//! 避免栈溢出/持有锁时二次崩溃。

use core::ffi::c_void;
use std::os::windows::io::IntoRawHandle;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

/// 异常记录结构（EXCEPTION_RECORD / EXCEPTION_POINTERS 的稳定 ABI 布局）。
/// windows crate 的 EXCEPTION_POINTERS 需要 `Win32_System_Kernel` feature，这里自建避免改依赖。
#[repr(C)]
struct ExceptionRecord {
    exception_code: i32,
    exception_flags: u32,
    exception_record: *mut c_void,
    exception_address: *mut c_void,
    number_parameters: u32,
    exception_information: [usize; 15],
}

#[repr(C)]
struct ExceptionPointers {
    exception_record: *mut ExceptionRecord,
    context_record: *mut c_void,
}

static LOG_HANDLE: AtomicIsize = AtomicIsize::new(0);
static IN_HANDLER: AtomicBool = AtomicBool::new(false);

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

unsafe extern "system" fn on_exception(info: *mut ExceptionPointers) -> i32 {
    if info.is_null() || (*info).exception_record.is_null() {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let r = &*(*info).exception_record;
    // OutputDebugString 产生的调试异常，量大且非致命，直接忽略
    let code = r.exception_code as u32;
    if code == 0x40010006 || code == 0x4001000A {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // 重入保护：handler 内部若再触发异常（例如栈溢出时栈已耗尽），直接放行
    if IN_HANDLER.swap(true, Ordering::Relaxed) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    {
        let h = LOG_HANDLE.load(Ordering::Relaxed) as *mut c_void;
        if !h.is_null() {
            let mut buf = [0u8; 192];
            let mut n = 0usize;
            n = push(n, &mut buf, b"pid=");
            n = push_dec(n, &mut buf, GetCurrentProcessId() as usize);
            n = push(n, &mut buf, b" code=0x");
            n = push_hex(n, &mut buf, code as usize);
            n = push(n, &mut buf, b" addr=0x");
            n = push_hex(n, &mut buf, r.exception_address as usize);
            n = push(n, &mut buf, b" tid=");
            n = push_dec(n, &mut buf, GetCurrentThreadId() as usize);
            if code == 0xC0000005 && r.number_parameters >= 2 {
                n = push(n, &mut buf, b" flags=");
                n = push_dec(n, &mut buf, r.exception_information[0]);
                n = push(n, &mut buf, b" fault=0x");
                n = push_hex(n, &mut buf, r.exception_information[1]);
            }
            n = push(n, &mut buf, b"\n");
            let mut written = 0u32;
            let _ = WriteFile(HANDLE(h), Some(&buf[..n]), Some(&mut written), None);
        }
    }
    IN_HANDLER.store(false, Ordering::Relaxed);
    EXCEPTION_CONTINUE_SEARCH
}

fn push(mut n: usize, buf: &mut [u8; 192], s: &[u8]) -> usize {
    for &b in s {
        if n < buf.len() {
            buf[n] = b;
            n += 1;
        }
    }
    n
}

fn push_hex(mut n: usize, buf: &mut [u8; 192], v: usize) -> usize {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut shifted = v;
    let mut digits = [0u8; 16];
    let mut cnt = 0usize;
    loop {
        digits[cnt] = H[(shifted & 0xF) as usize];
        cnt += 1;
        shifted >>= 4;
        if shifted == 0 || cnt == 16 {
            break;
        }
    }
    while cnt > 0 {
        cnt -= 1;
        if n < buf.len() {
            buf[n] = digits[cnt];
            n += 1;
        }
    }
    n
}

fn push_dec(mut n: usize, buf: &mut [u8; 192], v: usize) -> usize {
    let mut shifted = v;
    let mut digits = [0u8; 20];
    let mut cnt = 0usize;
    loop {
        digits[cnt] = b'0' + (shifted % 10) as u8;
        cnt += 1;
        shifted /= 10;
        if shifted == 0 || cnt == 20 {
            break;
        }
    }
    while cnt > 0 {
        cnt -= 1;
        if n < buf.len() {
            buf[n] = digits[cnt];
            n += 1;
        }
    }
    n
}

/// 主进程启动时尽早调用一次。失败不影响程序运行。
pub fn install() {
    unsafe extern "system" {
        fn AddVectoredExceptionHandler(
            first: u32,
            handler: Option<unsafe extern "system" fn(*mut ExceptionPointers) -> i32>,
        ) -> *mut c_void;
    }

    unsafe {
        // 日志文件放在 exe 同目录，与 orbit.log 并列
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        if let Some(dir) = dir {
            let path = dir.join("orbit-crash.log");
            if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                LOG_HANDLE.store(f.into_raw_handle() as isize, Ordering::SeqCst);
            }
        }
        AddVectoredExceptionHandler(1, Some(on_exception));
    }
}

/// 供各后台线程打印自身 tid，便于和 crash 日志对照定位是哪个线程崩的。
pub fn current_thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}
