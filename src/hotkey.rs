//! 微信实例热键：把设置里的 `wechat_hotkeys`（`F1,F2` / `Ctrl+Alt+1,…`）注册成
//! 全局热键，按实例创建先后一一对应。动作只有「前台显示 ↔ 最小化」，**从不 hide**。
//!
//! 为什么必须只做最小化：微信一旦真的隐藏到托盘（`IsWindowVisible=false`），Qt 会
//! 把整棵部件树标记不可见并从 UIA 树里摘掉；外部 `ShowWindow` 只能改 Win32 可见性，
//! 没法让 Qt 重新 `show()`，于是会话 Cell 永远读不到（2026-11-23 实测：SW_RESTORE /
//! SW_SHOWMINNOACTIVE 全部返回 0 行）。最小化则是另一回事——窗口依旧可见，树完整。
//!
//! 热键 id = `HOTKEY_BASE + 实例序号`，`window.rs` 的 WM_HOTKEY 按同一约定分发。
//! 设置一改（手改文件或设置页）就整组重装：先注销旧 id 再注册，删项能立刻释放。

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
    MOD_WIN,
};

/// WM_HOTKEY 的起始 id（`window.rs` 用 `id - HOTKEY_BASE` 取实例序号）
pub const HOTKEY_BASE: usize = 100;

/// 条目支持「热键=名称关键词」：`F1=吴东` 表示绑给窗口标题含「吴东」的那个微信，
/// 不再依赖启动先后。返回 (热键部分, Some(标题关键词))；没写「=」返回 (原文, None)。
pub fn split_binding(entry: &str) -> (&str, Option<&str>) {
    match entry.split_once('=') {
        Some((spec, name)) => {
            let name = name.trim();
            (spec.trim(), (!name.is_empty()).then_some(name))
        }
        None => (entry.trim(), None),
    }
}

/// 已注册成功的 id（重装前逐个注销；注册失败的 id 不放进来）
static REGISTERED: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// 解析一条热键文本：`F1` / `Shift+F2` / `Ctrl+Alt+1` / `Ctrl+Alt+O`。
/// 修饰键可写 ctrl/control、alt、shift、win/super（大小写不敏感），最多四个组合。
fn parse(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk: Option<u32> = None;
    for part in spec.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let low = part.to_ascii_lowercase();
        match low.as_str() {
            "ctrl" | "control" => mods = mods | MOD_CONTROL,
            "alt" => mods = mods | MOD_ALT,
            "shift" => mods = mods | MOD_SHIFT,
            "win" | "super" | "meta" => mods = mods | MOD_WIN,
            _ => {
                // 主键：F1~F24 或单个字母/数字
                let v = if low.len() >= 2 && low.starts_with('f') {
                    low[1..]
                        .parse::<u32>()
                        .ok()
                        .filter(|n| (1..=24).contains(n))
                        .map(|n| 0x6F + n)
                } else if part.chars().count() == 1 {
                    let c = part.chars().next()?;
                    if c.is_ascii_alphanumeric() {
                        Some(c.to_ascii_uppercase() as u32)
                    } else {
                        None
                    }
                } else {
                    None
                };
                // 只接受一个主键；出现第二个说明写法有问题
                if v.is_none() || vk.is_some() {
                    return None;
                }
                vk = v;
            }
        }
    }
    vk.map(|v| (mods, v))
}

/// 只验证写法是否认识，不真正注册（状态显示用）
pub fn parse_for_check(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    parse(spec)
}

/// 按当前设置重装全部微信热键。`hwnd` 是胶囊窗口（热键消息投给它）。
pub fn apply(hwnd: HWND) {
    if hwnd.is_invalid() {
        return;
    }
    // 先注销上一组
    if let Ok(mut v) = REGISTERED.lock() {
        for id in v.drain(..) {
            let _ = unsafe { UnregisterHotKey(hwnd, id) };
        }
    } else {
        return;
    }
    let list = crate::settings::wechat_hotkeys();
    if list.is_empty() {
        return;
    }
    let mut ok: Vec<i32> = Vec::new();
    for (i, entry) in list.iter().enumerate() {
        let (spec, binding) = split_binding(entry);
        let Some((mods, vk)) = parse(spec) else {
            crate::tlog!("[hotkey] 写法不认识，跳过：{:?}", spec);
            crate::banner::show(
                crate::banner::BannerNotice::info(
                    "微信热键",
                    &format!("微信{} 热键「{}」写法不认识", i + 1, spec),
                    "支持 F1~F24、Ctrl/Alt/Shift/Win 组合；按名称绑写 F1=名称关键词",
                )
                .with_secs(4),
            );
            continue;
        };
        let id = (HOTKEY_BASE + i) as i32;
        let who = match binding {
            Some(n) => format!("微信{}「{}」", i + 1, n),
            None => format!("微信{}", i + 1),
        };
        match unsafe { RegisterHotKey(hwnd, id, mods, vk) } {
            Ok(()) => {
                ok.push(id);
                crate::tlog!("[hotkey] 微信{} -> {} 注册成功", i + 1, entry);
                let sub = match binding {
                    Some(n) => format!("按 {} 唤醒标题含「{}」的微信", spec, n),
                    None => format!("按 {} 显示/最小化第 {} 个启动的微信", spec, i + 1),
                };
                crate::banner::show(
                    crate::banner::BannerNotice::info(
                        "微信热键",
                        &format!("{} ← {} 注册成功", who, spec),
                        &sub,
                    )
                    .with_secs(3),
                );
            }
            Err(e) => {
                // 最常见原因：被别的程序（或微信自己的快捷键）占着
                crate::tlog!(
                    "[hotkey] 微信{} -> {} 注册失败（可能已被占用）code={:?}",
                    i + 1,
                    entry,
                    e.code()
                );
                crate::banner::show(
                    crate::banner::BannerNotice::info(
                        "微信热键",
                        &format!("{} ← {} 注册失败", who, spec),
                        "热键被占用（多半是微信自己的快捷键），请到微信设置清空后重启 Orbit",
                    )
                    .with_secs(5),
                );
            }
        }
    }
    if !ok.is_empty() {
        if let Ok(mut v) = REGISTERED.lock() {
            *v = ok;
        }
    }
}

/// 热键注册状态（设置页/诊断用）：(实例序号, 热键文本, 是否注册成功)
pub fn status() -> Vec<(usize, String, bool)> {
    let list = crate::settings::wechat_hotkeys();
    let ok = REGISTERED.lock().map(|v| v.clone()).unwrap_or_default();
    list.iter()
        .enumerate()
        .map(|(i, s)| (i, s.clone(), ok.contains(&((HOTKEY_BASE + i) as i32))))
        .collect()
}
