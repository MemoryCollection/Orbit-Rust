//! 极简 HTTP(S) GET（WinHTTP，自动走系统代理）。只取文本，单次请求，不重试。

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
};

const MAX_BODY: usize = 2 * 1024 * 1024;

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
                    let mut body: Vec<u8> = Vec::new();
                    loop {
                        let mut avail = 0u32;
                        if WinHttpQueryDataAvailable(req, &mut avail).is_err() || avail == 0 {
                            break;
                        }
                        let mut buf = vec![0u8; avail as usize];
                        let mut read = 0u32;
                        if WinHttpReadData(req, buf.as_mut_ptr().cast(), avail, &mut read).is_err()
                            || read == 0
                        {
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
            },
        };

        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        result
    }
}
