//! 托盘图标回执消息转发。
//!
//! 托盘图标在注册时会带来三样东西：宿主窗口 `hWnd`、图标 `uID`、回调消息号
//! `uCallbackMessage`。向宿主窗口投这个消息号，按协议版本编码 wParam/lParam，
//! 就等于"用户在这个图标上按了键/把指针放上去了"——**不用移动系统光标**。
//!
//! 问题在于：第三方程序的回调消息号是私有的（微信是 `WM_APP+0x385`），
//! 系统没有任何 API 能查。这里的做法是把目标进程里的 `NOTIFYICONDATAW` 结构体
//! 从内存里挖出来（`cbSize/hWnd/uID/uFlags/uCallbackMessage/uVersion`），
//! 再用 `Shell_NotifyIconGetRect` 的矩形和我们看到的图标矩形比对确认身份。
//! 结果按 exe 缓存到 `orbit.traycache`，同一个程序只需扫一次。

use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT, TRUE, WPARAM};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
    MEM_RELEASE, PAGE_READWRITE, VIRTUAL_ALLOCATION_TYPE,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Shell::{Shell_NotifyIconGetRect, ShellExecuteW, NOTIFYICONIDENTIFIER};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumThreadWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowThreadProcessId,
    SendMessageW, SendNotifyMessageW, SW_SHOWNORMAL, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

/// sizeof(NOTIFYICONDATAW)，x64
const NID_SIZE: usize = 0x3d0;
/// 悬停打开/关闭（Explorer 对 v3/v4 图标发的通知码，WM_USER+6 / +7）
pub const NIN_POPUPOPEN: u32 = 0x0406;
pub const NIN_POPUPCLOSE: u32 = 0x0407;
/// 左键的"激活"语义（v3/v4 才有）
const NIN_SELECT: u32 = 0x0400;

/// 跨线程传递的裸字段（`Target` 里有 HWND，不能直接 Send）
pub type Raw = (u32, usize, u32, u32, u32);

#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub pid: u32,
    pub hwnd: HWND,
    pub msg: u32,
    pub uid: u32,
    pub ver: u32,
}

impl Target {
    pub fn raw(&self) -> Raw {
        (self.pid, self.hwnd.0 as usize, self.msg, self.uid, self.ver)
    }
    pub fn from_raw(r: Raw) -> Self {
        Self { pid: r.0, hwnd: HWND(r.1 as *mut c_void), msg: r.2, uid: r.3, ver: r.4 }
    }
}

/// WM_CONTEXTMENU：v3/v4 右键时 Explorer 实际发的通知（替代 RBUTTONDOWN/UP）
const WM_CONTEXTMENU: u32 = 0x007B;

fn encode(ver: u32, uid: u32, event: u32, x: i32, y: i32) -> (u32, u32) {
    if ver >= 4 {
        // v4：wParam = 屏幕坐标，lParam 低 16 = 事件、高 16 = 图标 uid
        (((y as u32) << 16) | (x as u32 & 0xFFFF), (uid << 16) | event)
    } else {
        // v0/v3：wParam = uid，lParam = 事件
        (uid, event)
    }
}

/// 给诊断工具用：按版本编码 (wParam, lParam)
#[allow(dead_code)]
pub fn encode_pub(ver: u32, uid: u32, event: u32, x: i32, y: i32) -> (u32, u32) {
    encode(ver, uid, event, x, y)
}

fn send(t: &Target, event: u32, x: i32, y: i32) {
    let (w, l) = encode(t.ver, t.uid, event, x, y);
    unsafe {
        let _ = SendNotifyMessageW(t.hwnd, t.msg, WPARAM(w as usize), LPARAM(l as isize));
    }
}

/// 悬停进入。Explorer 进图标时发的回调事件只有 WM_MOUSEMOVE（WM_MOUSEHOVER 是
/// TrackMouseEvent 在**真窗口**上产生的窗口消息，不是托盘回调里的事件值，不能混用）；
/// v4 且自绘 tooltip 的应用由 NIN_POPUPOPEN 触发弹出。
pub fn hover(t: &Target, center: (i32, i32)) {
    send(t, WM_MOUSEMOVE, center.0, center.1);
    if t.ver >= 4 {
        send(t, NIN_POPUPOPEN, center.0, center.1);
    }
}

/// 悬停期间维持跟踪（真托盘大约 150ms 一条）
pub fn hover_move(t: &Target, center: (i32, i32)) {
    send(t, WM_MOUSEMOVE, center.0, center.1);
}

/// 悬停离开：只有 v4 自绘 tooltip 的应用需要 NIN_POPUPCLOSE 收尾
pub fn leave(t: &Target, center: (i32, i32)) {
    if t.ver >= 4 {
        send(t, NIN_POPUPCLOSE, center.0, center.1);
    }
}

/// 左键。**严格按 Explorer 的真实行为发**：
///   v3/v4 —— 整个单击只发**一条 NIN_SELECT**（微软文档：v3 起 NIN_SELECT/WM_CONTEXTMENU
///            **替代** WM_LBUTTONDOWN/UP）。以前这里连发 DOWN→UP→NIN_SELECT 三条，
///            对"抬起和 SELECT 都切换窗口"的应用等于连续点两次——窗口开了又立刻关
///            （小米电脑管家实测：点击闪退、左右键都弹不出窗口，就是这个原因）。
///   v0-2  —— WM_LBUTTONDOWN + WM_LBUTTONUP；双击第二击 DBLCLK + UP（第一击不重放）。
pub fn click(t: &Target, center: (i32, i32), dbl: bool) {
    if t.msg == 0 || t.msg == u32::MAX {
        // 消息号为 0 等于投 WM_NULL：点了什么都不发生。正常不该出现——真出现说明
        // 图标注册信息没拿全（老版本「占位 ADD 被过滤 + MODIFY 补入库」就是这个坑，
        // 任务管理器的 CPU 图标踩过），这里留痕方便定位。
        crate::tlog!("[traymsg] 丢弃点击：回调消息号无效 msg={} hwnd={:#x}", t.msg, t.hwnd.0 as usize);
        return;
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(t.pid);
    }
    let (x, y) = center;
    crate::tlog!(
        "[traymsg] click v{} msg={:#x} uid={} dbl={} hwnd={:#x}",
        t.ver, t.msg, t.uid, dbl, t.hwnd.0 as usize
    );
    if t.ver >= 3 {
        // 双击的第二击对 v3/v4 来说也只是又一条 NIN_SELECT，应用自己按时间差认双击
        send(t, NIN_SELECT, x, y);
        return;
    }
    if dbl {
        send(t, WM_LBUTTONDBLCLK, x, y);
        send(t, WM_LBUTTONUP, x, y);
        return;
    }
    send(t, WM_LBUTTONDOWN, x, y);
    send(t, WM_LBUTTONUP, x, y);
}

/// 右键。v3/v4 只发一条 WM_CONTEXTMENU（Explorer 的真实行为）；v0-2 发 DOWN + UP。
pub fn right_click(t: &Target, center: (i32, i32)) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(t.pid);
    }
    let (x, y) = center;
    crate::tlog!(
        "[traymsg] right_click v{} msg={:#x} uid={} hwnd={:#x}",
        t.ver, t.msg, t.uid, t.hwnd.0 as usize
    );
    if t.ver >= 3 {
        send(t, WM_CONTEXTMENU, x, y);
        return;
    }
    send(t, WM_RBUTTONDOWN, x, y);
    send(t, WM_RBUTTONUP, x, y);
}


// ---------- 从 explorer 的 TRAYDATA 读注册信息 ----------
//
// 托盘图标注册时 explorer 会为每个图标留一份 TRAYDATA（hWnd / uID / uCallbackMessage /
// uVersion / guid / szTip / exe），就在托盘工具栏的按钮 dwData 上。用 TB_GETBUTTON 把这个
// 按钮结构拿过来（缓冲区开在 explorer 进程里），再 ReadProcessMemory 读 TRAYDATA，
// 就拿到了**准确的**回调消息号和协议版本，不用猜、也不用扫目标进程内存。
// 这也是 ManagedShell 在非 shell 场景下的做法（Orbit 的托盘点击/悬停就靠它）。

const TB_GETBUTTON: u32 = 0x0417;
const TB_BUTTONCOUNT: u32 = 0x0418;
/// sizeof(TRAYDATA)，x64
const TRAYDATA_SIZE: usize = 1112;
/// sizeof(TBBUTTON)，x64（dwData 在 +16）
const TBBUTTON_SIZE: usize = 32;

/// 一条托盘注册信息
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TrayEntry {
    pub hwnd: HWND,
    pub uid: u32,
    pub msg: u32,
    pub ver: u32,
    pub guid: GUID,
    pub tip: String,
    pub exe: String,
}

/// 找托盘工具栏：Win10 在 SysPager 里，Win11 可能挂在 TrayNotifyWnd 下，
/// 找不到就向下枚举一层兜底。
fn tray_toolbar() -> Option<HWND> {
    unsafe {
        let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        let notify = FindWindowExW(tray, None, w!("TrayNotifyWnd"), PCWSTR::null()).ok()?;
        if let Ok(pager) = FindWindowExW(notify, None, w!("SysPager"), PCWSTR::null()) {
            if let Ok(tb) = FindWindowExW(pager, None, w!("ToolbarWindow32"), PCWSTR::null()) {
                return Some(tb);
            }
        }
        // 有的版本没有 SysPager
        let mut found = HWND::default();
        let ctx = (&mut found) as *mut HWND as isize;
        let _ = EnumChildWindows(
            notify,
            Some(find_toolbar),
            LPARAM(ctx),
        );
        if found.0.is_null() {
            None
        } else {
            Some(found)
        }
    }
}

unsafe extern "system" fn find_toolbar(h: HWND, l: LPARAM) -> BOOL {
    let mut b = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut b) };
    if String::from_utf16_lossy(&b[..n as usize]) == "ToolbarWindow32" {
        unsafe { *(l.0 as *mut HWND) = h };
        return BOOL(0);
    }
    TRUE
}

/// 读出所有托盘图标的注册信息（含回调消息号与版本）
pub fn tray_items() -> Vec<TrayEntry> {
    let mut out = Vec::new();
    let Some(tb) = tray_toolbar() else { return out };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(tb, Some(&mut pid)) };
    if pid == 0 {
        return out;
    }
    let access = PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;
    let Ok(proc) = (unsafe { OpenProcess(access, false, pid) }) else { return out };
    let count = unsafe { SendMessageW(tb, TB_BUTTONCOUNT, WPARAM(0), LPARAM(0)) }.0 as i32;
    let size = TRAYDATA_SIZE.max(TBBUTTON_SIZE);
    // MEM_COMMIT | MEM_RESERVE = 0x3000
    let buf = unsafe { VirtualAllocEx(proc, None, size, VIRTUAL_ALLOCATION_TYPE(0x3000), PAGE_READWRITE) };
    if buf.is_null() {
        unsafe {
            let _ = CloseHandle(proc);
        }
        return out;
    }
    let mut raw = vec![0u8; size];
    for i in 0..count.max(0) {
        let got = unsafe { SendMessageW(tb, TB_GETBUTTON, WPARAM(i as usize), LPARAM(buf as isize)) };
        if got.0 == 0 {
            continue;
        }
        if read_mem(proc, buf as usize, &mut raw[..TBBUTTON_SIZE]).is_err() {
            continue;
        }
        let dw = u64::from_le_bytes(raw[16..24].try_into().unwrap());
        if dw == 0 {
            continue;
        }
        if read_mem(proc, dw as usize, &mut raw[..TRAYDATA_SIZE]).is_err() {
            continue;
        }
        let r = &raw[..TRAYDATA_SIZE];
        let hwnd = HWND(u64::from_le_bytes(r[0..8].try_into().unwrap()) as *mut c_void);
        if hwnd.0.is_null() {
            continue;
        }
        let uid = u32::from_le_bytes(r[8..12].try_into().unwrap());
        let msg = u32::from_le_bytes(r[12..16].try_into().unwrap());
        let ver = u32::from_le_bytes(r[20..24].try_into().unwrap());
        let exe = wide_to_string(&r[48..48 + 520]).to_lowercase();
        let tip = wide_to_string(&r[568..568 + 520]);
        let guid = GUID {
            data1: u32::from_le_bytes(r[1092..1096].try_into().unwrap()),
            data2: u16::from_le_bytes(r[1096..1098].try_into().unwrap()),
            data3: u16::from_le_bytes(r[1098..1100].try_into().unwrap()),
            data4: r[1100..1108].try_into().unwrap(),
        };
        out.push(TrayEntry { hwnd, uid, msg, ver, guid, tip, exe });
    }
    unsafe {
        let _ = VirtualFreeEx(proc, buf, 0, MEM_RELEASE);
        let _ = CloseHandle(proc);
    }
    out
}

fn read_mem(proc: HANDLE, addr: usize, buf: &mut [u8]) -> Result<(), ()> {
    let mut got = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            proc,
            addr as *const c_void,
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
            Some(&mut got),
        )
    };
    if ok.is_ok() && got == buf.len() { Ok(()) } else { Err(()) }
}

fn wide_to_string(b: &[u8]) -> String {
    let mut v: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    if v.is_empty() {
        return String::new();
    }
    String::from_utf16_lossy(&mut v)
}

/// 在一堆候选窗口里找"这个进程注册过的托盘图标"：枚举窗口 × uid 0..4 调
/// `Shell_NotifyIconGetRect`，能查到矩形的就是真注册过的。
pub fn find_icons(pid: u32) -> Vec<(HWND, u32, RECT, String)> {
    let mut out = Vec::new();
    for (h, class) in windows_of_pid(pid) {
        for uid in 0..4u32 {
            let id = NOTIFYICONIDENTIFIER {
                cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: h,
                uID: uid,
                guidItem: GUID::default(),
            };
            if let Ok(r) = unsafe { Shell_NotifyIconGetRect(&id) } {
                if r.right > r.left {
                    out.push((h, uid, r, class.clone()));
                }
            }
        }
    }
    out
}

/// 这个图标现在在屏幕上的中心（拿它当悬停坐标：应用会校验指针是否落在图标附近）
#[allow(dead_code)]
pub fn icon_center_of(t: Target) -> Option<(i32, i32)> {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: t.hwnd,
        uID: t.uid,
        guidItem: GUID::default(),
    };
    let r = unsafe { Shell_NotifyIconGetRect(&id) }.ok()?;
    Some(((r.left + r.right) / 2, (r.top + r.bottom) / 2))
}

/// 用 TRAYDATA 里的信息认出"就是屏幕上这个图标"，并给出准确的回调目标
pub fn from_tray(icon: RECT) -> Option<Target> {
    for e in tray_items() {
        if e.msg < 0x0400 || e.msg > 0xFFFF {
            continue;
        }
        let id = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: e.hwnd,
            uID: e.uid,
            guidItem: e.guid,
        };
        let Ok(r) = (unsafe { Shell_NotifyIconGetRect(&id) }) else { continue };
        if !near((r.left, r.top, r.right, r.bottom), icon) {
            continue;
        }
        let ver = if e.ver == 3 || e.ver < 3 { if e.ver == 0 { 4 } else { e.ver } } else { 4 };
        return Some(Target { pid: 0, hwnd: e.hwnd, msg: e.msg, uid: e.uid, ver });
    }
    None
}

// ---------- 找宿主进程 ----------

/// exe（小写全路径）→ 拥有托盘消息窗口的进程
pub fn host_pids(exe: &str) -> Vec<u32> {
    if exe.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for pid in pids_of_exe(exe) {
        if find_icons(pid).is_empty() {
            continue;
        }
        out.push(pid);
    }
    out
}

fn pids_of_exe(exe: &str) -> Vec<u32> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let name = String::from_utf16_lossy(&pe.szExeFile);
                let name = name.trim_end_matches('\0');
                if !name.is_empty() && exe.ends_with(&name.to_lowercase()) {
                    out.push(pe.th32ProcessID);
                }
                pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

pub fn windows_of_pid(pid: u32) -> Vec<(HWND, String)> {
    let mut out: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return out };
        let mut te = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        if Thread32First(snap, &mut te).is_ok() {
            loop {
                if te.th32OwnerProcessID == pid {
                    let mut v: Vec<(HWND, String)> = Vec::new();
                    let _ = EnumThreadWindows(
                        te.th32ThreadID,
                        Some(enum_thread),
                        LPARAM(&mut v as *mut _ as isize),
                    );
                    for w in v {
                        if !out.iter().any(|(h, _)| *h == w.0) {
                            out.push(w);
                        }
                    }
                }
                te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut te).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

unsafe extern "system" fn enum_thread(h: HWND, l: LPARAM) -> BOOL {
    let v = &mut *(l.0 as *mut Vec<(HWND, String)>);
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) };
    v.push((h, String::from_utf16_lossy(&b[..n as usize])));
    TRUE
}

// ---------- 缓存 ----------

fn cache_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("orbit.traycache")))
        .unwrap_or_else(|| std::path::PathBuf::from("orbit.traycache"))
}

fn cache_load(exe: &str) -> Option<(u32, u32, u32)> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    for line in text.lines() {
        let mut it = line.split('|');
        if it.next() == Some(exe) {
            let msg = it.next()?.parse().ok()?;
            let uid = it.next()?.parse().ok()?;
            let ver = it.next()?.parse().ok()?;
            return Some((msg, uid, ver));
        }
    }
    None
}

fn cache_save(exe: &str, msg: u32, uid: u32, ver: u32) {
    let path = cache_path();
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let keep: Vec<&str> = text.lines().filter(|l| !l.starts_with(&format!("{exe}|"))).collect();
    text = keep.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("{exe}|{msg}|{uid}|{ver}\n"));
    let _ = std::fs::write(path, text);
    crate::vlog!("[traymsg] 记录 {exe} 回调消息 {msg:#x}");
}

/// 为一个图标找它的回调目标：先查缓存，再扫一次内存。
/// `icon` 是 UIA 给出的图标屏幕矩形，用来在多个注册项里认出"就是这一个"。
pub fn target(exe: &str, icon: RECT, pid_hint: u32) -> Option<Target> {
    let pids = {
        let h = host_pids(exe);
        if h.is_empty() {
            if pid_hint == 0 { return None } else { vec![pid_hint] }
        } else {
            h
        }
    };
    crate::vlog!(
        "[traymsg] 查 {exe} icon=({},{},{},{}) pids={pids:?}",
        icon.left, icon.top, icon.right, icon.bottom
    );
    // 首选：explorer 的 TRAYDATA 里就有准确的回调消息号与版本，毫秒级
    if let Some(mut t) = from_tray(icon) {
        crate::vlog!("[traymsg] TRAYDATA 命中 msg={:#x} v{}", t.msg, t.ver);
        t.pid = unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(t.hwnd, Some(&mut pid));
            pid
        };
        return Some(t);
    }
    // 缓存里的消息号 + 现枚举到的宿主窗口：不用再扫内存
    if let Some((msg, _cached_uid, ver)) = cache_load(exe) {
        crate::vlog!("[traymsg] 缓存命中 msg={msg:#x} v{ver}，按现枚举窗口校验");
        for &pid in &pids {
            for (hwnd, uid) in candidates(pid) {
                let t = Target { pid, hwnd, msg, uid, ver };
                if matches(t, icon) {
                    return Some(t);
                }
            }
        }
    }
    for pid in pids {
        let cands = candidates(pid);
        crate::vlog!("[traymsg] pid={pid} 候选窗口 {} 个 {:?}", cands.len(), cands);
        if cands.is_empty() {
            continue;
        }
        if let Some(t) = scan(pid, icon, &cands) {
            cache_save(exe, t.msg, t.uid, t.ver);
            return Some(t);
        }
    }
    None
}

/// 这个进程里"确实注册过托盘图标"的 (宿主窗口, uid)
fn candidates(pid: u32) -> Vec<(HWND, u32)> {
    find_icons(pid).into_iter().map(|(h, u, _, _)| (h, u)).collect()
}

fn matches(t: Target, icon: RECT) -> bool {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: t.hwnd,
        uID: t.uid,
        guidItem: GUID::default(),
    };
    match unsafe { Shell_NotifyIconGetRect(&id) } {
        Ok(r) => near((r.left, r.top, r.right, r.bottom), icon),
        Err(_) => false,
    }
}

/// 判断"UIA 里的这个图标"是不是"注册项返回的这个矩形"。
///
/// 两边不是同一种矩形：`Shell_NotifyIconGetRect` 给的是**按钮**（32×48，含内边距），
/// UIA 给的是按钮里那个 **16×16 的字形**。以前拿左上角比 ±4，永远对不上——
/// 于是每次悬停都"找不到回调目标"，转发根本没发出去。
fn near(a: (i32, i32, i32, i32), b: RECT) -> bool {
    // 同类型矩形：左上角对齐
    if (a.0 - b.left).abs() <= 8 && (a.1 - b.top).abs() <= 8 {
        return true;
    }
    // 字形 ⊂ 按钮：字形中心落在按钮矩形里（各边留 8px 余量）就算同一个
    let cx = (b.left + b.right) / 2;
    let cy = (b.top + b.bottom) / 2;
    cx >= a.0 - 8 && cx <= a.2 + 8 && cy >= a.1 - 8 && cy <= a.3 + 8
}

// ---------- 内存扫描 ----------

fn scan(pid: u32, icon: RECT, cands: &[(HWND, u32)]) -> Option<Target> {
    let map: HashMap<u64, (HWND, u32)> = cands.iter().map(|(h, u)| (h.0 as usize as u64, (*h, *u))).collect();
    if map.is_empty() {
        return None;
    }
    let proc: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()? };
    let mut best: Option<Target> = None;
    for deep in [false, true] {
        best = scan_pass(proc, &map, pid, icon, deep);
        if best.is_some() {
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(proc);
    }
    best
}

/// `deep = false` 只扫私有堆段（结构体正常都在这里，快 3~5 倍）；
/// `deep = true` 把其余已提交可读段也扫一遍，防止有的程序把结构体放在别处。
fn scan_pass(
    proc: HANDLE,
    map: &HashMap<u64, (HWND, u32)>,
    pid: u32,
    icon: RECT,
    deep: bool,
) -> Option<Target> {
    let mut best: Option<Target> = None;
    let mut addr: usize = 0x10000;
    let max = 0x7fff_ffef_ffffusize;
    let mut buf = vec![0u8; 4 << 20];
    while addr < max && best.is_none() {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let n = unsafe {
            VirtualQueryEx(
                proc,
                Some(addr as *const c_void),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if n == 0 {
            break;
        }
        let base = mbi.BaseAddress as usize;
        let size = mbi.RegionSize;
        if size == 0 {
            break;
        }
        let prot = mbi.Protect.0;
        // 只扫私有可读的小段（结构体在堆上）：跳过映像/映射和超大保留区，省掉大半时间
        let heapish = mbi.Type.0 == 0x20000 && size <= (64 << 20);
        let take = deep || heapish;
        if mbi.State == MEM_COMMIT && prot != 0x01 && (prot & 0x100) == 0 && take {
            let mut off = 0usize;
            while off < size {
                let want = (size - off).min(buf.len());
                let mut got = 0usize;
                let ok = unsafe {
                    ReadProcessMemory(
                        proc,
                        (base + off) as *const c_void,
                        buf.as_mut_ptr() as *mut c_void,
                        want,
                        Some(&mut got),
                    )
                };
                if ok.is_ok() && got >= 64 {
                    if let Some(t) = scan_chunk(&buf[..got], &map, pid, icon) {
                        best = Some(t);
                        break;
                    }
                }
                off += want.max(1);
            }
        }
        let next = base.saturating_add(size);
        if next <= addr {
            break;
        }
        addr = next;
    }
    best
}

fn scan_chunk(b: &[u8], map: &HashMap<u64, (HWND, u32)>, pid: u32, icon: RECT) -> Option<Target> {
    let mut i = 8;
    while i + NID_SIZE <= b.len() {
        let v = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if let Some(&(hwnd, _want_uid)) = map.get(&v) {
            let s = &b[i - 8..i - 8 + NID_SIZE];
            let cb = u32::from_le_bytes(s[0..4].try_into().unwrap());
            let uid = u32::from_le_bytes(s[16..20].try_into().unwrap());
            let flags = u32::from_le_bytes(s[20..24].try_into().unwrap());
            let msg = u32::from_le_bytes(s[24..28].try_into().unwrap());
            if (0x150..=0x500).contains(&cb)
                && (flags & 0x1) != 0
                && (0x0400..=0xFFFF).contains(&msg)
                && uid < 0x1000
            {
                // 版本在 +816（uTimeout/uVersion 联合体）：4 = 坐标/uid 分列，3 及以下 = uid/事件。
                // 这份结构体可能是注册后又被复用的旧拷贝，字段不一定还准，所以只认 3/4，
                // 其余一律按 Win11 的 v4 走。
                let ver = u32::from_le_bytes(s[816..820].try_into().unwrap());
                let ver = if ver == 3 { 3 } else { 4 };
                let t = Target { pid, hwnd, msg, uid, ver };
                if matches(t, icon) {
                    return Some(t);
                }
            }
        }
        i += 8;
    }
    None
}

// ---------- 任务管理器（elevated）专用唤醒 ----------
//
// 任务管理器清单里写的是 `requestedExecutionLevel level="highestAvailable"`，
// 管理员账户下它以 High 完整性级别运行，而 Orbit 是 Medium。跨完整性级别实测：
//   - 给它投私有消息（`uCallbackMessage`）会被 UIPI 静默丢弃，
//     `PostMessage` 直接返回 `ACCESS_DENIED`；
//   - `ShowWindow` / `ShowWindowAsync` / `SetForegroundWindow` 全都没有效果，
//     最小化的窗口纹丝不动。
// 唯一可靠的路径是**再启动一次 taskmgr.exe**：它是单实例，新进程会立刻自己退出，
// 同时把已存在的那个实例的窗口显示到前台——和点开始菜单里的「任务管理器」完全一样。
// 实测：收进托盘（iconic + 隐藏）的任务管理器，再启动一次就回来了。

/// 这个托盘图标是不是任务管理器（隐藏到托盘时 tooltip 是「CPU xx%」）
pub fn is_taskmgr(exe: &str, name: &str) -> bool {
    let e = exe.to_ascii_lowercase();
    if e.ends_with("\\taskmgr.exe") || e == "taskmgr.exe" {
        return true;
    }
    name == "任务管理器" || name.to_ascii_uppercase().starts_with("CPU ")
}

/// 把任务管理器叫回来（跨完整性级别说明见上）
pub fn wake_taskmgr() {
    let r = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("taskmgr.exe"),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    crate::tlog!("[traymsg] 唤起任务管理器 ShellExecute={} ok={}", r.0 as isize, r.0 as isize > 32);
}
