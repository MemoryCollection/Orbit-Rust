//! uiadump — 微信 UIA 树诊断工具（独立于 orbit 主程序，只读）
//!
//! 用法:
//!   uiadump                     抓一轮，打印每个微信实例的窗口 + UIA 树 + 会话 Cell
//!   uiadump --watch 300         每 3 秒抓一轮，持续 300 秒；第一轮打全树，之后打摘要+Cell
//!   uiadump --watch 60 --tree   每轮都打全树
//!   uiadump --depth 6 --interval 2
//!
//! 输出直接 stdout，重定向到文件即可。每轮结束 flush。

use std::time::Duration;

use windows::core::{BSTR, VARIANT};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition,
    IUIAutomationElement, TreeScope_Children, TreeScope_Descendants, TreeScope_Subtree,
    UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_IsOffscreenPropertyId, UIA_NamePropertyId, IUIAutomationInvokePattern,
    UIA_InvokePatternId, IUIAutomationSelectionItemPattern, IUIAutomationLegacyIAccessiblePattern,
    UIA_SelectionItemPatternId, UIA_LegacyIAccessiblePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowW, GetClassNameW, GetForegroundWindow, GetWindowTextW,
    GetWindowThreadProcessId, GetWindowRect, GetCursorPos, SetCursorPos, IsHungAppWindow,
    IsIconic, IsWindowVisible, ShowWindow, SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINNOACTIVE,
    SW_SHOWNA, PostMessageW, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

const CELL_CLASS: &str = "mmui::ChatSessionCell";

#[repr(C)]
struct EnumCtx {
    out: Vec<(HWND, u32)>,
}

unsafe extern "system" fn enum_cb(h: HWND, l: LPARAM) -> BOOL {
    let c = &mut *(l.0 as *mut EnumCtx);
    let mut cls = [0u16; 128];
    let n = GetClassNameW(h, &mut cls);
    let name = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if name.contains("QWindowIcon") {
        let mut p = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut p));
        c.out.push((h, p));
    }
    BOOL(1)
}

fn qwindow_icons() -> Vec<(HWND, u32)> {
    let mut d = EnumCtx { out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut d as *mut _ as isize));
    }
    d.out
}

fn proc_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let s = String::from_utf16_lossy(&buf[..len as usize]);
        s.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

fn is_wechat_exe(n: &str) -> bool {
    n.eq_ignore_ascii_case("Weixin.exe") || n.eq_ignore_ascii_case("WeChat.exe")
}

fn all_wechat_pids() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            let name = String::from_utf16_lossy(&pe.szExeFile);
            let base = name.trim_end_matches('\0');
            if is_wechat_exe(base) {
                out.push((pe.th32ProcessID, base.to_string()));
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn win_title(h: HWND) -> String {
    let mut b = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

fn win_class(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => o.push_str("\\n"),
            '\r' => {}
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    if o.chars().count() > 240 {
        let t: String = o.chars().take(240).collect();
        format!("{t}…")
    } else {
        o
    }
}

fn ctype_name(t: i32) -> &'static str {
    match t {
        50000 => "Button",
        50001 => "Calendar",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50010 => "MenuBar",
        50011 => "MenuItem",
        50012 => "ProgressBar",
        50013 => "RadioButton",
        50014 => "ScrollBar",
        50015 => "Slider",
        50016 => "Spinner",
        50017 => "StatusBar",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50022 => "ToolTip",
        50023 => "Tree",
        50024 => "TreeItem",
        50025 => "Custom",
        50026 => "Group",
        50027 => "Thumb",
        50028 => "DataGrid",
        50029 => "DataItem",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        50034 => "Header",
        50035 => "HeaderItem",
        50036 => "Table",
        50037 => "TitleBar",
        50038 => "Separator",
        50039 => "SemanticZoom",
        50040 => "AppBar",
        _ => "?",
    }
}

struct Stats {
    nodes: usize,
    cells: usize,
    cell_names: Vec<String>,
}

fn props(e: &IUIAutomationElement) -> (String, String, String, i32, bool) {
    unsafe {
        let cls = e.CachedClassName().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let name = e.CachedName().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let aid = e.CachedAutomationId().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let ct = e.CachedControlType().map(|t| t.0).unwrap_or(-1);
        let off = e.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false);
        (cls, name, aid, ct, off)
    }
}

fn props_live(e: &IUIAutomationElement) -> (String, String, String, i32, bool) {
    unsafe {
        let cls = e.CurrentClassName().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let name = e.CurrentName().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let aid = e.CurrentAutomationId().map(|b: BSTR| b.to_string()).unwrap_or_default();
        let ct = e.CurrentControlType().map(|t| t.0).unwrap_or(-1);
        let off = e.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(false);
        (cls, name, aid, ct, off)
    }
}

fn dump_node(
    e: &IUIAutomationElement,
    depth: usize,
    max_depth: usize,
    cap: &mut usize,
    filter: &IUIAutomationCondition,
    out: &mut String,
    st: &mut Stats,
) {
    if *cap == 0 || depth > max_depth {
        return;
    }
    *cap -= 1;
    st.nodes += 1;
    let (cls, name, aid, ct, off) = props_live(e);
    if cls == CELL_CLASS {
        st.cells += 1;
        let mut line = format!("[{}] {}", st.cells - 1, esc(&name));
        if !aid.is_empty() {
            line.push_str(&format!("   (aid={aid})"));
        }
        st.cell_names.push(line);
    }
    out.push_str(&format!(
        "{}{}({}) class={} off={} aid={} name={}\n",
        "  ".repeat(depth),
        ctype_name(ct),
        ct,
        if cls.is_empty() { "-" } else { &cls },
        if off { 1 } else { 0 },
        if aid.is_empty() { "-" } else { &aid },
        esc(&name)
    ));
    if depth == max_depth {
        return;
    }
    let kids = unsafe { e.FindAll(TreeScope_Children, filter) };
    let Ok(kids) = kids else { return };
    let n = unsafe { kids.Length() }.unwrap_or(0);
    for i in 0..n {
        if *cap == 0 {
            return;
        }
        if let Ok(k) = unsafe { kids.GetElement(i) } {
            dump_node(&k, depth + 1, max_depth, cap, filter, out, st);
        }
    }
}

struct Held {
    pid: u32,
    hwnd: HWND,
    el: IUIAutomationElement,
}


struct Opts {
    allheld: bool,
    dump_wins: bool,
}

/// 全量缓存的后代元素（用于「隐藏后逐个刷新，看哪些还留有文字」实验）
struct HeldAll {
    pid: u32,
    hwnd: HWND,
    class0: String,
    name0: String,
    el: IUIAutomationElement,
}

#[repr(C)]
struct AllCtx {
    out: Vec<(HWND, u32)>,
}

unsafe extern "system" fn enum_all_cb(h: HWND, l: LPARAM) -> BOOL {
    let c = &mut *(l.0 as *mut AllCtx);
    let mut p = 0u32;
    let _ = GetWindowThreadProcessId(h, Some(&mut p));
    c.out.push((h, p));
    BOOL(1)
}

fn all_top_windows() -> Vec<(HWND, u32)> {
    let mut d = AllCtx { out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_all_cb), LPARAM(&mut d as *mut _ as isize));
    }
    d.out
}

#[repr(C)]
struct ChildCtx {
    out: Vec<HWND>,
}

unsafe extern "system" fn enum_child_cb(h: HWND, l: LPARAM) -> BOOL {
    let c = &mut *(l.0 as *mut ChildCtx);
    c.out.push(h);
    BOOL(1)
}

/// 列出微信进程的**所有**窗口（顶层不限 class，含隐藏）+ 全部子窗口，
/// 并对每个窗口试一次 UIA（能否拿到会话 Cell）。
fn dump_all_windows(auto: &IUIAutomation, cache: &IUIAutomationCacheRequest, cond: &IUIAutomationCondition) {
    let pids: Vec<u32> = all_wechat_pids().into_iter().map(|(p, _)| p).collect();
    let tops: Vec<(HWND, u32)> = all_top_windows()
        .into_iter()
        .filter(|(_, p)| pids.contains(p))
        .collect();
    let mut bypid: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (h, p) in &tops {
        *bypid.entry(*p).or_insert(0) += 1;
        let mut kids = ChildCtx { out: Vec::new() };
        unsafe {
            let _ = EnumChildWindows(*h, Some(enum_child_cb), LPARAM(&mut kids as *mut _ as isize));
        }
        let (cells, err) = unsafe {
            match auto.ElementFromHandleBuildCache(*h, Some(cache)) {
                Ok(root) => match root.FindAllBuildCache(TreeScope_Descendants, Some(cond), Some(cache)) {
                    Ok(a) => (a.Length().unwrap_or(0), String::new()),
                    Err(e) => (-1, format!("{:?}", e.code())),
                },
                Err(e) => (-1, format!("{:?}", e.code())),
            }
        };
        let (tvis, ticon) = unsafe { (IsWindowVisible(*h).as_bool(), IsIconic(*h).as_bool()) };
        println!(
            "  顶层 {:?} class={} title={:?} vis={} iconic={} kids={} cells={}{}",
            h,
            win_class(*h),
            win_title(*h),
            tvis,
            ticon,
            kids.out.len(),
            cells,
            if err.is_empty() { String::new() } else { format!(" err={err}") },
        );
        for k in kids.out.iter().take(24) {
            let kc = win_class(*k);
            let kcells = unsafe {
                auto.ElementFromHandleBuildCache(*k, Some(cache))
                    .ok()
                    .and_then(|r| r.FindAllBuildCache(TreeScope_Descendants, Some(cond), Some(cache)).ok())
                    .and_then(|a| a.Length().ok())
                    .unwrap_or(-1)
            };
            let kvis = unsafe { IsWindowVisible(*k).as_bool() };
            println!(
                "      子 {:?} class={} vis={} cells={}",
                k,
                if kc.is_empty() { "-" } else { &kc },
                kvis,
                kcells
            );
        }
        if kids.out.len() > 24 {
            println!("      ... 还有 {} 个子窗口", kids.out.len() - 24);
        }
    }
    for (p, n) in bypid {
        println!("  pid {p}: 顶层窗口 {n} 个");
    }
    println!();
}

fn round(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    cache_kids: &IUIAutomationCacheRequest,
    cond: &IUIAutomationCondition,
    true_cond: &IUIAutomationCondition,
    tree: bool,
    max_depth: usize,
    held: &mut Vec<Held>,
    opts: &Opts,
    held_all: &mut Vec<HeldAll>,
) {
    let windower = qwindow_icons();
    let procs = all_wechat_pids();
    let mut wx_wins: Vec<(HWND, u32, String)> = Vec::new();
    for (h, p) in &windower {
        let nm = proc_name(*p);
        if is_wechat_exe(&nm) {
            wx_wins.push((*h, *p, nm));
        }
    }
    let mut front = 0u32;
    unsafe {
        GetWindowThreadProcessId(
            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow(),
            Some(&mut front),
        );
    }
    println!(
        "--- 微信进程 {} 个: {:?} | QWindowIcon 窗口 {} 个 | 前台 pid={front}",
        procs.len(),
        procs
            .iter()
            .map(|(p, n)| format!("{p}:{n}"))
            .collect::<Vec<_>>(),
        wx_wins.len(),
    );

    let mut total_cells = 0usize;
    for (h, pid, exe) in &wx_wins {
        unsafe {
            println!(
                "[win hwnd={:?} pid={} exe={} class={} title={:?} visible={} iconic={} hung={}]",
                h,
                pid,
                exe,
                win_class(*h),
                win_title(*h),
                IsWindowVisible(*h).as_bool(),
                IsIconic(*h).as_bool(),
                IsHungAppWindow(*h).as_bool(),
            );
        }
        let root = unsafe { auto.ElementFromHandleBuildCache(*h, Some(cache)) };
        let root = match root {
            Ok(r) => r,
            Err(e) => {
                println!("  ElementFromHandle 失败: {e:?}");
                continue;
            }
        };
        let mut st = Stats { nodes: 0, cells: 0, cell_names: Vec::new() };
        // 会话 Cell 单独全量子树查一遍（不受树打印深度/节点上限影响）
        let mut cell_count_by_findall = 0usize;
        let mut fresh: Vec<Held> = Vec::new();
        if let Ok(arr) =
            unsafe { root.FindAllBuildCache(TreeScope_Descendants, Some(cond), Some(cache)) }
        {
            let n = unsafe { arr.Length() }.unwrap_or(0);
            cell_count_by_findall = n as usize;
            for i in 0..n {
                if let Ok(el) = unsafe { arr.GetElement(i) } {
                    let (_, name, aid, _, off) = props(&el);
                    println!(
                        "  CELL[{}] off={} aid={} name={}",
                        i,
                        if off { 1 } else { 0 },
                        if aid.is_empty() { "-" } else { &aid },
                        esc(&name)
                    );
                    fresh.push(Held { pid: *pid, hwnd: *h, el });
                }
            }
        }
        println!("  Cell(FindAll Descendants)={cell_count_by_findall}");
        if opts.allheld {
            held_all.retain(|x| x.hwnd != *h);
            if let Ok(arr) = unsafe {
                root.FindAllBuildCache(TreeScope_Descendants, Some(true_cond), Some(cache))
            } {
                let n = unsafe { arr.Length() }.unwrap_or(0).min(800);
                for i in 0..n {
                    if let Ok(el) = unsafe { arr.GetElement(i) } {
                        let (cls, name, _, _, _) = props(&el);
                        held_all.push(HeldAll {
                            pid: *pid,
                            hwnd: *h,
                            class0: cls,
                            name0: name,
                            el,
                        });
                    }
                }
            }
            println!("  [全量 held] 本窗口缓存后代 {} 个", held_all.iter().filter(|x| x.hwnd == *h).count());
        }
        let mut cap = 400usize;
        let mut buf = String::new();
        if tree {
            dump_node(&root, 0, max_depth, &mut cap, true_cond, &mut buf, &mut st);
            println!("  UIA 树（最多 {max_depth} 层 / 400 节点）:");
            print!("{buf}");
            println!("  树统计: nodes={} cells={}", st.nodes, st.cells);
        }
        total_cells += cell_count_by_findall;
        if !fresh.is_empty() {
            // 只保留当前窗口的；跨轮换新
            held.retain(|x| x.hwnd != *h);
            held.extend(fresh);
        }
    }

    // 树里没有 Cell（微信关到托盘）时，试着刷新上一轮持有的元素
    if total_cells == 0 && !held.is_empty() {
        println!("  [held 刷新测试] 树里 0 个 Cell，用上轮缓存的 {} 个元素 BuildUpdatedCache", held.len());
        let mut ok_n = 0;
        let mut fail_n = 0;
        for h in held.iter() {
            match unsafe { h.el.BuildUpdatedCache(Some(cache)) } {
                Ok(up) => {
                    let (cls, name, aid, _, off) = props(&up);
                    if cls == CELL_CLASS && !name.trim().is_empty() {
                        ok_n += 1;
                        println!(
                            "    held ok pid={} aid={} off={} name={}",
                            h.pid,
                            if aid.is_empty() { "-" } else { &aid },
                            if off { 1 } else { 0 },
                            esc(&name)
                        );
                    } else {
                        fail_n += 1;
                        println!(
                            "    held 空 pid={} class={} name={}",
                            h.pid,
                            if cls.is_empty() { "-" } else { &cls },
                            esc(&name)
                        );
                    }
                }
                Err(e) => {
                    fail_n += 1;
                    println!("    held 刷新失败 pid={} err={:?}", h.pid, e.code());
                }
            }
        }
        println!("  [held 结果] 可用={ok_n} 失败/空={fail_n} 持有={}", held.len());
    } else if total_cells > 0 {
        println!("  [held] 本轮抓到 {} 个 Cell，持有总量 {}", total_cells, held.len());
    }
    if opts.allheld && total_cells == 0 && !held_all.is_empty() {
        let mut ok = 0usize;
        let mut empty = 0usize;
        let mut fail = 0usize;
        let mut samples: Vec<String> = Vec::new();
        for h in held_all.iter() {
            match unsafe { h.el.BuildUpdatedCache(Some(cache)) } {
                Ok(up) => {
                    let (cls, name, aid, ct, _) = props(&up);
                    if !name.trim().is_empty() {
                        ok += 1;
                        if samples.len() < 30 {
                            samples.push(format!(
                                "pid={} {}({}) class={} aid={} name={}",
                                h.pid,
                                ctype_name(ct),
                                ct,
                                if cls.is_empty() { "-" } else { &cls },
                                if aid.is_empty() { "-" } else { &aid },
                                esc(&name)
                            ));
                        }
                    } else {
                        empty += 1;
                    }
                }
                Err(_) => fail += 1,
            }
        }
        println!(
            "  [全量 held] {} 个元素：仍有 Name={} / Name 空={} / 刷新失败={}",
            held_all.len(),
            ok,
            empty,
            fail
        );
        for s in samples {
            println!("    keep {s}");
        }
    }
    println!();
}


/// 新窗口扫描：每 400ms 枚举微信进程的所有顶层窗口，凡「新出现」或「由隐藏变可见」
/// 就打印详情 + 抓一次 UIA（Cell 数 + 前 6 层树）。用来抓「来消息时微信自己弹的提示窗」。
fn popup_watch(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    cond: &IUIAutomationCondition,
    true_cond: &IUIAutomationCondition,
    secs: u64,
) {
    use std::collections::HashMap;
    let mut seen: HashMap<isize, (bool, String)> = HashMap::new();
    let t0 = std::time::Instant::now();
    let mut ticks = 0u32;
    while t0.elapsed() < Duration::from_secs(secs) {
        ticks += 1;
        let pids: Vec<u32> = all_wechat_pids().into_iter().map(|(p, _)| p).collect();
        let wins: Vec<(HWND, u32)> = all_top_windows()
            .into_iter()
            .filter(|(_, p)| pids.contains(p))
            .collect();
        let mut alive: Vec<isize> = Vec::new();
        for (h, pid) in &wins {
            let key = h.0 as isize;
            alive.push(key);
            let vis = unsafe { IsWindowVisible(*h).as_bool() };
            let cls = win_class(*h);
            let prev = seen.get(&key).cloned();
            let is_new = prev.is_none();
            let shown = prev.as_ref().map(|(v, _)| !*v && vis).unwrap_or(vis);
            if is_new || shown {
                let title = win_title(*h);
                println!(
                    "[{}] {} hwnd={:?} pid={} class={} title={:?} vis={}",
                    now(),
                    if is_new { "新窗口" } else { "由隐转显" },
                    h,
                    pid,
                    cls,
                    title,
                    vis
                );
                if vis {
                    let root = unsafe { auto.ElementFromHandle(*h) };
                    if let Ok(root) = root {
                        let cells = unsafe {
                            root.FindAll(TreeScope_Descendants, cond)
                                .ok()
                                .and_then(|a| a.Length().ok())
                                .unwrap_or(-1)
                        };
                        println!("    Cell={cells}");
                        let mut cap = 200usize;
                        let mut buf = String::new();
                        let mut st = Stats { nodes: 0, cells: 0, cell_names: Vec::new() };
                        dump_node(&root, 0, 6, &mut cap, true_cond, &mut buf, &mut st);
                        print!("{buf}");
                        for c in st.cell_names.iter().take(20) {
                            println!("    {c}");
                        }
                    } else {
                        println!("    ElementFromHandle 失败");
                    }
                }
            }
            seen.insert(key, (vis, cls));
        }
        for k in seen.keys() {
            if !alive.contains(k) {
                println!("[{}] 窗口消失 hwnd=0x{:x}", now(), *k as usize);
            }
        }
        seen.retain(|k, _| alive.contains(k));
        if ticks % 25 == 0 {
            println!("[{}] …扫描中（{} 个微信顶层窗口）", now(), wins.len());
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    println!("===== 窗口扫描结束 =====");
}


/// 两个微信的 QWindowIcon 主窗口（含隐藏）
fn wechat_windows_all() -> Vec<HWND> {
    qwindow_icons()
        .into_iter()
        .filter(|(_, p)| is_wechat_exe(&proc_name(*p)))
        .map(|(h, _)| h)
        .collect()
}

/// 托盘 tooltip：任务栏通知区按钮（AID=NotifyItemIcon）的 Name 就是 tooltip 全文
fn dump_tray(auto: &IUIAutomation) {
    let tb = match unsafe {
        windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            windows::core::w!("Shell_TrayWnd"),
            windows::core::PCWSTR::null(),
        )
    } {
        Ok(h) => h,
        Err(e) => {
            println!("找不到 Shell_TrayWnd {:?}", e.code());
            return;
        }
    };
    let root = match unsafe { auto.ElementFromHandle(tb) } {
        Ok(r) => r,
        Err(e) => {
            println!("ElementFromHandle(任务栏) 失败 {:?}", e.code());
            return;
        }
    };
    let by_type = unsafe {
        auto.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(windows::Win32::UI::Accessibility::UIA_ButtonControlTypeId.0 as i32),
        )
    }
    .ok();
    let by_aid = unsafe {
        auto.CreatePropertyCondition(
            UIA_AutomationIdPropertyId,
            &VARIANT::from("NotifyItemIcon"),
        )
    }
    .ok();
    let cond = match (by_type, by_aid) {
        (Some(a), Some(b)) => unsafe { auto.CreateAndCondition(&a, &b) }.ok(),
        _ => None,
    };
    let Some(cond) = cond else {
        println!("条件构造失败");
        return;
    };
    let arr = match unsafe { root.FindAll(TreeScope_Descendants, &cond) } {
        Ok(a) => a,
        Err(e) => {
            println!("FindAll 失败 {:?}", e.code());
            return;
        }
    };
    let n = unsafe { arr.Length() }.unwrap_or(0);
    println!("任务栏托盘按钮 {n} 个");
    for i in 0..n {
        let Ok(e) = (unsafe { arr.GetElement(i) }) else { continue };
        let name = unsafe { e.CurrentName() }
            .map(|b: BSTR| b.to_string())
            .unwrap_or_default();
        let pid = unsafe { e.CurrentProcessId() }.unwrap_or(-1);
        let exe = proc_name(pid.max(0) as u32);
        if exe.eq_ignore_ascii_case("Weixin.exe") || exe.eq_ignore_ascii_case("WeChat.exe") || name.contains("微信") {
            println!("  pid={pid} exe={exe} tooltip={}", esc(&name));
        }
    }
}

/// 从屏幕直接 BitBlt 一块区域存 BMP——分层窗口（ORBIT_WXHOVER / 横幅）用
/// PrintWindow 往往是全黑，只能抓屏上合成后的像素。
fn grab_screen(x0: i32, y0: i32, w: i32, hh: i32, path: &str) -> bool {
    unsafe {
        let sdc = GetDC(HWND::default());
        let mdc = CreateCompatibleDC(sdc);
        let bmp = CreateCompatibleBitmap(sdc, w, hh);
        let old = SelectObject(mdc, bmp);
        let ok = windows::Win32::Graphics::Gdi::BitBlt(
            mdc,
            0,
            0,
            w,
            hh,
            sdc,
            x0,
            y0,
            windows::Win32::Graphics::Gdi::SRCCOPY,
        )
        .is_ok();
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -hh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * hh * 4) as usize];
        let lines = GetDIBits(
            mdc,
            bmp,
            0,
            hh as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        SelectObject(mdc, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mdc);
        ReleaseDC(HWND::default(), sdc);
        println!("  BitBlt={ok} lines={lines} {w}x{hh}");
        if lines <= 0 {
            return false;
        }
        let mut out = Vec::with_capacity(54 + buf.len());
        let file_size = 54 + buf.len() as u32;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&(-hh).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&buf);
        std::fs::write(path, &out).is_ok()
    }
}

/// PrintWindow 截一张隐藏窗口，存 BMP。返回是否成功。
fn capture(h: HWND, path: &str) -> bool {
    unsafe {
        let mut r = RECT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(h, &mut r).is_err() {
            return false;
        }
        let (w, hh) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || hh <= 0 {
            println!("  窗口尺寸非法 {w}x{hh}");
            return false;
        }
        let sdc = GetDC(HWND::default());
        let mdc = CreateCompatibleDC(sdc);
        let bmp = CreateCompatibleBitmap(sdc, w, hh);
        let old = SelectObject(mdc, bmp);
        // 2 = PW_RENDERFULLCONTENT
        let ok = PrintWindow(h, mdc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -hh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * hh * 4) as usize];
        let lines = GetDIBits(
            mdc,
            bmp,
            0,
            hh as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        SelectObject(mdc, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mdc);
        ReleaseDC(HWND::default(), sdc);
        // 统计非黑像素比例，判断到底有没有画出东西
        let mut nz = 0usize;
        for px in buf.chunks_exact(4) {
            if px[0] > 8 || px[1] > 8 || px[2] > 8 {
                nz += 1;
            }
        }
        let total = (w * hh) as usize;
        println!(
            "  PrintWindow={ok} lines={lines} {w}x{hh} 非黑像素 {:.1}%",
            nz as f64 * 100.0 / total as f64
        );
        if lines <= 0 {
            return false;
        }
        let mut out = Vec::with_capacity(54 + buf.len());
        let file_size = 54 + buf.len() as u32;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&hh.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&buf);
        std::fs::write(path, &out).is_ok()
    }
}


fn count_cells(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    cond: &IUIAutomationCondition,
    h: HWND,
) -> (i32, Vec<String>) {
    let Ok(root) = (unsafe { auto.ElementFromHandleBuildCache(h, Some(cache)) }) else {
        return (-1, Vec::new());
    };
    let Ok(arr) = (unsafe { root.FindAllBuildCache(TreeScope_Descendants, Some(cond), Some(cache)) })
    else {
        return (-1, Vec::new());
    };
    let n = unsafe { arr.Length() }.unwrap_or(0);
    let mut names = Vec::new();
    for i in 0..n.min(3) {
        if let Ok(el) = unsafe { arr.GetElement(i) } {
            if let Ok(b) = unsafe { el.CurrentName() } {
                names.push(esc(&b.to_string()));
            }
        }
    }
    (n, names)
}

fn state(h: HWND) -> String {
    unsafe {
        format!(
            "vis={} iconic={}",
            IsWindowVisible(h).as_bool(),
            IsIconic(h).as_bool()
        )
    }
}

/// 探针：隐藏 → SW_SHOWMINNOACTIVE 唤回，验证树能否恢复、是否抢前台
fn probe_minimized(auto: &IUIAutomation, cache: &IUIAutomationCacheRequest, cond: &IUIAutomationCondition) {
    for h in wechat_windows_all() {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        let (c0, n0) = count_cells(auto, cache, cond, h);
        println!("[probe] hwnd={:?} pid={} {} cells={}", h, pid, state(h), c0);
        for x in &n0 {
            println!("    初始行 {x}");
        }

        unsafe { ShowWindow(h, SW_HIDE) };
        std::thread::sleep(Duration::from_millis(500));
        let (c1, _) = count_cells(auto, cache, cond, h);
        println!("  → SW_HIDE 后           {} cells={}", state(h), c1);

        let fg0 = unsafe { GetForegroundWindow() };
        let t0 = std::time::Instant::now();
        unsafe { ShowWindow(h, SW_SHOWMINNOACTIVE) };
        std::thread::sleep(Duration::from_millis(600));
        let (c2, n2) = count_cells(auto, cache, cond, h);
        let fg1 = unsafe { GetForegroundWindow() };
        println!(
            "  → SW_SHOWMINNOACTIVE 后 {} cells={} 耗时={}ms 前台变化={} ({:?}→{:?})",
            state(h),
            c2,
            t0.elapsed().as_millis(),
            fg0 != fg1,
            fg0,
            fg1
        );
        for x in &n2 {
            println!("    唤回后行 {x}");
        }
        println!();
    }
}


/// 探针 v2：逐级尝试 还原→最小化 / SHOWNA→最小化，看树在哪一步回来。
fn probe2(auto: &IUIAutomation, cache: &IUIAutomationCacheRequest, cond: &IUIAutomationCondition) {
    for h in wechat_windows_all() {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        println!("[probe2] hwnd={:?} pid={} {}", h, pid, state(h));
        let (c, _) = count_cells(auto, cache, cond, h);
        println!("  起始 cells={c}");

        // 1) SW_RESTORE：真正显示一次
        let fg0 = unsafe { GetForegroundWindow() };
        unsafe { let _ = ShowWindow(h, SW_RESTORE); };
        std::thread::sleep(Duration::from_millis(1500));
        let (c1, n1) = count_cells(auto, cache, cond, h);
        println!(
            "  SW_RESTORE 后        {} cells={} 前台变化={}",
            state(h),
            c1,
            unsafe { GetForegroundWindow() } != fg0
        );
        for x in n1.iter() {
            println!("    行 {x}");
        }

        // 2) SW_MINIMIZE：最小化，树是否保留
        unsafe { let _ = ShowWindow(h, SW_MINIMIZE); };
        std::thread::sleep(Duration::from_millis(1200));
        let (c2, _) = count_cells(auto, cache, cond, h);
        println!("  SW_MINIMIZE 后       {} cells={}", state(h), c2);

        // 3) SW_SHOWNA：不激活显示（温和唤出）
        unsafe { let _ = ShowWindow(h, SW_SHOWNA); };
        std::thread::sleep(Duration::from_millis(1200));
        let (c3, _) = count_cells(auto, cache, cond, h);
        let fg3 = unsafe { GetForegroundWindow() };
        println!(
            "  SW_SHOWNA 后         {} cells={} 前台变化={}",
            state(h),
            c3,
            fg3 != fg0
        );

        // 收尾回最小化，保持用户原有观感
        unsafe { let _ = ShowWindow(h, SW_MINIMIZE); };
        std::thread::sleep(Duration::from_millis(600));
        let (c4, _) = count_cells(auto, cache, cond, h);
        println!("  收尾 SW_MINIMIZE 后    {} cells={}\n", state(h), c4);
    }
}


/// 唤回：点任务栏托盘里的微信按钮（走微信自己的 show 路径），再验证树是否恢复。
/// 托盘按钮无法直接对应 pid，就逐个 invoke，看哪个能让空树变满。
fn wake(auto: &IUIAutomation, cache: &IUIAutomationCacheRequest, cond: &IUIAutomationCondition) {
    let tb = unsafe {
        windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            windows::core::w!("Shell_TrayWnd"),
            windows::core::PCWSTR::null(),
        )
    };
    let Ok(tb) = tb else { println!("找不到任务栏"); return };
    let Ok(root) = (unsafe { auto.ElementFromHandle(tb) }) else { return };
    let by_type = unsafe {
        auto.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(windows::Win32::UI::Accessibility::UIA_ButtonControlTypeId.0 as i32),
        )
    }
    .ok();
    let by_aid = unsafe {
        auto.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("NotifyItemIcon"))
    }
    .ok();
    let Some(cond2) = (match (by_type, by_aid) {
        (Some(a), Some(b)) => unsafe { auto.CreateAndCondition(&a, &b) }.ok(),
        _ => None,
    }) else { return };
    let Ok(arr) = (unsafe { root.FindAll(TreeScope_Descendants, &cond2) }) else { return };
    let n = unsafe { arr.Length() }.unwrap_or(0);
    println!("托盘按钮 {n} 个");

    let targets: Vec<HWND> = wechat_windows_all();
    for i in 0..n {
        let Ok(e) = (unsafe { arr.GetElement(i) }) else { continue };
        let name = unsafe { e.CurrentName() }.map(|b: BSTR| b.to_string()).unwrap_or_default();
        let pid = unsafe { e.CurrentProcessId() }.unwrap_or(-1);
        let exe = proc_name(pid.max(0) as u32);
        if !(exe.eq_ignore_ascii_case("Weixin.exe") || exe.eq_ignore_ascii_case("WeChat.exe")) {
            continue;
        }
        let before: Vec<i32> = targets.iter().map(|h| count_cells(auto, cache, cond, *h).0).collect();
        println!("  点击托盘按钮 name={} pid={pid}  点击前各窗口 cells={:?}", esc(&name), before);
        let pat = unsafe { e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) };
        match pat {
            Ok(p) => {
                if unsafe { p.Invoke() }.is_err() {
                    println!("    Invoke 失败");
                    continue;
                }
            }
            Err(x) => {
                println!("    无 Invoke 模式 {:?}", x.code());
                continue;
            }
        }
        std::thread::sleep(Duration::from_millis(1500));
        let after: Vec<i32> = targets.iter().map(|h| count_cells(auto, cache, cond, *h).0).collect();
        println!("    点击后各窗口 cells={:?}", after);
        for (h, c) in targets.iter().zip(after.iter()) {
            if *c > 0 {
                println!("    ✔ {:?} 已恢复 {} 行", h, c);
                let _ = h;
                break;
            }
        }
    }
    println!("\n最终状态：");
    for h in wechat_windows_all() {
        let (c, names) = count_cells(auto, cache, cond, h);
        println!("  {:?} {} cells={}", h, state(h), c);
        for x in names { println!("    {x}"); }
    }
}

/// 会话行能力探针：每行支持哪些 UIA pattern（决定能不能"点击跳转到某个会话"）。
/// `--select N` 时对第 N 行尝试 SelectionItem.Select() / Legacy DoDefaultAction，
/// 再回读选中态验证是否真的切换了对话。
/// 取第 k 行元素（每次现抓，避免用完缓存/失效引用）
fn row_elem(
    auto: &IUIAutomation,
    h: HWND,
    cond: &IUIAutomationCondition,
    k: usize,
) -> Option<IUIAutomationElement> {
    let root = unsafe { auto.ElementFromHandle(h) }.ok()?;
    let arr = unsafe { root.FindAll(TreeScope_Descendants, cond) }.ok()?;
    if unsafe { arr.Length() }.unwrap_or(0) <= k as i32 {
        return None;
    }
    unsafe { arr.GetElement(k as i32) }.ok()
}

/// 当前选中行（"选中"= SelectionItem.IsSelected），拿不到返回 None
fn selected_row(auto: &IUIAutomation, h: HWND, cond: &IUIAutomationCondition) -> Option<String> {
    let root = unsafe { auto.ElementFromHandle(h) }.ok()?;
    let arr = unsafe { root.FindAll(TreeScope_Descendants, cond) }.ok()?;
    let n = unsafe { arr.Length() }.unwrap_or(0);
    for i in 0..n {
        let Ok(e) = (unsafe { arr.GetElement(i) }) else { continue };
        let Ok(p) = (unsafe {
            e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        }) else {
            continue;
        };
        if unsafe { p.CurrentIsSelected() }.map(|b| b.as_bool()).unwrap_or(false) {
            let nm = unsafe { e.CurrentName() }.map(|b: BSTR| b.to_string()).unwrap_or_default();
            return Some(format!("[{i}] {}", esc(&nm)));
        }
    }
    None
}

/// 第 k 行的 name（看未读数 [N条] 有没有被清掉）
fn row_name(
    auto: &IUIAutomation,
    h: HWND,
    cond: &IUIAutomationCondition,
    k: usize,
) -> Option<String> {
    let e = row_elem(auto, h, cond, k)?;
    let nm = unsafe { e.CurrentName() }.map(|b: BSTR| b.to_string()).unwrap_or_default();
    Some(esc(&nm))
}

/// 蹲守微信托盘悬停浮窗（`mmui::UnreadMessageHoverWindow`）：它只在鼠标停在
/// 托盘图标上的那一两秒存在，之后被销毁。一旦抓到就 dump 全树 + 每个元素的
/// pattern，看清微信给"点击跳会话"留了什么接口（持续 `secs` 秒）。
fn dump_hover_win(
    auto: &IUIAutomation,
    true_cond: &IUIAutomationCondition,
    h: HWND,
    pid: u32,
    act: bool,
) {
    let Ok(root) = (unsafe { auto.ElementFromHandle(h) }) else { return };
    let rcls = unsafe { root.CurrentClassName() }
        .map(|b: BSTR| b.to_string())
        .unwrap_or_default();
    let rect = unsafe { root.CurrentBoundingRectangle() }.ok();
    println!("\n★ 抓到浮窗 hwnd={:?} pid={pid} class={rcls} rect={rect:?}", h);
    let mut out = String::new();
    let mut st = Stats { nodes: 0, cells: 0, cell_names: Vec::new() };
    let mut cap = 300usize;
    dump_node(&root, 0, 12, &mut cap, true_cond, &mut out, &mut st);
    print!("{out}");
    // Qt 的 Tool 窗口对 FindAll 不实现（返回空），只能按 TreeWalker 逐层走
    let Ok(walker) = (unsafe { auto.RawViewWalker() }) else { return };
    println!("  ---- 逐元素能力（TreeWalker）----");
    walk_node(auto, &walker, &root, 0);
    if act {
        if let Some(el) = find_aid(&walker, &root, "unread_hover_session_item", 0) {
            println!("  >>> 对浮窗会话行执行 DoDefaultAction（等价双击）");
            match unsafe {
                el.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )
            } {
                Ok(p) => println!("  >>> 结果 {:?}", unsafe { p.DoDefaultAction() }.map(|_| "ok")),
                Err(x) => println!("  >>> 无 Legacy 模式 {:?}", x.code()),
            }
            std::thread::sleep(Duration::from_millis(1600));
            println!("  >>> 动作后：浮窗存在={} 主窗口状态={}",
                find_hover(auto).is_some(),
                wechat_windows_all().first().map(|w| state(*w)).unwrap_or_default());
        } else {
            println!("  >>> 树里没找到 unread_hover_session_item");
        }
    }
}

fn find_aid(
    walker: &windows::Win32::UI::Accessibility::IUIAutomationTreeWalker,
    e: &IUIAutomationElement,
    want: &str,
    depth: usize,
) -> Option<IUIAutomationElement> {
    if depth > 10 {
        return None;
    }
    let (_, _, aid, _, _) = props_live(e);
    if aid == want {
        return Some(e.clone());
    }
    if let Ok(c) = unsafe { walker.GetFirstChildElement(e) } {
        let mut cur = Some(c);
        while let Some(x) = cur {
            if let Some(f) = find_aid(walker, &x, want, depth + 1) {
                return Some(f);
            }
            cur = unsafe { walker.GetNextSiblingElement(&x) }.ok();
        }
    }
    None
}

fn walk_node(
    _auto: &IUIAutomation,
    walker: &windows::Win32::UI::Accessibility::IUIAutomationTreeWalker,
    e: &IUIAutomationElement,
    depth: usize,
) {
    if depth > 10 {
        return;
    }
    let (cls, name, aid, ct, _) = props_live(e);
    let inv = unsafe {
        e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
    }
    .is_ok();
    let sel = unsafe {
        e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .is_ok();
    let leg = unsafe {
        e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(UIA_LegacyIAccessiblePatternId)
    };
    let def = match &leg {
        Ok(p) => unsafe { p.CurrentDefaultAction() }
            .map(|b: BSTR| b.to_string())
            .unwrap_or_default(),
        Err(_) => String::from("-"),
    };
    let rect = unsafe { e.CurrentBoundingRectangle() }.ok();
    println!(
        "{}{}({}) class={} aid={} name={} invoke={inv} selItem={sel} defAction={:?} rect={:?}",
        "  ".repeat(depth + 1),
        ctype_name(ct),
        ct,
        if cls.is_empty() { "-" } else { &cls },
        if aid.is_empty() { "-" } else { &aid },
        esc(&name),
        def,
        rect
    );
    if let Ok(c) = unsafe { walker.GetFirstChildElement(e) } {
        let mut cur = Some(c);
        while let Some(x) = cur {
            walk_node(_auto, walker, &x, depth + 1);
            cur = unsafe { walker.GetNextSiblingElement(&x) }.ok();
        }
    }
}

/// 浮窗是否在：class 命中 ToolSaveBits + UIA 根 class 是 UnreadMessageHover
fn find_hover(auto: &IUIAutomation) -> Option<(HWND, u32)> {
    for (h, pid) in all_top_windows() {
        let cls = win_class(h);
        if !cls.contains("ToolSaveBits") && !cls.contains("QWindowTool") {
            continue;
        }
        let Ok(root) = (unsafe { auto.ElementFromHandle(h) }) else { continue };
        let rcls = unsafe { root.CurrentClassName() }
            .map(|b: BSTR| b.to_string())
            .unwrap_or_default();
        if rcls.contains("UnreadMessageHover") {
            return Some((h, pid));
        }
    }
    None
}

fn hunt_hover(auto: &IUIAutomation, true_cond: &IUIAutomationCondition, secs: u64) {
    let t0 = std::time::Instant::now();
    eprintln!("[hunt] 蹲守 {secs}s，请把鼠标停在 Dock 上的微信图标");
    while t0.elapsed().as_secs() < secs {
        if let Some((h, pid)) = find_hover(auto) {
            dump_hover_win(auto, true_cond, h, pid, false);
            return;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    println!("[hunt] {secs}s 内没抓到浮窗（鼠标没停到微信托盘图标上？）");
}

/// 自动扫描：沿 y 这一行把鼠标从 x1 移到 x2，每步停一下看浮窗有没有弹。
/// 找到就 dump 并还原鼠标位置。用于没人配合时自己找托盘图标。
fn scan_hover(
    auto: &IUIAutomation,
    true_cond: &IUIAutomationCondition,
    x1: i32,
    x2: i32,
    y: i32,
    step: i32,
) {
    let mut p = windows::Win32::Foundation::POINT::default();
    let saved = unsafe { GetCursorPos(&mut p) }.is_ok();
    println!("[scan] y={y} x {x1}→{x2} step={step}");
    let mut x = x1;
    while (step > 0 && x <= x2) || (step < 0 && x >= x2) {
        unsafe {
            let _ = SetCursorPos(x, y);
        }
        std::thread::sleep(Duration::from_millis(280));
        if let Some((h, pid)) = find_hover(auto) {
            println!("[scan] 在 x={x} 命中");
            dump_hover_win(auto, true_cond, h, pid, false);
            if saved {
                unsafe {
                    let _ = SetCursorPos(p.x, p.y);
                }
            }
            return;
        }
        x += step;
    }
    if saved {
        unsafe {
            let _ = SetCursorPos(p.x, p.y);
        }
    }
    println!("[scan] 这一行没命中");
}

/// 找 Orbit 自绘的微信悬停预览浮窗（class=ORBIT_WXHOVER）：
/// 沿 y 行把鼠标移到微信托盘图标上，一旦浮窗可见就停在那里（不还原鼠标）。
fn hover_probe(x1: i32, x2: i32, y: i32, step: i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    let saved = unsafe { GetCursorPos(&mut p) }.is_ok();
    let _ = saved;
    println!("[hover] 扫描 y={y} x {x1}→{x2}，找 ORBIT_WXHOVER");
    // x1 == x2 时按纵向扫（Dock 贴右边时托盘图标是竖着排的）
    let vertical = x1 == x2;
    let mut cur = if vertical { y } else { x1 };
    let end = if vertical { 1200 } else { x2 };
    while (step > 0 && cur <= end) || (step < 0 && cur >= end) {
        let (px, py) = if vertical { (x1, cur) } else { (cur, y) };
        unsafe {
            let _ = SetCursorPos(px, py);
        }
        std::thread::sleep(Duration::from_millis(320));
        let h = unsafe {
            FindWindowW(
                windows::core::w!("ORBIT_WXHOVER"),
                windows::core::PCWSTR::null(),
            )
        };
        if let Ok(h) = h {
            if unsafe { IsWindowVisible(h).as_bool() } {
                let mut r = windows::Win32::Foundation::RECT::default();
                let _ = unsafe { GetWindowRect(h, &mut r) };
                println!(
                    "[hover] ({px},{py}) 命中：浮窗 hwnd={:?} rect=({},{}) {}x{}",
                    h,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top
                );
                return;
            }
        }
        cur += step;
    }
    println!("[hover] 没找到浮窗");
}

/// 在屏幕坐标上左键单击一次（用 mouse_event，不依赖目标窗口焦点）。
fn click_at(x: i32, y: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    };
    unsafe {
        let _ = SetCursorPos(x, y);
    }
    std::thread::sleep(Duration::from_millis(160));
    unsafe {
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        std::thread::sleep(Duration::from_millis(60));
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    println!("[click] ({x},{y}) 左键单击");
}

/// 后台点击：把屏幕坐标转成目标窗口客户区坐标，直接 PostMessage 一组鼠标消息。
/// 真实光标完全不动，窗口也不需要在前台。
fn post_click(h: HWND, sx: i32, sy: i32) {
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT { x: sx, y: sy };
        if !windows::Win32::Graphics::Gdi::ScreenToClient(h, &mut pt).as_bool() {
            println!("[postclick] ScreenToClient 失败");
            return;
        }
        let lp = ((pt.y as isize) << 16) | (pt.x as isize & 0xFFFF);
        let _ = PostMessageW(h, WM_MOUSEMOVE, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(lp));
        std::thread::sleep(Duration::from_millis(40));
        let _ = PostMessageW(h, WM_LBUTTONDOWN, windows::Win32::Foundation::WPARAM(1), windows::Win32::Foundation::LPARAM(lp));
        std::thread::sleep(Duration::from_millis(70));
        let _ = PostMessageW(h, WM_LBUTTONUP, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(lp));
        println!("[postclick] hwnd={h:?} 屏幕({sx},{sy}) → 客户区({},{})", pt.x, pt.y);
    }
}

fn probe_cells(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    cond: &IUIAutomationCondition,
    select: Option<usize>,
    only: Option<char>,
) {
    for h in wechat_windows_all() {
        let Ok(root) = (unsafe { auto.ElementFromHandle(h) }) else { continue };
        let Ok(arr) = (unsafe { root.FindAll(TreeScope_Descendants, cond) }) else { continue };
        let n = unsafe { arr.Length() }.unwrap_or(0);
        if n == 0 {
            continue;
        }
        let _ = cache;
        println!("窗口 {:?} {} 共 {n} 行", h, state(h));
        for i in 0..n {
            let Ok(e) = (unsafe { arr.GetElement(i) }) else { continue };
            let name = unsafe { e.CurrentName() }.map(|b: BSTR| b.to_string()).unwrap_or_default();
            let inv = unsafe { e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }.is_ok();
            let sel = unsafe {
                e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            };
            let sel_ok = sel.is_ok();
            let is_sel = match &sel {
                Ok(p) => unsafe { p.CurrentIsSelected() }.map(|b| b.as_bool()).unwrap_or(false),
                Err(_) => false,
            };
            let legacy = unsafe {
                e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )
            };
            let legacy_def = match &legacy {
                Ok(p) => unsafe { p.CurrentDefaultAction() }
                    .map(|b: BSTR| b.to_string())
                    .unwrap_or_default(),
                Err(_) => String::from("-"),
            };
            let r = unsafe { e.CurrentBoundingRectangle() }.ok();
            println!(
                "  [{i}] name={} invoke={inv} selItem={sel_ok} isSel={is_sel} legacy={:?} defaultAction={:?} rect={:?}",
                esc(&name),
                legacy.is_ok(),
                legacy_def,
                r
            );
        }
        if let Some(k) = select {
            if (k as i32) < n {
                // 最小化 / 收在托盘时任何动作都可能被 Qt 忽略，先让窗口真正显示
                let iconic0 = unsafe { IsIconic(h).as_bool() };
                let vis0 = unsafe { IsWindowVisible(h).as_bool() };
                if iconic0 || !vis0 {
                    unsafe {
                        let _ = ShowWindow(h, SW_RESTORE);
                    }
                    std::thread::sleep(Duration::from_millis(900));
                    println!("  (窗口原本 iconic={iconic0} vis={vis0}，已 SW_RESTORE)");
                }
                unsafe {
                    if GetForegroundWindow() != h {
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(h);
                        std::thread::sleep(Duration::from_millis(400));
                    }
                }
                println!("  动作前选中行 = {:?}", selected_row(auto, h, cond));
                println!("  动作前行[{k}] = {:?}", row_name(auto, h, cond, k));

                let run = |c: char| only.map_or(true, |o| o.eq_ignore_ascii_case(&c));

                // A: SelectionItem.Select()
                let e = if run('A') { row_elem(auto, h, cond, k) } else { None };
                if let Some(e) = &e {
                    match unsafe {
                        e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                            UIA_SelectionItemPatternId,
                        )
                    } {
                        Ok(p) => println!("  A Select() → {:?}", unsafe { p.Select() }.map(|_| "ok")),
                        Err(x) => println!("  A Select() 无模式 {:?}", x.code()),
                    }
                    std::thread::sleep(Duration::from_millis(900));
                    println!("     → 选中行 {:?}", selected_row(auto, h, cond));
                }

                // B: LegacyIAccessible.DoDefaultAction()（微信自己声明 defaultAction="双击"）
                let e = if run('B') { row_elem(auto, h, cond, k) } else { None };
                if let Some(e) = &e {
                    match unsafe {
                        e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                            UIA_LegacyIAccessiblePatternId,
                        )
                    } {
                        Ok(p) => println!(
                            "  B DoDefaultAction() → {:?}",
                            unsafe { p.DoDefaultAction() }.map(|_| "ok")
                        ),
                        Err(x) => println!("  B DoDefaultAction 无模式 {:?}", x.code()),
                    }
                    std::thread::sleep(Duration::from_millis(900));
                    println!("     → 选中行 {:?}", selected_row(auto, h, cond));
                }

                // C: InvokePattern.Invoke()
                let e = if run('C') { row_elem(auto, h, cond, k) } else { None };
                if let Some(e) = &e {
                    match unsafe {
                        e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                    } {
                        Ok(p) => println!("  C Invoke() → {:?}", unsafe { p.Invoke() }.map(|_| "ok")),
                        Err(x) => println!("  C Invoke 无模式 {:?}", x.code()),
                    }
                    std::thread::sleep(Duration::from_millis(900));
                    println!("     → 选中行 {:?}", selected_row(auto, h, cond));
                    println!("     → 行[{k}] {:?}", row_name(auto, h, cond, k));
                }
            } else {
                println!("  --select {k} 超出范围（共 {n} 行）");
            }
        }
    }
}

fn main() {
    let mut watch = 0u64;
    let mut interval = 3u64;
    let mut max_depth = 8usize;
    let mut tree_always = false;
    let mut allheld = false;
    let mut dump_wins = false;
    let mut popup = 0u64;
    let mut tray = false;
    let mut shot = false;
    let mut probemin = false;
    let mut probe2f = false;
    let mut wakef = false;
    let mut patterns = false;
    let mut select: Option<usize> = None;
    let mut hwnd_arg: Option<isize> = None;
    let mut hunt = 0u64;
    let mut scan: Option<(i32, i32, i32, i32)> = None;
    let mut act_flag = false;
    // --only A|B|C：只跑某一条动作路径（干净隔离测试用）
    let mut only: Option<char> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--watch" => {
                i += 1;
                watch = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60);
            }
            "--interval" => {
                i += 1;
                interval = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(3);
            }
            "--depth" => {
                i += 1;
                max_depth = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(8);
            }
            "--tree" => tree_always = true,
            "--allheld" => allheld = true,
            "--windows" => dump_wins = true,
            "--tray" => tray = true,
            "--shot" => shot = true,
            "--probemin" => probemin = true,
            "--probe2" => probe2f = true,
            "--wake" => wakef = true,
            "--patterns" => patterns = true,
            "--def" => act_flag = true,
            "--sysrestore" => {
                // 走 WM_SYSCOMMAND/SC_RESTORE，让 Qt 自己恢复上次的尺寸和位置
                if let Some(h) = wechat_windows_all().into_iter().next() {
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                            h,
                            windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND,
                            windows::Win32::Foundation::WPARAM(0xF120),
                            windows::Win32::Foundation::LPARAM(0),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(1500));
                    let mut r = windows::Win32::Foundation::RECT::default();
                    let _ = unsafe { GetWindowRect(h, &mut r) };
                    println!("[sysrestore] {:?} state={} rect=({},{} {}x{})", h, state(h),
                        r.left, r.top, r.right - r.left, r.bottom - r.top);
                }
                return;
            }
            "--restore" => {
                let main = wechat_windows_all().into_iter().next();
                if let Some(h) = main {
                    unsafe {
                        let _ = ShowWindow(h, SW_RESTORE);
                        std::thread::sleep(Duration::from_millis(400));
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(h);
                        std::thread::sleep(Duration::from_millis(900));
                        println!("[restore] {:?} state={}", h, state(h));
                        let a2: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) { Ok(x) => x, Err(_) => return };
                        let c2 = match a2.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from("mmui::ChatSessionCell")) { Ok(x) => x, Err(_) => return };
                        let cr = match a2.CreateCacheRequest() { Ok(x) => x, Err(_) => return };
                        probe_cells(&a2, &cr, &c2, None, None);
                    }
                }
                return;
            }
            "--hotkey" => {
                // 直接给 Orbit 胶囊投 WM_HOTKEY，测 hotkey::dispatch 的分发（不用真按键）
                if let Some(id) = args.get(i + 1).and_then(|s| s.parse::<i32>().ok()) {
                    let h = unsafe {
                        FindWindowW(
                            windows::core::w!("ORBIT_CAPSULE"),
                            windows::core::PCWSTR::null(),
                        )
                    };
                    match h {
                        Ok(h) if !h.is_invalid() => {
                            let r = unsafe {
                                PostMessageW(
                                    h,
                                    WM_HOTKEY,
                                    windows::Win32::Foundation::WPARAM(id as usize),
                                    windows::Win32::Foundation::LPARAM(0),
                                )
                            };
                            println!("[hotkey] capsule={h:?} id={id} post={:?}", r.is_ok());
                        }
                        _ => println!("[hotkey] 找不到 ORBIT_CAPSULE"),
                    }
                    std::thread::sleep(Duration::from_millis(800));
                    for w in wechat_windows_all() {
                        println!("  窗口 {:?} state={}", w, state(w));
                    }
                    return;
                }
            }
            "--postclick" => {
                let g = |k: usize| args.get(i + k).and_then(|s| s.parse::<i32>().ok());
                if let (Some(a), Some(b)) = (g(1), g(2)) {
                    let target = wechat_windows_all().into_iter().next();
                    if let Some(h) = target {
                        let mut w = windows::Win32::Foundation::POINT { x: a, y: b };
                        let _ = w;
                        post_click(h, a, b);
                        std::thread::sleep(Duration::from_millis(900));
                    } else {
                        println!("[postclick] 找不到微信窗口");
                    }
                    return;
                }
            }
            "--click" => {
                let g = |k: usize| args.get(i + k).and_then(|s| s.parse::<i32>().ok());
                if let (Some(a), Some(b)) = (g(1), g(2)) {
                    click_at(a, b);
                    std::thread::sleep(Duration::from_millis(1200));
                    for w in wechat_windows_all() {
                        println!("  窗口 {:?} state={}", w, state(w));
                    }
                    return;
                }
            }
            "--only" => {
                i += 1;
                only = args.get(i).and_then(|s| s.chars().next());
            }
            "--grab" => {
                let g = |k: usize| args.get(i + k).and_then(|s| s.parse::<i32>().ok());
                if let (Some(a), Some(b), Some(c), Some(d)) = (g(1), g(2), g(3), g(4)) {
                    let ok = grab_screen(a, b, c, d, "grab.bmp");
                    println!("grab -> {} {}", if ok { "ok" } else { "fail" }, "grab.bmp");
                    return;
                }
            }
            "--show" => {
                // 让微信主窗口可见 2.5s（不激活），让 Orbit 的读取线程抓一次 UIA 树
                let main = wechat_windows_all().into_iter().next();
                if let Some(h) = main {
                    unsafe { let _ = ShowWindow(h, SW_SHOWNA); }
                    println!("[show] 显示 {:?} 2.5s 供读取线程抓 UIA", h);
                    std::thread::sleep(Duration::from_millis(2500));
                    unsafe { let _ = ShowWindow(h, SW_MINIMIZE); }
                    println!("[show] 已最小化（UIA 完整可读）");
                } else {
                    println!("[show] 找不到微信窗口");
                }
                return;
            }
            "--hoverprobe" => {
                let g = |k: usize| args.get(i + k).and_then(|s| s.parse::<i32>().ok());
                if let (Some(a), Some(b), Some(c), Some(d)) = (g(1), g(2), g(3), g(4)) {
                    hover_probe(a, b, c, d);
                    unsafe { CoUninitialize() };
                    return;
                }
            }
            "--hunt" => {
                i += 1;
                hunt = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(30);
            }
            "--scan" => {
                // --scan x1 x2 y step
                let g = |k: usize| args.get(i + k).and_then(|s| s.parse::<i32>().ok());
                if let (Some(a), Some(b), Some(c), Some(d)) = (g(1), g(2), g(3), g(4)) {
                    scan = Some((a, b, c, d));
                    i += 4;
                }
            }
            "--hwnd" => {
                i += 1;
                hwnd_arg = args.get(i).and_then(|s| {
                    let t = s.trim();
                    if let Some(hex) = t.strip_prefix("0x") {
                        isize::from_str_radix(hex, 16).ok()
                    } else {
                        t.parse::<isize>().ok()
                    }
                });
            }
            "--select" => {
                i += 1;
                select = args.get(i).and_then(|s| s.parse().ok());
            }
            "--popupwatch" => {
                i += 1;
                popup = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60);
            }
            _ => {}
        }
        i += 1;
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let auto: IUIAutomation = match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) } {
        Ok(a) => a,
        Err(e) => {
            println!("UIA 初始化失败 {:?}", e.code());
            return;
        }
    };
    let v = VARIANT::from(CELL_CLASS);
    let cond: IUIAutomationCondition = unsafe {
        auto.CreatePropertyCondition(UIA_ClassNamePropertyId, &v)
            .unwrap_or_else(|_| auto.CreateTrueCondition().unwrap())
    };
    let cache = unsafe { auto.CreateCacheRequest() }.expect("cache request");
    let _ = unsafe { cache.AddProperty(UIA_ControlTypePropertyId) };
    let _ = unsafe { cache.AddProperty(UIA_ClassNamePropertyId) };
    let _ = unsafe { cache.AddProperty(UIA_NamePropertyId) };
    let _ = unsafe { cache.AddProperty(UIA_AutomationIdPropertyId) };
    let _ = unsafe { cache.AddProperty(UIA_IsOffscreenPropertyId) };
    let _ = unsafe { cache.SetTreeScope(TreeScope_Subtree) };
    let true_cond = unsafe { auto.CreateTrueCondition() }.unwrap();
    let _ = unsafe { cache.SetTreeFilter(&true_cond) };

    // 树按层遍历要单独的 Children 范围缓存请求：UIA 要求 FindAll 的范围
    // 不能小于 cache request 的 TreeScope，否则直接返回空。
    let cache_kids = unsafe { auto.CreateCacheRequest() }.expect("cache kids");
    let _ = unsafe { cache_kids.AddProperty(UIA_ControlTypePropertyId) };
    let _ = unsafe { cache_kids.AddProperty(UIA_ClassNamePropertyId) };
    let _ = unsafe { cache_kids.AddProperty(UIA_NamePropertyId) };
    let _ = unsafe { cache_kids.AddProperty(UIA_AutomationIdPropertyId) };
    let _ = unsafe { cache_kids.AddProperty(UIA_IsOffscreenPropertyId) };
    let _ = unsafe { cache_kids.SetTreeScope(TreeScope_Children) };
    let _ = unsafe { cache_kids.SetTreeFilter(&true_cond) };

    if hunt > 0 {
        hunt_hover(&auto, &true_cond, hunt);
        unsafe { CoUninitialize() };
        return;
    }
    if let Some((x1, x2, y, step)) = scan {
        scan_hover(&auto, &true_cond, x1, x2, y, step);
        unsafe { CoUninitialize() };
        return;
    }
    if let Some(v) = hwnd_arg {
        let h = HWND(v as *mut std::ffi::c_void);
        println!(
            "窗口 {:?} class={} title={:?} {}",
            h,
            win_class(h),
            win_title(h),
            state(h)
        );
        let mut p = 0u32;
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut p));
        }
        dump_hover_win(&auto, &true_cond, h, p, act_flag);
        let f = format!("hwnd_{:x}.bmp", v as usize);
        if capture(h, &f) {
            println!("截图 -> {f}");
        }
        unsafe { CoUninitialize() };
        return;
    }
    if patterns || select.is_some() {
        probe_cells(&auto, &cache, &cond, select, only);
        unsafe { CoUninitialize() };
        return;
    }
    if wakef {
        wake(&auto, &cache, &cond);
        unsafe { CoUninitialize() };
        return;
    }
    if probe2f {
        probe2(&auto, &cache, &cond);
        unsafe { CoUninitialize() };
        return;
    }
    if probemin {
        probe_minimized(&auto, &cache, &cond);
        unsafe { CoUninitialize() };
        return;
    }
    if tray {
        dump_tray(&auto);
        unsafe { CoUninitialize() };
        return;
    }
    if shot {
        for h in wechat_windows_all() {
            println!("截取 {:?} {} vis={}", h, win_class(h), unsafe { IsWindowVisible(h).as_bool() });
            let name = format!("shot_{:x}.bmp", h.0 as usize);
            if capture(h, &name) {
                println!("  -> {name}");
            }
        }
        unsafe { CoUninitialize() };
        return;
    }
    if popup > 0 {
        println!("===== 新窗口扫描 {}s =====", popup);
        popup_watch(&auto, &cache, &cond, &true_cond, popup);
        unsafe { CoUninitialize() };
        return;
    }
    let opts = Opts { allheld, dump_wins };
    let mut held: Vec<Held> = Vec::new();
    let mut held_all: Vec<HeldAll> = Vec::new();
    if dump_wins {
        println!("===== 微信进程全部窗口清单 {} =====", now());
        dump_all_windows(&auto, &cache, &cond);
    }
    let mut first = true;
    if watch == 0 {
        println!("===== 单轮抓取 {} =====", now());
        round(&auto, &cache, &cache_kids, &cond, &true_cond, true, max_depth, &mut held, &opts, &mut held_all);
    } else {
        let t0 = std::time::Instant::now();
        let mut n = 0;
        println!("===== 持续监测 {}s，每 {}s 一轮 =====", watch, interval);
        while t0.elapsed() < Duration::from_secs(watch) {
            n += 1;
            println!("===== round {n} t={} (+{:.1}s) =====", now(), t0.elapsed().as_secs_f64());
            round(&auto, &cache, &cache_kids, &cond, &true_cond, first || tree_always, max_depth, &mut held, &opts, &mut held_all);
            first = false;
            use std::io::Write;
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_secs(interval));
        }
        println!("===== 监测结束 =====");
    }
    unsafe { CoUninitialize() };
}

fn now() -> String {
    // 用 GetLocalTime 取时间，不引入 chrono
    unsafe {
        let st = windows::Win32::System::SystemInformation::GetLocalTime();
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
        )
    }
}
