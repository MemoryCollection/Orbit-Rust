//! 剪贴板历史：独立线程用 AddClipboardFormatListener 监听变化（无轮询），
//! 只收纯文字（复制的文件、图片一律跳过），内容去重（重复内容移到最前），
//! 固定条目排在最前且跨重启保留，落盘到 <exe>/data/clipboard.txt。
//! 密码管理器等标记为"不要进剪贴板历史"的内容会跳过。

use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage,
    HWND_MESSAGE, MSG, WNDCLASSW,
};

use crate::store;

/// 未固定条目的上限
pub const MAX_ITEMS: usize = 30;
/// 固定条目的上限
pub const MAX_PINNED: usize = 20;
/// 单条最长字符数
pub const MAX_CHARS: usize = 8000;

const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const CLASS: PCWSTR = w!("ORBIT_CLIPBOARD");

#[derive(Clone)]
pub struct Clip {
    pub text: String,
    /// 本地时间 HH:MM
    pub at: String,
    /// 固定：永远排在最前，重启后仍在
    pub pinned: bool,
}

pub struct Clipboard {
    pub items: Vec<Clip>,
    rx: Receiver<Clip>,
}

thread_local! {
    static SENDER: RefCell<Option<Sender<Clip>>> = const { RefCell::new(None) };
}

impl Clipboard {
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("orbit-clipboard".into())
            .spawn(move || listen(tx))
            .ok();
        Self { items: load(), rx }
    }

    /// 取走监听线程新记录；返回 true 表示界面需要重绘。
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(clip) => {
                    if self.push(clip) {
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if changed {
            save(&self.items);
        }
        changed
    }

    /// 切换某一行的固定状态，返回是否重绘（行号 = items 下标）
    pub fn toggle_pin(&mut self, index: usize) -> bool {
        let Some(item) = self.items.get_mut(index) else { return false };
        item.pinned = !item.pinned;
        // 稳定排序：固定的整体前移，各自组内保持原顺序
        self.items.sort_by_key(|c| !c.pinned);
        let pinned = self.items.iter().filter(|c| c.pinned).count();
        if pinned > MAX_PINNED {
            let mut seen = 0;
            self.items.retain(|c| {
                if c.pinned {
                    seen += 1;
                    seen <= MAX_PINNED
                } else {
                    true
                }
            });
        }
        save(&self.items);
        true
    }

    /// 记录一条剪贴板内容；返回 true 表示有变化（需要落盘/重绘）
    fn push(&mut self, clip: Clip) -> bool {
        let text = clip.text.trim();
        if text.is_empty() || text.chars().count() > MAX_CHARS {
            return false;
        }
        // 去重：已有相同内容就把它挪到前面（已固定的保持原位）
        let existing = self.items.iter().position(|c| c.text == text);
        if let Some(i) = existing {
            if self.items[i].pinned {
                self.items[i].at = clip.at;
                return true;
            }
            self.items.remove(i);
        }
        // 插到固定区之后
        let pos = self.items.iter().take_while(|c| c.pinned).count();
        self.items.insert(pos, Clip { text: text.to_string(), at: clip.at, pinned: false });
        // 未固定条目只保留最近 MAX_ITEMS 条
        let mut n = 0;
        self.items.retain(|c| {
            if c.pinned {
                true
            } else {
                n += 1;
                n <= MAX_ITEMS
            }
        });
        true
    }
}

fn listen(tx: Sender<Clip>) {
    unsafe {
        SENDER.with(|s| *s.borrow_mut() = Some(tx));
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            CLASS,
            PCWSTR::null(),
            Default::default(),
            0, 0, 0, 0,
            HWND_MESSAGE,
            None,
            instance,
            None,
        ) else {
            return;
        };
        if AddClipboardFormatListener(hwnd).is_err() {
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CLIPBOARDUPDATE {
            if let Some(text) = read_text(hwnd) {
                let st = GetLocalTime();
                let at = format!("{:02}:{:02}", st.wHour, st.wMinute);
                SENDER.with(|s| {
                    if let Some(tx) = s.borrow().as_ref() {
                        let _ = tx.send(Clip { text, at, pinned: false });
                    }
                });
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

unsafe fn read_text(hwnd: HWND) -> Option<String> {
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
        return None;
    }
    // 其他程序可能正占用剪贴板，稍等重试
    let mut opened = false;
    for _ in 0..5 {
        if OpenClipboard(hwnd).is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return None;
    }
    let out = (|| {
        if excluded() {
            return None;
        }
        // 只收纯文字：复制文件、复制图片都不进历史
        if non_text() {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let memory = HGLOBAL(handle.0);
        let ptr = GlobalLock(memory) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let max = (GlobalSize(memory) / 2).min(MAX_CHARS * 2 + 1);
        let mut len = 0;
        while len < max && *ptr.add(len) != 0 {
            len += 1;
        }
        let text = (len <= MAX_CHARS * 2)
            .then(|| String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
        let _ = GlobalUnlock(memory);
        text
    })();
    let _ = CloseClipboard();
    out
}

// ---------- 只收文字 ----------

/// 常见剪贴板格式号（CF_* 数值；这里写死省得再引一层常量）
const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;

/// 剪贴板里有"非文字"内容时返回 true：复制的文件（CF_HDROP）、图片（DIB/BITMAP/PNG）。
/// Word/Excel 复制文字时会顺手塞一张预览位图，这种情况同时带 RTF 或 CSV，
/// 仍按文字收；纯图片（截图、网页另存图）没有文本格式，直接跳过。
unsafe fn non_text() -> bool {
    if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
        return true;
    }
    let image = IsClipboardFormatAvailable(CF_DIB).is_ok()
        || IsClipboardFormatAvailable(CF_DIBV5).is_ok()
        || IsClipboardFormatAvailable(CF_BITMAP).is_ok()
        || ["PNG", "image/png", "JFIF", "GIF"].iter().any(|n| has_format(n));
    if !image {
        return false;
    }
    !(has_format("Rich Text Format") || has_format("Csv") || has_format("CSV"))
}

unsafe fn has_format(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let f = RegisterClipboardFormatW(PCWSTR(wide.as_ptr()));
    f != 0 && IsClipboardFormatAvailable(f).is_ok()
}

/// 密码管理器 / 远程桌面会给敏感内容加这些格式，表示不要进入剪贴板历史
unsafe fn excluded() -> bool {
    let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    let ignore = RegisterClipboardFormatW(w!("Clipboard Viewer Ignore"));
    if (exclude != 0 && IsClipboardFormatAvailable(exclude).is_ok())
        || (ignore != 0 && IsClipboardFormatAvailable(ignore).is_ok())
    {
        return true;
    }
    let history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
    if history == 0 || IsClipboardFormatAvailable(history).is_err() {
        return false;
    }
    let Ok(handle) = GetClipboardData(history) else { return true };
    let memory = HGLOBAL(handle.0);
    let ptr = GlobalLock(memory) as *const u32;
    if ptr.is_null() {
        return true;
    }
    let allowed = GlobalSize(memory) >= 4 && *ptr != 0;
    let _ = GlobalUnlock(memory);
    !allowed
}

// ---------- 持久化 ----------
// 每行：`时间 \x1f 固定标记 \x1f 文本`，文本内换行转义为 \x1e。
// 固定标记为 1/0；老格式（只有两个字段）按未固定读入。

fn store_path() -> Option<std::path::PathBuf> {
    Some(store::path("clipboard.txt"))
}

fn load() -> Vec<Clip> {
    let Some(path) = store_path() else { return Vec::new() };
    let Ok(raw) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut items: Vec<Clip> = raw
        .lines()
        .filter_map(|line| {
            let (at, rest) = line.split_once('\x1f')?;
            let (pinned, text) = match rest.split_once('\x1f') {
                Some((flag, text)) => (flag.trim() == "1", text),
                None => (false, rest),
            };
            Some(Clip { text: text.replace('\x1e', "\n"), at: at.to_string(), pinned })
        })
        .collect();
    // 老文件可能不是固定优先顺序
    items.sort_by_key(|c| !c.pinned);
    items
}

fn save(items: &[Clip]) {
    let Some(path) = store_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body: String = items
        .iter()
        .map(|c| {
            format!("{}\x1f{}\x1f{}\n", c.at, if c.pinned { 1 } else { 0 }, c.text.replace('\n', "\x1e"))
        })
        .collect();
    let _ = std::fs::write(path, body);
}

/// 列表/胶囊上的一行预览：换行与连续空白压成一个空格
pub fn preview(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (i, c) in joined.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// 把文本写回剪贴板（面板里点某一行时用）
pub fn set_text(text: &str) {
    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let data: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = data.len() * 2;
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
            let p = GlobalLock(h) as *mut u8;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, p, bytes);
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0));
            }
        }
        let _ = CloseClipboard();
    }
}
