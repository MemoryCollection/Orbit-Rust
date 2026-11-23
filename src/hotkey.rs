//! 微信实例热键：把设置里的热键注册成全局热键，按下时「前台显示 ↔ 最小化」，
//! **从不 hide**。
//!
//! 为什么必须只做最小化：微信一旦真的隐藏到托盘（`IsWindowVisible=false`），Qt 会
//! 把整棵部件树标记不可见并从 UIA 树里摘掉；外部 `ShowWindow` 只能改 Win32 可见性，
//! 没法让 Qt 重新 `show()`，于是会话 Cell 永远读不到（2026-11-23 实测：SW_RESTORE /
//! SW_SHOWMINNOACTIVE 全部返回 0 行）。最小化则是另一回事——窗口依旧可见，树完整。
//!
//! 两条绑定通道，注册时**账号绑定优先**：
//!
//! 1. `wechat_hotkey_bind`：`账号ID=热键`，绑到账号快照（设置页账号列表每行直接填）。
//!    跟账号走，微信换启动顺序、换昵称都不会串；按下时现查该账号此刻的 pid。
//! 2. `wechat_hotkeys`：按启动先后排的热键列表，给没有快照的实例兜底。
//!    每项可写 `F1=标题关键词`（老语法，按窗口标题匹配）。
//!
//! 热键 id = `HOTKEY_BASE + 计划序号`，`window.rs` 的 WM_HOTKEY 交给 [`dispatch`]
//! 按同一份计划分发。设置一改（手改文件或设置页）就整组重装：先注销旧 id 再注册，
//! 删项能立刻释放。

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
    MOD_WIN,
};

/// WM_HOTKEY 的起始 id（本模块内部按注册计划序号分配）
pub const HOTKEY_BASE: usize = 100;

/// 一个热键槽绑给谁
#[derive(Clone)]
enum Target {
    /// 账号快照 id：按下时现查该账号的 pid
    Account(String),
    /// 窗口标题关键词（老语法 `F1=名称`），按标题包含匹配
    Named(String),
    /// 按启动顺序的第 n 个实例（0 起）
    Slot(usize),
}

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

/// 已注册成功的 (热键 id, 目标)（重装前逐个注销；注册失败的 id 不放进来）
static REGISTERED: Mutex<Vec<(i32, Target)>> = Mutex::new(Vec::new());

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
    let old: Vec<i32> = match REGISTERED.lock() {
        Ok(mut v) => v.drain(..).map(|(id, _)| id).collect(),
        Err(_) => return,
    };
    for id in old {
        let _ = unsafe { UnregisterHotKey(hwnd, id) };
    }

    // 组装注册计划：① 账号绑定（精确，优先占热键）② 启动顺序槽位（兜底）
    let mut plan: Vec<(String, Target, String)> = Vec::new();
    let snaps = crate::wechatsnap::list();
    for (acc, spec) in crate::settings::wechat_hotkey_binds() {
        let spec = spec.trim().to_string();
        if spec.is_empty() {
            continue;
        }
        let nick = snaps
            .iter()
            .find(|s| s.id == acc)
            .map(|s| s.nickname.clone())
            .unwrap_or_else(|| acc.clone());
        plan.push((spec, Target::Account(acc), nick));
    }
    for (i, entry) in crate::settings::wechat_hotkeys().iter().enumerate() {
        let (spec, bind) = split_binding(entry);
        if spec.is_empty() {
            continue;
        }
        let target = match bind {
            Some(name) => Target::Named(name.to_string()),
            None => Target::Slot(i),
        };
        plan.push((spec.to_string(), target, format!("微信{}", i + 1)));
    }
    if plan.is_empty() {
        return;
    }

    let mut ok: Vec<(i32, Target)> = Vec::new();
    // 已占用的热键（规范化文本），避免账号绑定与顺序表抢同一个键
    let mut used: Vec<String> = Vec::new();

    for (i, (spec, target, who)) in plan.into_iter().enumerate() {
        let id = (HOTKEY_BASE + i) as i32;
        let key = spec.to_ascii_lowercase().replace(' ', "");
        if used.contains(&key) {
            crate::tlog!("[hotkey] {who} 热键 {spec:?} 与前一条重复，跳过");
            continue;
        }
        let Some((mods, vk)) = parse(&spec) else {
            crate::tlog!("[hotkey] {who} 热键写法不认识，跳过：{spec:?}");
            crate::banner::show(
                crate::banner::BannerNotice::info(
                    "微信热键",
                    &format!("{who} 热键「{spec}」写法不认识"),
                    "支持 F1~F24、Ctrl/Alt/Shift/Win 组合",
                )
                .with_secs(4),
            );
            continue;
        };
        match unsafe { RegisterHotKey(hwnd, id, mods, vk) } {
            Ok(()) => {
                used.push(key);
                ok.push((id, target));
                crate::tlog!("[hotkey] {who} -> {spec} 注册成功");
            }
            Err(e) => {
                // 最常见原因：被别的程序（或微信自己的快捷键）占着
                crate::tlog!(
                    "[hotkey] {who} -> {spec} 注册失败（可能已被占用）code={:?}",
                    e.code()
                );
                crate::banner::show(
                    crate::banner::BannerNotice::info(
                        "微信热键",
                        &format!("{who} ← {spec} 注册失败"),
                        "热键被占用（多半是微信自己的快捷键），请到微信设置清空后重启 Orbit",
                    )
                    .with_secs(5),
                );
            }
        }
    }

    if let Ok(mut v) = REGISTERED.lock() {
        *v = ok;
    }
}

/// WM_HOTKEY 分发：按注册计划找到这个 id 该唤谁。
pub fn dispatch(id: i32) {
    let target = REGISTERED
        .lock()
        .ok()
        .and_then(|v| v.iter().find(|(i, _)| *i == id).map(|(_, t)| t.clone()));
    match target {
        Some(Target::Account(acc)) => crate::wechatmsg::toggle_account(&acc),
        Some(Target::Named(kw)) => crate::wechatmsg::toggle_named(&kw),
        Some(Target::Slot(n)) => crate::wechatmsg::toggle(n),
        // 没注册成功（被占用 / 写法不认识）或已注销
        None => crate::tlog!("[hotkey] id={id} 没有对应的已注册热键"),
    }
}

/// 顺序槽位的注册状态（诊断用）：(槽位下标, 热键文本, 是否注册成功)
pub fn status() -> Vec<(usize, String, bool)> {
    let list = crate::settings::wechat_hotkeys();
    let ok = REGISTERED.lock().map(|v| v.clone()).unwrap_or_default();
    list.iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let (spec, bind) = split_binding(entry);
            if spec.is_empty() || bind.is_some() {
                // 空槽 / 按名称绑的那条不算"第 i 个实例"的热键
                return None;
            }
            let id = (HOTKEY_BASE + i) as i32;
            let hit = ok.iter().any(|(x, t)| *x == id && matches!(t, Target::Slot(n) if *n == i));
            Some((i, spec.to_string(), hit))
        })
        .collect()
}
