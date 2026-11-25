//! 极简 HTTP(S) GET（WinHTTP，自动走系统代理）。只取文本，单次请求，不重试。

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

const MAX_BODY: usize = 2 * 1024 * 1024;

/// `WINHTTP_HEADER_NAME_BY_INDEX` / `WINHTTP_NO_HEADER_INDEX`。
/// 新版 Windows SDK（10.0.26100）里这两个宏都是 `NULL`；旧 SDK 曾写成 `(PVOID)-1`，
/// 但当前 winhttp.dll 会直接解引用传入的 name 指针，传 -1 会以
/// `0xC0000005 fault=0xffffffffffffffff` 原生崩溃（不走 Rust panic hook）。
/// windows crate 未导出这两个宏，这里按当前 SDK 的值补上。
const BY_INDEX: PCWSTR = PCWSTR::null();

/// 循环读光响应体（UTF-8 lossy）；空响应报错
unsafe fn read_body(req: *mut core::ffi::c_void) -> Result<String, String> {
    let mut body: Vec<u8> = Vec::new();
    loop {
        let mut avail = 0u32;
        if WinHttpQueryDataAvailable(req, &mut avail).is_err() || avail == 0 {
            break;
        }
        let mut buf = vec![0u8; avail as usize];
        let mut read = 0u32;
        if WinHttpReadData(req, buf.as_mut_ptr().cast(), avail, &mut read).is_err() || read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read as usize]);
        if body.len() > MAX_BODY {
            break;
        }
    }
    if body.is_empty() {
        Err("空响应".into())
    } else {
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

/// GET 一个 https URL，返回响应体文本（UTF-8 lossy）。任何失败都返回 Err(原因)。
pub fn get(url: &str, timeout_ms: i32) -> Result<String, String> {
    let rest = url.strip_prefix("https://").ok_or("只支持 https")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host_h = HSTRING::from(host);
    let path_h = HSTRING::from(path);
    let agent_h = HSTRING::from("Orbit/0.1");

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(agent_h.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHttpOpen 失败".into());
        }
        let _ = WinHttpSetTimeouts(session, timeout_ms, timeout_ms, timeout_ms, timeout_ms);

        let connect = WinHttpConnect(session, PCWSTR(host_h.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return Err(format!("连接 {host} 失败"));
        }

        let req = WinHttpOpenRequest(
            connect,
            w!("GET"),
            PCWSTR(path_h.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return Err("创建请求失败".into());
        }

        let result = match WinHttpSendRequest(req, None, None, 0, 0, 0) {
            Err(e) => Err(format!("SendRequest: {e}")),
            Ok(_) => match WinHttpReceiveResponse(req, std::ptr::null_mut()) {
                Err(e) => Err(format!("ReceiveResponse: {e}")),
                Ok(_) => {
                    // 先看状态码：非 2xx 的响应体多半是错误页（代理劫持 / 404 等），
                    // 不检查的话调用方会把 HTML 当文本解析，报出莫名其妙的错
                    let mut status = 0u32;
                    let mut st_size = std::mem::size_of::<u32>() as u32;
                    let st_ok = WinHttpQueryHeaders(
                        req,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        BY_INDEX,
                        Some(&mut status as *mut u32 as *mut core::ffi::c_void),
                        &mut st_size,
                        std::ptr::null_mut(),
                    )
                    .is_ok();
                    if st_ok && !(200..300).contains(&status) {
                        Err(format!("HTTP {status}"))
                    } else {
                        read_body(req)
                    }
                }
            },
        };

        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore] // 走真实网络，cargo test 默认跳过；用 -- --ignored 手动跑
    fn live_get_github_version() {
        let r = super::get("https://www.baidu.com/", 8000);
        println!("result: {r:?}");
        assert!(r.is_ok());
    }
}
