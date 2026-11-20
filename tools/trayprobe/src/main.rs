//! 托盘图标回调探针：从目标进程内存里把注册的 `NOTIFYICONDATAW` 挖出来
//! （拿到 `hWnd` / `uID` / `uCallbackMessage` / `uVersion` / `guidItem`），
//! 再用 `Shell_NotifyIconGetRect` 取图标矩形，最后按协议版本试发鼠标事件。
//!
//! 用法：
//!   trayprobe scan <pid>          只看扫到了什么
//!   trayprobe try  <pid>          扫到之后按 v4 发鼠标事件，看目标有没有反应
//!   trayprobe send <hwnd> <msg> <uid> <ver> <event> <x> <y>

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use windows::core::GUID;
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::Shell::{Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    SendNotifyMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP,
};

/// sizeof(NOTIFYICONDATAW) on x64
const NID_SIZE: usize = 0x3d0;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: trayprobe scan|try <pid>  |  trayprobe send <hwnd> <msg> <uid> <ver> <event> <x> <y>");
        return;
    }
    match args[1].as_str() {
        "scan" | "try" => {
            let pid: u32 = args[2].parse().expect("pid");
            let cands = discover(pid);
            if cands.is_empty() {
                println!("(没扫到 NOTIFYICONDATA)");
                return;
            }
            let mut seen = HashSet::new();
            println!("\n== 候选 ==");
            let mut chosen: Vec<(Cand, GUID, u32)> = Vec::new();
            for c in cands {
                if !seen.insert((c.hwnd.0 as usize, c.uid, c.msg)) {
                    continue;
                }
                let nid = read_nid(pid, c.addr);
                let guid = nid.as_ref().map(|n| n.guid).unwrap_or_default();
                let ver = nid.as_ref().map(|n| n.ver).unwrap_or(0);
                let itip = nid.as_ref().map(|n| n.tip.clone()).unwrap_or_default();
                println!(
                    "hwnd={:#x} class={:<44} uid={:<3} flags={:#04x} cb={:#05x} msg={:#06x} ver={} tip={:?} guid={:?} rect={:?}",
                    c.hwnd.0 as usize,
                    c.class,
                    c.uid,
                    c.flags,
                    c.cb,
                    c.msg,
                    ver,
                    itip,
                    guid,
                    icon_rect(c.hwnd, c.uid, guid),
                );
                chosen.push((c, guid, ver));
            }
            if args[1] == "try" {
                for (c, guid, ver) in &chosen {
                    try_events(c, *guid, *ver);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }
        }
        "send" => {
            let h: usize = usize::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("hwnd");
            let hwnd = HWND(h as *mut c_void);
            let (msg, uid, ver, event) = (num(&args[3]), num(&args[4]), num(&args[5]), num(&args[6]));
            let (x, y): (i32, i32) = (args[7].parse().unwrap(), args[8].parse().unwrap());
            let (w, l) = encode(ver, uid, event, x, y);
            let ok = unsafe { SendNotifyMessageW(hwnd, msg, WPARAM(w as usize), LPARAM(l as isize)) };
            println!("SendNotifyMessage({h:#x}, {msg:#x}, w={w:#x}, l={l:#x}) -> {ok:?}");
        }
        // 对照实验：真的把光标放到图标上（600ms 后还原）
        "hoverreal" => {
            let pid: u32 = args[2].parse().expect("pid");
            for c in discover(pid) {
                let nid = read_nid(pid, c.addr);
                let guid = nid.as_ref().map(|n| n.guid).unwrap_or_default();
                let Some((l, t, r, b)) = icon_rect(c.hwnd, c.uid, guid) else { continue };
                let (x, y) = ((l + r) / 2, (t + b) / 2);
                let mut old = windows::Win32::Foundation::POINT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut old);
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y);
                }
                let before = all_windows(pid);
                std::thread::sleep(std::time::Duration::from_millis(700));
                let new: Vec<_> = all_windows(pid).difference(&before).cloned().collect();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(old.x, old.y);
                }
                println!("真实悬停 @({x},{y}) 0.7s → 新窗口 {new:?}");
                break;
            }
        }
        "close" => {
            let h: usize = usize::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("hwnd");
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    HWND(h as *mut c_void),
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            println!("已发 WM_CLOSE 给 {h:#x}");
        }
        "ev" => {
            // ev <pid> <event> [wparam覆盖]
            let pid: u32 = args[2].parse().expect("pid");
            let ev = num(&args[3]);
            let wparam: Option<u32> = args.get(4).map(|s| num(s));
            for c in discover(pid) {
                let nid = read_nid(pid, c.addr);
                let guid = nid.as_ref().map(|n| n.guid).unwrap_or_default();
                let ver = nid.as_ref().map(|n| n.ver).unwrap_or(4);
                let Some((l, t, r, b)) = icon_rect(c.hwnd, c.uid, guid) else { continue };
                let (x, y) = ((l + r) / 2, (t + b) / 2);
                let before = all_windows(pid);
                let (w, l2) = encode(ver, c.uid, ev, x, y);
                let w = wparam.unwrap_or(w);
                unsafe {
                    let _ = SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
                }
                std::thread::sleep(std::time::Duration::from_millis(600));
                let new: Vec<_> = all_windows(pid).difference(&before).cloned().collect();
                println!("ev={ev:#x} w={w:#x} l={l2:#x} → {new:?}");
                break;
            }
        }
        "lclick" | "rclick" => {
            let pid: u32 = args[2].parse().expect("pid");
            let right = args[1] == "rclick";
            for c in discover(pid) {
                let nid = read_nid(pid, c.addr);
                let guid = nid.as_ref().map(|n| n.guid).unwrap_or_default();
                let ver = nid.as_ref().map(|n| n.ver).unwrap_or(4);
                let Some((l, t, r, b)) = icon_rect(c.hwnd, c.uid, guid) else { continue };
                let (x, y) = ((l + r) / 2, (t + b) / 2);
                let before = all_windows(pid);
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
                }
                let (down, up) = if right { (WM_RBUTTONDOWN, WM_RBUTTONUP) } else { (WM_LBUTTONDOWN, WM_LBUTTONUP) };
                for ev in [down, up] {
                    let (w, l2) = encode(ver, c.uid, ev, x, y);
                    unsafe {
                        let _ = SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
                if !right && ver >= 3 {
                    let (w, l2) = encode(ver, c.uid, 0x0400, x, y);
                    unsafe {
                        let _ = SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(800));
                let new: Vec<_> = all_windows(pid).difference(&before).cloned().collect();
                let mut fg = HWND::default();
                unsafe { fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
                println!("[{:#x}] {} → 新窗口 {new:?} 前台={:#x} {:?}", c.hwnd.0 as usize, args[1], fg.0 as usize, tip_of(fg));
                break;
            }
        }
        "move" => {
            let pid: u32 = args[2].parse().expect("pid");
            for c in discover(pid) {
                let nid = read_nid(pid, c.addr);
                let guid = nid.as_ref().map(|n| n.guid).unwrap_or_default();
                let Some((l, t, r, b)) = icon_rect(c.hwnd, c.uid, guid) else { continue };
                let (x, y) = ((l + r) / 2, (t + b) / 2);
                for &(name, ver) in &[("v4", 4u32), ("v3", 3u32)] {
                    let before = all_windows(pid);
                    let (w, l2) = encode(ver, c.uid, WM_MOUSEMOVE, x, y);
                    unsafe {
                        let _ = SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    let after = all_windows(pid);
                    let new: Vec<_> = after.difference(&before).cloned().collect();
                    println!("[{name}] msg={:#x} uid={} @({x},{y}) → 新窗口 {new:?}", c.msg, c.uid);
                }
            }
        }
        "shot" => {
            let (x, y, w, h): (i32, i32, i32, i32) = (
                args[2].parse().unwrap(),
                args[3].parse().unwrap(),
                args[4].parse().unwrap(),
                args[5].parse().unwrap(),
            );
            shot(x, y, w, h, &args[6]);
        }
        other => eprintln!("未知模式 {other}"),
    }
}

/// 按协议版本打包 wParam/lParam：v4 用坐标+uid，v0/v3 用 uid+事件
fn encode(ver: u32, uid: u32, event: u32, x: i32, y: i32) -> (u32, u32) {
    if ver >= 4 {
        (((y as u32) << 16) | (x as u32 & 0xFFFF), (uid << 16) | event)
    } else {
        (uid, event)
    }
}

fn num(s: &str) -> u32 {
    match s.trim().strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).expect("hex"),
        None => s.trim().parse().expect("dec"),
    }
}

// ---------- 内存扫描 ----------

struct Cand {
    hwnd: HWND,
    class: String,
    cb: u32,
    uid: u32,
    flags: u32,
    msg: u32,
    /// NOTIFYICONDATAW 在目标进程里的地址
    addr: usize,
}

struct Nid {
    tip: String,
    ver: u32,
    guid: GUID,
}

fn open(pid: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok() }
}

fn read_bytes(h: HANDLE, addr: usize, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut got = 0usize;
    unsafe {
        ReadProcessMemory(h, addr as *const c_void, buf.as_mut_ptr() as *mut c_void, len, Some(&mut got))
            .ok()?;
    }
    if got < len {
        return None;
    }
    Some(buf)
}

fn read_nid(pid: u32, addr: usize) -> Option<Nid> {
    let h = open(pid)?;
    let b = read_bytes(h, addr, NID_SIZE);
    unsafe {
        let _ = CloseHandle(h);
    }
    let b = b?;
    let tip: String = b[40..296]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .map(|c| char::from_u32(c as u32).unwrap_or('?'))
        .collect();
    let ver = u32::from_le_bytes(b[816..820].try_into().unwrap());
    // GUID 在内存里是 Data1(u32,LE) Data2(u16,LE) Data3(u16,LE) Data4[8]
    let guid = GUID {
        data1: u32::from_le_bytes(b[952..956].try_into().unwrap()),
        data2: u16::from_le_bytes(b[956..958].try_into().unwrap()),
        data3: u16::from_le_bytes(b[958..960].try_into().unwrap()),
        data4: b[960..968].try_into().unwrap(),
    };
    Some(Nid { tip, ver, guid })
}

fn discover(pid: u32) -> Vec<Cand> {
    let wins = windows_of_pid(pid);
    println!("pid {pid}: {} 个窗口", wins.len());
    for (h, c) in &wins {
        println!("  hwnd={:#x} class={c}", h.0 as usize);
    }
    let map: HashMap<u64, usize> = wins
        .iter()
        .enumerate()
        .map(|(i, (h, _))| (h.0 as usize as u64, i))
        .collect();

    let Some(proc) = open(pid) else {
        println!("OpenProcess 失败");
        return Vec::new();
    };

    let mut out: Vec<Cand> = Vec::new();
    let mut addr: usize = 0x10000;
    let max = 0x7fff_ffef_ffffusize;
    let mut buf = vec![0u8; 4 << 20];
    let mut scanned = 0usize;
    while addr < max {
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
        if mbi.State == MEM_COMMIT && prot != 0x01 && (prot & 0x100) == 0 {
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
                    scanned += got;
                    scan_chunk(&buf[..got], base + off, &map, &wins, &mut out);
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
    unsafe {
        let _ = CloseHandle(proc);
    }
    println!("扫描 {:.1} MB", scanned as f64 / 1048576.0);
    out
}

fn scan_chunk(
    b: &[u8],
    chunk_base: usize,
    map: &HashMap<u64, usize>,
    wins: &[(HWND, String)],
    out: &mut Vec<Cand>,
) {
    let mut i = 8;
    while i + 40 <= b.len() {
        let v = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if let Some(&wi) = map.get(&v) {
            // NOTIFYICONDATAW: cbSize(0) hWnd(8) uID(16) uFlags(20) uCallbackMessage(24)
            let s = &b[i - 8..i + 40];
            let cb = u32::from_le_bytes(s[0..4].try_into().unwrap());
            let uid = u32::from_le_bytes(s[16..20].try_into().unwrap());
            let flags = u32::from_le_bytes(s[20..24].try_into().unwrap());
            let msg = u32::from_le_bytes(s[24..28].try_into().unwrap());
            if (0x150..=0x500).contains(&cb)
                && (flags & 0x1) != 0
                && (0x0400..=0xFFFF).contains(&msg)
                && uid < 0x1000
            {
                out.push(Cand {
                    hwnd: wins[wi].0,
                    class: wins[wi].1.clone(),
                    cb,
                    uid,
                    flags,
                    msg,
                    addr: chunk_base + i - 8,
                });
            }
        }
        i += 8;
    }
}

// ---------- 窗口枚举 ----------

fn windows_of_pid(pid: u32) -> Vec<(HWND, String)> {
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
    v.push((h, class_of(h)));
    TRUE
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) };
    String::from_utf16_lossy(&b[..n as usize])
}

fn tip_of(h: HWND) -> String {
    let mut b = [0u16; 128];
    let n = unsafe { GetWindowTextW(h, &mut b) };
    String::from_utf16_lossy(&b[..n as usize])
}

// ---------- 图标矩形 ----------

fn icon_rect(hwnd: HWND, uid: u32, guid: GUID) -> Option<(i32, i32, i32, i32)> {
    // 带 NIF_GUID 的图标必须用 guid 查（用 hWnd/uID 会 E_FAIL）
    let tries = [
        NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: HWND::default(),
            uID: 0,
            guidItem: guid,
        },
        NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: uid,
            guidItem: GUID::default(),
        },
    ];
    for id in tries {
        if let Ok(r) = unsafe { Shell_NotifyIconGetRect(&id) } {
            return Some((r.left, r.top, r.right, r.bottom));
        }
    }
    None
}

// ---------- 试发 ----------

fn try_events(c: &Cand, guid: GUID, ver: u32) {
    let Some((l, t, r, b)) = icon_rect(c.hwnd, c.uid, guid) else {
        println!("[{:#x}/{}] 取不到图标矩形，跳过", c.hwnd.0 as usize, c.uid);
        return;
    };
    let (x, y) = ((l + r) / 2, (t + b) / 2);
    let before = visible_set(c.hwnd);
    let events = [
        ("WM_MOUSEMOVE", WM_MOUSEMOVE),
        ("WM_RBUTTONDOWN", WM_RBUTTONDOWN),
        ("WM_RBUTTONUP", WM_RBUTTONUP),
        ("WM_LBUTTONDOWN", WM_LBUTTONDOWN),
        ("WM_LBUTTONUP", WM_LBUTTONUP),
    ];
    for (name, ev) in events {
        let (w, l2) = encode(ver, c.uid, ev, x, y);
        unsafe {
            SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
        }
        println!("[{:#x}/{}] {name} w={w:#x} l={l2:#x}", c.hwnd.0 as usize, c.uid);
        std::thread::sleep(std::time::Duration::from_millis(350));
        let new: Vec<String> = visible_set(c.hwnd).difference(&before).cloned().collect();
        if !new.is_empty() {
            println!("    → 新窗口: {new:?}");
        }
    }
    // v3/v4 的"激活"语义：NIN_SELECT
    let (w, l2) = encode(ver, c.uid, 0x0400, x, y);
    unsafe {
        SendNotifyMessageW(c.hwnd, c.msg, WPARAM(w as usize), LPARAM(l2 as isize));
    }
    std::thread::sleep(std::time::Duration::from_millis(350));
    let new: Vec<String> = visible_set(c.hwnd).difference(&before).cloned().collect();
    println!("    NIN_SELECT 后新窗口: {new:?}");
}

fn visible_set(any: HWND) -> HashSet<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(any, Some(&mut pid)) };
    windows_of_pid(pid)
        .into_iter()
        .filter(|(h, _)| unsafe { IsWindowVisible(*h).as_bool() })
        .map(|(h, c)| format!("{:#x} {c} {:?}", h.0 as usize, tip_of(h)))
        .collect()
}

// ---------- 截屏（看悬停卡片有没有弹） ----------

fn shot(x: i32, y: i32, w: i32, h: i32, path: &str) {
    unsafe {
        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        let Ok(hbmp) = hbmp else { return };
        let old = SelectObject(mem, HGDIOBJ(hbmp.0));
        let _ = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY);
        let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        // 写 24 位 BMP（自下而上）
        let row = (w * 3 + 3) & !3;
        let mut out = Vec::with_capacity((row * h) as usize + 54);
        let data_size = row * h;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + data_size).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&data_size.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        for yy in (0..h).rev() {
            let mut line = vec![0u8; row as usize];
            for xx in 0..w {
                let i = ((yy * w + xx) * 4) as usize;
                line[(xx * 3) as usize] = px[i];
                line[(xx * 3 + 1) as usize] = px[i + 1];
                line[(xx * 3 + 2) as usize] = px[i + 2];
            }
            out.extend_from_slice(&line);
        }
        let _ = SelectObject(mem, old);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        let _ = std::fs::write(path, out);
        println!("已保存 {path}");
    }
}

/// 该 pid 所有窗口的快照（含不可见），带 class 和矩形
fn all_windows(pid: u32) -> HashSet<String> {
    windows_of_pid(pid)
        .into_iter()
        .map(|(h, c)| {
            let mut r = windows::Win32::Foundation::RECT::default();
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(h, &mut r);
            }
            format!("{:#x} {c} vis={} ({},{},{},{})", h.0 as usize, unsafe { IsWindowVisible(h).as_bool() } as u8, r.left, r.top, r.right, r.bottom)
        })
        .collect()
}
