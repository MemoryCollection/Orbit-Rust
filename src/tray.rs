//! 托盘镜像（Win11 XAML 托盘）：UIA 枚举 Explorer 的 `NotifyItemIcon` 按钮，
//! 图标优先取程序 exe 里的 256px 大图标（从任务栏抠 16px 再放大太糊），
//! 左键走 `LegacyIAccessible.DoDefaultAction`，右键/悬停转给真实图标。

use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR, PWSTR, VARIANT};
use windows::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern,
    TreeScope_Children, TreeScope_Descendants, UIA_AutomationIdPropertyId, UIA_ButtonControlTypeId,
    UIA_ControlTypePropertyId, UIA_ImageControlTypeId, UIA_LegacyIAccessiblePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetAncestor, GetCursorPos, GetWindowRect, SetCursorPos,
    WindowFromPoint, ASFW_ANY, GA_ROOT,
};

use crate::appicon::IconCache;
use crate::config as cfg;
use crate::trayhost;

/// 图标像素的刷新周期：位置签名没变也要定期重抓（应用会换图标，比如静音/未读角标）
const ICON_TTL: Duration = Duration::from_secs(20);
/// 托盘按钮的 AutomationId（应用注册的托盘图标；系统图标用 SystemTrayIcon）
const AID_NOTIFY_ITEM: &str = "NotifyItemIcon";
/// PrintWindow 的 PW_RENDERFULLCONTENT（让 DWM 直接把合成结果画进位图）
const PW_RENDERFULLCONTENT: u32 = 2;

pub struct TrayItem {
    /// tooltip 首行（悬停提示 / ORBIT_TRACE 日志用）
    pub name: String,
    /// 图标所属进程的可执行文件（小写全路径）。Win11 的托盘按钮挂在 explorer 的
    /// XAML 树上时拿到的就是 explorer.exe，调用方需要再用名字兜底匹配。
    pub exe: String,
    /// 图标像素（straight RGBA，边长 = px）
    pub rgba: Vec<u8>,
    /// 图标来源：`exe`=程序大图标，`raw`=任务栏抓图，`dot`=占位（仅日志用）
    pub src: &'static str,
    /// 这一刻真实托盘上的图标是否正在"灭"（应用用闪烁提示新消息）
    pub dim: bool,
    /// 最近一次看到"灭帧"的时刻：胶囊用它判断"这个图标正在闪烁"（新消息提醒）。
    /// 一直亮着的图标是 None；灭帧之后如果一直没再闪，靠 `cfg::FLASH_WINDOW_MS` 过期。
    pub flash_at: Option<Instant>,
    /// 图标"亮"的时候的不透明像素占比，闪烁判定拿它当参考，缓慢向当前值衰减
    ink_hi: f32,
    /// 托盘按钮元素，点击时用它的 LegacyIAccessible（UIA 模式）
    element: Option<IUIAutomationElement>,
    /// 托管模式下这条图标的注册数据（含回调消息/GUID/版本），Dock 直接拿它当目标
    pub host: Option<trayhost::Icon>,
    /// 图标本身的屏幕矩形（不是按钮矩形：按钮是 32×48 的整条）
    pub(crate) icon_rect: RECT,
}

impl TrayItem {
    /// 跨会话稳定的标识，设置页用它记住"这个图标被单独隐藏了"：
    /// 有 GUID 用 GUID；否则 exe + 应用自己的 uID；再不行退回名字。
    /// 不用 hWnd（每次启动都变），也不用 tooltip（会随状态变）。
    pub fn key(&self) -> String {
        icon_key(self.host.as_ref(), &self.exe, &self.name)
    }
}

/// 托盘图标的稳定键（详见 `TrayItem::key`）
pub fn icon_key(host: Option<&trayhost::Icon>, exe: &str, name: &str) -> String {
    if let Some(h) = host {
        if h.guid != windows::core::GUID::default() {
            return format!("g:{:?}", h.guid);
        }
        if !exe.is_empty() {
            return format!("e:{}#{}", exe.to_lowercase(), h.uid);
        }
        return format!("n:{}#{}", name.trim(), h.uid);
    }
    format!("n:{}|{}", exe.to_lowercase(), name.trim())
}

pub struct TrayMirror {
    pub items: Vec<TrayItem>,
    /// 托盘托管共享表（Some = 走托管路径，不再依赖 UIA）
    hosted: Option<std::sync::Arc<std::sync::Mutex<trayhost::State>>>,
    /// 上次看到的托管表版本号
    host_gen: u64,
    /// 托管模式：最近一次成功抓到的图标像素，按 (hwnd, uid, guid) 缓存。
    /// 灭帧 hicon=0 或跨进程抓图偶发失败时沿用，确保亮帧一定能画出来。
    host_rgba: Vec<(u32, u32, String, Vec<u8>)>,
    /// 第一次用到时才创建（UIA 初始化约几毫秒，无托盘需求时不付这个代价）
    automation: Option<IUIAutomation>,
    /// 上次的 (图标数, 每个按钮的屏幕矩形) 签名
    last_sig: Vec<(i32, i32, i32, i32)>,
    /// 上次抓图标像素的时刻
    last_capture: Option<Instant>,
    /// 连续扫到空结果的次数（XAML 重建期间会短暂没有元素，不能立刻清空）
    empty_hits: u8,
    /// tooltip 名字 → 程序 exe → 256px 大图标
    icons: IconCache,
    /// 图标稳定键的首次出现顺序：位置一旦出现就固定，重注册（闪烁时的
    /// DELETE+ADD）不挪位；新图标排在同类（同 exe）实例后面，没有同类就排末尾。
    /// 多开微信因此默认相互挨着、先开的在前、后开的在后。
    order: Vec<String>,
}

impl Default for TrayMirror {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            hosted: trayhost::table(),
            host_gen: u64::MAX,
            host_rgba: Vec::new(),
            automation: None,
            last_sig: Vec::new(),
            last_capture: None,
            empty_hits: 0,
            icons: IconCache::default(),
            order: Vec::new(),
        }
    }
}

impl TrayMirror {
    /// 让托盘托管接入（有托管表就走直接数据源，不再依赖 UIA）。
    pub fn attach_host(&mut self) {
        self.hosted = trayhost::table();
    }

    /// 是否走托盘托管数据源
    pub fn hosted(&self) -> bool {
        self.hosted.is_some()
    }

    /// 丢掉所有已缓存的图标像素（图标尺寸变化后按新尺寸重抓）。
    ///
    /// 托管路径的缓存没有 px 维度，所以只能整体清空；清完把 `host_gen` 设成
    /// 不可能的值，下一次 `poll` 就会整表重建。
    pub fn invalidate(&mut self) {
        self.icons.clear();
        self.host_rgba.clear();
        self.last_sig.clear();
        self.last_capture = None;
        self.host_gen = u64::MAX;
    }

    /// 轮询托盘变化：托管模式走 NOTIFYICONDATA 表，否则走 UIA。
    pub fn poll(&mut self, icon_px: i32) -> bool {
        if self.hosted.is_some() {
            return self.poll_hosted(icon_px);
        }
        let Some(auto) = self.automation() else { return false };
        let Some(tb) = taskbar() else { return false };
        let found = unsafe { find_icons(&auto, tb) };
        if found.is_empty() && !self.items.is_empty() && self.empty_hits < 3 {
            // 交互/重建期间 UIA 会短暂返回空，先留着上一批，连续三次才认定托盘真的空了
            self.empty_hits += 1;
            trace("扫描为空，保留上一批");
            return false;
        }
        if !found.is_empty() {
            self.empty_hits = 0;
        }
        let sig: Vec<(i32, i32, i32, i32)> = found
            .iter()
            .map(|(_, r)| (r.left, r.top, r.right, r.bottom))
            .collect();
        // 上一次扫描如果是空的（启动时 XAML 还没建好），签名叫"没变化"，会一直卡在空状态下，
        // 所以空状态每次都要重试
        let stale = sig != self.last_sig || (found.is_empty() && self.items.is_empty());
        let due = self.last_capture.map_or(true, |t| t.elapsed() >= ICON_TTL);
        if !stale && !due {
            return false;
        }
        // 鼠标真的停在任务栏的那些图标上时，PrintWindow 会把悬停底色一起拍进来，
        // 抠图后留一个灰圈。这一轮不换像素，等鼠标离开再抓。
        // 注意：只能用 WindowFromPoint 判断——任务栏被做成透明后，它的屏幕矩形
        // 覆盖了整条屏幕底部，光标停在自己的 Dock 上也会落在矩形里。
        let blocked = cursor_over_tray(tb);
        if blocked {
            // 已经有这一批的像素就先留着；一次都没抓到过（启动时）用占位点把槽位撑起来，
            // 保证位置和点击仍然正确，下一次轮询再重试抓图
            if !self.items.is_empty() && sig == self.last_sig && self.last_capture.is_some() {
                trace("鼠标停在任务栏图标上，保留上一批像素");
                return false;
            }
            trace("鼠标停在任务栏图标上，本轮用占位像素");
        }

        // 只有真的需要换像素时才抓一次整条任务栏（约 1~3ms）
        let surface = if found.is_empty() || blocked { None } else { unsafe { capture_taskbar(tb) } };
        let mut items = Vec::with_capacity(found.len());
        for (element, rect) in found {
            let name = unsafe { element.CurrentName() }
                .map(|b| b.to_string())
                .unwrap_or_default();
            let name = name.lines().next().unwrap_or("").trim().to_string();
            let pid = unsafe { element.CurrentProcessId() }.unwrap_or(0);
            let exe = exe_of_pid(pid.max(0) as u32);
            let icon_rect = unsafe { icon_rect_of(&auto, &element).unwrap_or(rect) };
            // 调试：看看 UIA 会不会把真正的宿主窗口露出来（能拿到就能直接按 v0/3/4 协议投消息）
            let native = unsafe { element.CurrentNativeWindowHandle() }.map(|h| h.0 as usize).unwrap_or(0);
            trace(&format!(
                "{name}：NativeWindowHandle=0x{native:X} pid={pid} class={:?}",
                unsafe { element.CurrentClassName() }.map(|b| b.to_string()).unwrap_or_default()
            ));
            // 1) 任务栏上那 16px 的原始像素：糊，但一定是这个图标本身。
            //    注意可能是"闪烁熄灭"那一帧（整块空白）——那种不能当图标存下来
            let cap = surface
                .as_ref()
                .map(|s| s.icon(icon_rect, icon_px))
                .filter(|v| coverage(v) > 0.03);
            // 2) 程序 exe 里的 256px 大图标：清晰，但得先确认两者是同一个图标
            //    （输入法这类"中/英状态"图标跟 exe 图标完全不是一回事，硬换就错了）
            let own = if cfg::TRAY_USE_EXE_ICON {
                self.icons.icon_for(&name, icon_px)
            } else {
                None
            };
            let (rgba, src) = match (cap, own) {
                (Some(cap), Some((own_exe, big))) => {
                    let diff = icon_diff(&cap, &big);
                    if diff <= cfg::TRAY_ICON_MAX_DIFF {
                        (big, "exe")
                    } else {
                        trace(&format!(
                            "{name}：exe 图标对不上（差 {diff:.2}，{own_exe}），保留抓图"
                        ));
                        (cap, "raw")
                    }
                }
                (Some(cap), None) => (cap, "raw"),
                (None, Some((_, big))) => (big, "exe"),
                (None, None) => (placeholder(icon_px), "dot"),
            };
            items.push(TrayItem {
                name,
                exe,
                rgba,
                src,
                dim: false,
                flash_at: None,
                ink_hi: 0.0,
                element: Some(element),
                host: None,
                icon_rect,
            });
        }

        trace(&format!(
            "重建 {} 个图标：{}",
            items.len(),
            items
                .iter()
                .map(|i| format!(
                    "{}({}·{} @{},{})",
                    i.name,
                    short_exe(&i.exe),
                    i.src,
                    i.icon_rect.left,
                    i.icon_rect.top
                ))
                .collect::<Vec<_>>()
                .join(" / ")
        ));
        self.items = items;
        self.stabilize();
        self.last_sig = sig;
        // 占位像素不算"抓过"，下一轮继续重试（否则会一直停在灰点上）
        if !blocked {
            self.last_capture = Some(Instant::now());
        }
        true
    }

    /// 托盘托管路径：图标来自 NOTIFYICONDATA 表（hIcon/tooltip/回调消息/GUID/版本），
    /// 完全不碰 UIA/PrintWindow——真任务栏怎么隐藏都不影响。托管表版本号变化才重建。
    fn poll_hosted(&mut self, icon_px: i32) -> bool {
        // 顺手清掉窗口已销毁的幽灵图标（任务管理器每次刷新 CPU 都会漏一条）
        crate::trayhost::prune_dead_icons();
        let Some(tab) = self.hosted.clone() else { return false };
        let (gen, icons) = {
            let st = tab.lock().unwrap();
            (st.generation, st.icons.clone())
        };
        if gen == self.host_gen {
            // 版本没变，但闪烁标志可能变了（NIM_MODIFY 也会 +gen，正常都会进上面重建）
            return false;
        }
        self.host_gen = gen;

        // 整表重建会把上一批的闪烁时刻冲掉：按稳定键把仍在窗口内的带过来
        // （灭帧之后一直亮着的图标，2.5s 后自然过期，不会永远"闪烁"）
        let prev_flash: std::collections::HashMap<String, Option<Instant>> = self
            .items
            .iter()
            .map(|it| (it.key(), it.flash_at))
            .collect();

        let mut items = Vec::with_capacity(icons.len());
        for ic in &icons {
            // 1) hIcon 直接绘制（跨进程 USER 对象，最准）。灭帧 hicon=0 或跨进程
            // 抓图偶发失败时沿用上一次成功抓到的像素，否则会掉到 placeholder（暗点），
            // 表现就是"图标消失后不再出现"。亮帧一定能从缓存恢复。
            let g = format!("{:?}", ic.guid);
            let hit = |s: &Vec<(u32, u32, String, Vec<u8>)>| {
                s.iter()
                    .find(|(hwnd, uid, guid, _)| *hwnd == ic.hwnd && *uid == ic.uid && *guid == g)
                    .map(|(_, _, _, rgba)| rgba.clone())
            };
            let raw = if ic.hicon != 0 {
                let hicon =
                    windows::Win32::UI::WindowsAndMessaging::HICON(ic.hicon as *mut core::ffi::c_void);
                // 按图标**原生尺寸**抓像素，再自己缩到 Dock 尺寸：让 DrawIconEx 直接画到
                // 目标尺寸，GDI 会用最粗糙的放大，图标一放大就糊。
                let native = crate::render::icon_native_size(hicon)
                    .filter(|n| *n >= 8)
                    .unwrap_or(icon_px);
                // 注意：DrawIconEx 的**目标尺寸正好等于图标原生尺寸**时会走"直接贴掩码"
                // 的分支，取回来是一块纯黑（alpha=255、RGB=0）。只要尺寸不同就会走
                // 缩放绘制，颜色是对的——所以先放大抓一次，再由我们缩到目标尺寸。
                let cap = (native * 4).clamp(icon_px.max(native + 1), 192);
                match crate::render::capture_icon_rgba(hicon, cap) {
                    Some(rgba) => {
                        let rgba = fit_icon(&rgba, cap, icon_px);
                        let cov = coverage(&rgba);
                        let mid = rgba.chunks_exact(4).nth((rgba.len() / 8).max(0)).map(|p| (p[0], p[1], p[2], p[3]));
                        crate::tlog!(
                            "[tray] {} native={native} cap={cap} px={icon_px} cov={cov:.2} mid={mid:?}",
                            ic.tip
                        );
                        if let Some(slot) = self.host_rgba.iter_mut().find(|(hwnd, uid, guid, _)| {
                            *hwnd == ic.hwnd && *uid == ic.uid && *guid == g
                        }) {
                            slot.3 = rgba.clone();
                        } else {
                            self.host_rgba.push((ic.hwnd, ic.uid, g.clone(), rgba.clone()));
                        }
                        Some(rgba)
                    }
                    None => hit(&self.host_rgba),
                }
            } else {
                hit(&self.host_rgba)
            };
            // 2) exe 大图标更清晰；名字非空且与 raw 相似才换（输入法状态图标会对不上）
            let own = self.icons.icon_for(&ic.tip, icon_px);
            let (rgba, src) = match (raw, own) {
                (Some(raw), Some((_exe, big))) if icon_diff(&raw, &big) <= cfg::TRAY_ICON_MAX_DIFF => {
                    (big, "exe")
                }
                (Some(raw), _) => (raw, "hicon"),
                (None, Some((_exe, big))) if !ic.tip.is_empty() => (big, "exe"),
                (None, _) => (placeholder(icon_px), "dot"),
            };
            // 图标"应该"在Dock上的矩形：先留空，Dock 重画后用真实槽位覆盖（set_placements）
            let icon_rect = RECT::default();
            trace(&format!(
                "托管 ADD {} exe={} hwnd={:#x} msg={:#x} v{} guid={:?} src={src}",
                ic.tip, short_exe(&ic.exe), ic.hwnd, ic.msg, ic.ver, ic.guid
            ));
            items.push(TrayItem {
                name: ic.tip.clone(),
                exe: ic.exe.clone(),
                rgba,
                src,
                dim: ic.dim,
                // 这帧是"灭"= 正在闪烁；亮帧沿用上一批的闪烁时刻（窗口内仍算闪烁）
                flash_at: if ic.dim {
                    Some(Instant::now())
                } else {
                    prev_flash.get(&icon_key(Some(ic), &ic.exe, &ic.tip)).copied().flatten()
                },
                ink_hi: 0.0,
                element: None,
                host: Some(ic.clone()),
                icon_rect,
            });
        }
        self.items = items;
        self.stabilize();
        true
    }

    /// 按首次出现顺序稳定图标位置。
    ///
    /// 托管表按注册顺序排（NIM_ADD push 到末尾），应用重注册一次就挪到最后——
    /// 表现为图标来回跳。这里按稳定键记住首次出现顺序：
    /// - 老图标位置固定，重注册 / 图标刷新不挪位；
    /// - 新图标插到**同 exe（进程名相同）的最后一个实例**后面：多开微信默认
    ///   相互挨着，先开的在前、新创建的在后；
    /// - 没有同类的全新图标追加到末尾；
    /// - 消失的图标从顺序表里去掉（重启 Orbit 后顺序重新从零记录）。
    fn stabilize(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let mut order = std::mem::take(&mut self.order);
        let base = |exe: &str| {
            exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_string()
        };
        // 键 → 进程名，给"插到同类后面"用
        let key_base: std::collections::HashMap<String, String> =
            self.items.iter().map(|it| (it.key(), base(&it.exe))).collect();
        for it in &self.items {
            let k = it.key();
            if order.iter().any(|o| o == &k) {
                continue;
            }
            let my_base = base(&it.exe);
            let mut at = order.len();
            for (pos, o) in order.iter().enumerate().rev() {
                if key_base.get(o).map(|b| *b == my_base).unwrap_or(false) {
                    at = pos + 1;
                    break;
                }
            }
            order.insert(at, k);
        }
        // 丢掉已消失的键
        let alive: std::collections::HashSet<String> =
            self.items.iter().map(|it| it.key()).collect();
        order.retain(|k| alive.contains(k));
        // 稳定排序：order 里的键按其下标，重复键（键冲突的极端情况）保持原相对顺序
        self.items
            .sort_by_key(|it| order.iter().position(|k| *k == it.key()).unwrap_or(usize::MAX));
        self.order = order;
    }

    /// 快速采样：抓一次任务栏，看每个托盘图标此刻是"亮"还是"灭"。
    /// 应用用"图标一闪一闪"提示新消息（微信就是这样），Dock 直接跟着这个真实状态走，
    /// 频率自然和系统一致，不用去猜周期。返回是否有图标状态变了。
    pub fn sample(&mut self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let Some(tb) = taskbar() else { return false };
        // 光标真的压在任务栏图标上时，图标带悬停底色，会污染"墨量"统计
        if cursor_over_tray(tb) {
            return false;
        }
        let Some(surface) = (unsafe { capture_taskbar(tb) }) else { return false };
        let mut changed = false;
        for it in &mut self.items {
            let ink = surface.ink(it.icon_rect);
            // 参考值取"最近的亮态"：每轮 ×0.985，约 8 秒衰减一半；
            // 闪烁时亮态每 500ms 刷一次，参考值会稳稳停在高位
            it.ink_hi = (it.ink_hi * 0.985).max(ink);
            let dim = it.ink_hi > 0.12 && ink < it.ink_hi * 0.5;
            if dim != it.dim {
                it.dim = dim;
                if dim {
                    it.flash_at = Some(Instant::now());
                }
                changed = true;
                trace(&format!("{} {}（墨量 {:.2}/{:.2}）", it.name, if dim { "灭" } else { "亮" }, ink, it.ink_hi));
            }
        }
        changed
    }

    /// tooltip 名字 → 程序 exe（注册表的 InitialTooltip 表，进程名兜底）
    pub fn exe_of(&mut self, name: &str) -> Option<String> {
        self.icons.exe_for(name)
    }

    /// 点击第 idx 个托盘图标：左键 = 默认动作，右键 = 在图标真实坐标上发鼠标事件。
    /// 先把前台权限让出去：托盘动作经常要弹窗口/菜单，没有这个权限时
    /// 应用会被前台的焦点锁挡住（表现为"点了没反应"）。
    pub fn click(&self, idx: usize, right: bool) {
        let Some(it) = self.items.get(idx) else { return };
        unsafe {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        if right {
            right_click(it.icon_rect);
        } else if it.element.as_ref().map_or(true, |e| !default_action(e)) {
            // 兜底：默认动作不可用时按真实坐标点一下
            send_click(it.icon_rect, false);
        }
    }

    fn automation(&mut self) -> Option<IUIAutomation> {
        if self.automation.is_none() {
            let auto: windows::core::Result<IUIAutomation> =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) };
            self.automation = auto.ok();
        }
        self.automation.clone()
    }
}

/// 开发期跟踪：设 ORBIT_TRACE=1 时写入 orbit.log
fn trace(msg: &str) {
    crate::vlog!("[tray] {msg}");
}

/// 日志里只显示 exe 文件名
fn short_exe(exe: &str) -> &str {
    exe.rsplit(['\\', '/']).next().unwrap_or(exe)
}

/// 进程号 → 可执行文件全路径（小写）；取不到返回空串
pub(crate) fn exe_of_pid(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .is_ok();
        let _ = CloseHandle(proc);
        if ok && len > 0 {
            String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
        } else {
            String::new()
        }
    }
}

/// 光标是否真的压在任务栏的窗体上（不是只落在它的屏幕矩形里）。
/// 任务栏被做成透明后矩形覆盖整条屏幕底部，光标在自己的 Dock 上时也算"在矩形里"，
/// 所以必须用 WindowFromPoint 找到光标下的窗口，再往上找到根窗口比对。
fn cursor_over_tray(tb: HWND) -> bool {
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_err() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(tb, &mut r).is_ok()
            && (p.x < r.left || p.x > r.right || p.y < r.top || p.y > r.bottom)
        {
            return false;
        }
        let under = WindowFromPoint(p);
        if under.is_invalid() {
            return false;
        }
        GetAncestor(under, GA_ROOT) == tb
    }
}

/// 主屏任务栏（Win11 每块屏一个，主屏是 Shell_TrayWnd）
fn taskbar() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()
}

/// 枚举任务栏上的应用托盘按钮：(元素, 按钮屏幕矩形)
unsafe fn find_icons(auto: &IUIAutomation, tb: HWND) -> Vec<(IUIAutomationElement, RECT)> {
    let mut out = Vec::new();
    let root = match auto.ElementFromHandle(tb) {
        Ok(r) => r,
        Err(e) => {
            trace(&format!("ElementFromHandle 失败 {:?}", e.code().0));
            return out;
        }
    };
    trace("ElementFromHandle ok");
    let Ok(by_type) = auto.CreatePropertyCondition(
        UIA_ControlTypePropertyId,
        &VARIANT::from(UIA_ButtonControlTypeId.0 as i32),
    ) else {
        return out;
    };
    let Ok(by_aid) =
        auto.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(AID_NOTIFY_ITEM))
    else {
        return out;
    };
    let cond = match auto.CreateAndCondition(&by_type, &by_aid) {
        Ok(c) => c,
        Err(e) => {
            trace(&format!("CreateAndCondition 失败 {:?}", e.code().0));
            return out;
        }
    };
    let all = match root.FindAll(TreeScope_Descendants, &cond) {
        Ok(a) => a,
        Err(e) => {
            trace(&format!("FindAll 失败 {:?}", e.code().0));
            return out;
        }
    };
    trace(&format!("FindAll 命中 {}", all.Length().unwrap_or(-1)));
    for i in 0..all.Length().unwrap_or(0) {
        let Ok(e) = all.GetElement(i) else { continue };
        let Ok(r) = e.CurrentBoundingRectangle() else { continue };
        if r.right <= r.left || r.bottom <= r.top {
            continue;
        }
        out.push((e, r));
    }
    out
}

/// 按钮里那张 16×16 的图标 Image 元素矩形（比按钮矩形精确）
unsafe fn icon_rect_of(auto: &IUIAutomation, e: &IUIAutomationElement) -> Option<RECT> {
    let cond = auto
        .CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_ImageControlTypeId.0 as i32),
        )
        .ok()?;
    let img = e.FindFirst(TreeScope_Children, &cond).ok()?;
    let r = img.CurrentBoundingRectangle().ok()?;
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

// ---------- 图标像素 ----------

/// 整条任务栏的像素（BGRA，屏幕坐标原点 = 窗口左上角）
struct Surface {
    w: i32,
    h: i32,
    origin: (i32, i32),
    /// BGRA，逐行从下到上（DIB 原始顺序），取值时翻转
    px: Vec<u8>,
}

impl Surface {
    /// 图标矩形的屏幕坐标像素（BGRA → RGBA，行序翻正）
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        let sx = x - self.origin.0;
        let sy = y - self.origin.1;
        if sx < 0 || sy < 0 || sx >= self.w || sy >= self.h {
            return [0, 0, 0, 255];
        }
        let i = (((self.h - 1 - sy) * self.w + sx) * 4) as usize;
        [self.px[i + 2], self.px[i + 1], self.px[i], 255]
    }

    /// 裁出图标、抠掉任务栏底色，缩到 px×px 的 straight RGBA。
    /// 底色按"图标四周 padding 环"的四角做双线性估计——任务栏底色有轻微上下渐变，
    /// 用单一中位数会在图标边上留一圈方块。
    fn icon(&self, rect: RECT, px: i32) -> Vec<u8> {
        let (cw, ch) = (rect.right - rect.left, rect.bottom - rect.top);
        if cw <= 0 || ch <= 0 {
            return Vec::new();
        }
        // 取样的外扩量：按钮内图标四周有 8px 余量，扩 4px 仍在纯底色区
        let pad = 4;
        let corners = [
            self.patch(rect.left - pad, rect.top - pad),
            self.patch(rect.right + pad - 2, rect.top - pad),
            self.patch(rect.left - pad, rect.bottom + pad - 2),
            self.patch(rect.right + pad - 2, rect.bottom + pad - 2),
        ];

        let mut crop = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            let fy = (y as f32 + 0.5) / ch as f32;
            for x in 0..cw {
                let fx = (x as f32 + 0.5) / cw as f32;
                let p = self.at(rect.left + x, rect.top + y);
                let bg = bilerp(&corners, fx, fy);
                let di = ((y * cw + x) * 4) as usize;
                let mut d = [0f32; 3];
                let mut far: f32 = 0.0;
                for c in 0..3 {
                    d[c] = p[c] as f32 - bg[c];
                    far = far.max(d[c].abs());
                }
                if far <= 7.0 {
                    continue; // 全透明
                }
                let a = if far > 110.0 {
                    1.0
                } else {
                    let mut a = 0f32;
                    for c in 0..3 {
                        let b = bg[c].clamp(1.0, 254.0);
                        let v = if d[c] > 0.0 { d[c] / (255.0 - b) } else { -d[c] / b };
                        a = a.max(v);
                    }
                    a.clamp(0.0, 1.0)
                };
                if a < 0.12 {
                    continue;
                }
                for c in 0..3 {
                    let v = (p[c] as f32 - (1.0 - a) * bg[c]) / a;
                    crop[di + c] = v.clamp(0.0, 255.0) as u8;
                }
                crop[di + 3] = (a * 255.0).round() as u8;
            }
        }
        resize_premul(&crop, cw, ch, px, px)
    }

    /// (x,y) 起 2×2 区块的平均色
    fn patch(&self, x: i32, y: i32) -> [f32; 3] {
        let mut sum = [0f32; 3];
        for dy in 0..2 {
            for dx in 0..2 {
                let p = self.at(x + dx, y + dy);
                for c in 0..3 {
                    sum[c] += p[c] as f32;
                }
            }
        }
        [sum[0] / 4.0, sum[1] / 4.0, sum[2] / 4.0]
    }

    /// 图标矩形里"明显不是底色"的像素占比（0~1）。用于判断图标此刻是否被应用隐藏
    /// （闪烁提示 = 图标反复消失/出现）。不做缩放，代价只有几十微秒。
    fn ink(&self, rect: RECT) -> f32 {
        let (cw, ch) = (rect.right - rect.left, rect.bottom - rect.top);
        if cw <= 0 || ch <= 0 {
            return 0.0;
        }
        let pad = 4;
        let corners = [
            self.patch(rect.left - pad, rect.top - pad),
            self.patch(rect.right + pad - 2, rect.top - pad),
            self.patch(rect.left - pad, rect.bottom + pad - 2),
            self.patch(rect.right + pad - 2, rect.bottom + pad - 2),
        ];
        let mut hit = 0i32;
        for y in 0..ch {
            let fy = (y as f32 + 0.5) / ch as f32;
            for x in 0..cw {
                let p = self.at(rect.left + x, rect.top + y);
                let bg = bilerp(&corners, (x as f32 + 0.5) / cw as f32, fy);
                let mut far = 0f32;
                for c in 0..3 {
                    far = far.max((p[c] as f32 - bg[c]).abs());
                }
                if far > 24.0 {
                    hit += 1;
                }
            }
        }
        hit as f32 / (cw * ch) as f32
    }
}

/// 四角双线性插值（0,0 = 左上）
fn bilerp(c: &[[f32; 3]; 4], u: f32, v: f32) -> [f32; 3] {
    let mut out = [0f32; 3];
    for i in 0..3 {
        let top = c[0][i] * (1.0 - u) + c[1][i] * u;
        let bottom = c[2][i] * (1.0 - u) + c[3][i] * u;
        out[i] = top * (1.0 - v) + bottom * v;
    }
    out
}

/// 双线性缩放（预乘 alpha 插值，避免透明边缘发黑/发白）。
/// 放大时托盘源图只有 16~24px，双线性会糊成一团，改用 Catmull-Rom 三次插值。
fn resize_premul(src: &[u8], sw: i32, sh: i32, dw: i32, dh: i32) -> Vec<u8> {
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return vec![0u8; (dw.max(0) * dh.max(0) * 4) as usize];
    }
    let premul = |px: i32, py: i32| -> [f32; 4] {
        let i = ((py * sw + px) * 4) as usize;
        let a = src[i + 3] as f32 / 255.0;
        [src[i] as f32 * a, src[i + 1] as f32 * a, src[i + 2] as f32 * a, a]
    };
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if dw > sw || dh > sh {
        // Catmull-Rom（可分离：先横向再纵向的 4×4 邻域）
        let kernel = |t: f32| -> f32 {
            let a = t.abs();
            if a < 1.0 {
                1.5 * a * a * a - 2.5 * a * a + 1.0
            } else if a < 2.0 {
                -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0
            } else {
                0.0
            }
        };
        for dy in 0..dh {
            let sy = (dy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
            let y0 = sy.floor() as i32;
            for dx in 0..dw {
                let sx = (dx as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
                let x0 = sx.floor() as i32;
                let mut acc = [0f32; 4];
                let mut wsum = 0f32;
                for j in -1..=2 {
                    let wy = kernel(sy - (y0 + j) as f32);
                    if wy == 0.0 {
                        continue;
                    }
                    let py = (y0 + j).clamp(0, sh - 1);
                    for i in -1..=2 {
                        let wx = kernel(sx - (x0 + i) as f32);
                        let w = wx * wy;
                        if w == 0.0 {
                            continue;
                        }
                        let p = premul((x0 + i).clamp(0, sw - 1), py);
                        for c in 0..4 {
                            acc[c] += p[c] * w;
                        }
                        wsum += w;
                    }
                }
                if wsum <= 0.0 {
                    continue;
                }
                let a = (acc[3] / wsum).clamp(0.0, 1.0);
                let oi = ((dy * dw + dx) * 4) as usize;
                if a > 0.001 {
                    for c in 0..3 {
                        out[oi + c] = (acc[c] / wsum / a).clamp(0.0, 255.0) as u8;
                    }
                    out[oi + 3] = (a * 255.0).round() as u8;
                }
            }
        }
        // 放大后的低分辨率图标边缘会发虚：轻度锐化把细节找回来
        if dw == dh {
            sharpen_straight(&mut out, dw, 0.55);
        }
        return out;
    }

    let sample = |sx: f32, sy: f32| -> [f32; 4] {
        let x = sx.clamp(0.0, (sw - 1) as f32);
        let y = sy.clamp(0.0, (sh - 1) as f32);
        let (x0, y0) = (x.floor() as i32, y.floor() as i32);
        let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let p00 = premul(x0, y0);
        let p10 = premul(x1, y0);
        let p01 = premul(x0, y1);
        let p11 = premul(x1, y1);
        let mut o = [0f32; 4];
        for c in 0..4 {
            let top = p00[c] * (1.0 - fx) + p10[c] * fx;
            let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
            o[c] = top * (1.0 - fy) + bottom * fy;
        }
        o
    };
    for dy in 0..dh {
        for dx in 0..dw {
            let sx = (dx as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            let sy = (dy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
            let p = sample(sx, sy);
            let i = ((dy * dw + dx) * 4) as usize;
            if p[3] > 0.001 {
                out[i] = (p[0] / p[3]).clamp(0.0, 255.0) as u8;
                out[i + 1] = (p[1] / p[3]).clamp(0.0, 255.0) as u8;
                out[i + 2] = (p[2] / p[3]).clamp(0.0, 255.0) as u8;
                out[i + 3] = (p[3] * 255.0).round() as u8;
            }
        }
    }
    out
}

/// 按目标边长重采样一张正方形图标：缩小走面积平均（不留锯齿），
/// 放大走 Catmull-Rom + 锐化（16px 的托盘图标放到 40px 也不至于糊成一团）。
fn fit_icon(rgba: &[u8], src: i32, px: i32) -> Vec<u8> {
    if src <= 0 || px <= 0 || rgba.is_empty() || src == px {
        return rgba.to_vec();
    }
    if src > px {
        crate::render::resize_rgba(rgba, src, src, px, px)
    } else {
        resize_premul(rgba, src, src, px, px)
    }
}

/// 轻度 unsharp mask（straight RGBA）。在预乘域里算，半透明边缘才不会出现白边；
/// 只锐化 RGB，alpha 保持原样。
fn sharpen_straight(buf: &mut [u8], px: i32, amount: f32) {
    let n = px.max(1) as usize;
    if buf.len() < n * n * 4 || amount <= 0.0 {
        return;
    }
    // 预乘 + 3×3 高斯模糊（1 2 1 / 2 4 2 / 1 2 1，权重 16）
    let at = |x: usize, y: usize| -> [f32; 4] {
        let i = (y * n + x) * 4;
        let a = buf[i + 3] as f32 / 255.0;
        [
            buf[i] as f32 * a,
            buf[i + 1] as f32 * a,
            buf[i + 2] as f32 * a,
            a,
        ]
    };
    let mut out = vec![0u8; n * n * 4];
    const W: [[f32; 3]; 3] = [[1.0, 2.0, 1.0], [2.0, 4.0, 2.0], [1.0, 2.0, 1.0]];
    for y in 0..n {
        for x in 0..n {
            let p = at(x, y);
            let mut blur = [0f32; 3];
            for (dy, row) in W.iter().enumerate() {
                for (dx, w) in row.iter().enumerate() {
                    let sx = (x + dx).saturating_sub(1).min(n - 1);
                    let sy = (y + dy).saturating_sub(1).min(n - 1);
                    let s = at(sx, sy);
                    for c in 0..3 {
                        blur[c] += s[c] * w / 16.0;
                    }
                }
            }
            let i = (y * n + x) * 4;
            let a = p[3];
            if a <= 0.004 {
                continue;
            }
            for c in 0..3 {
                let v = (p[c] + (p[c] - blur[c]) * amount).clamp(0.0, a);
                out[i + c] = (v / a).clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    buf.copy_from_slice(&out);
}

/// 图标里不透明像素的占比（0~1）。接近 0 = 抓到的是一帧"闪烁熄灭"的空白，
/// 或者抠底色抠过头了，都不能直接当图标用。
fn coverage(rgba: &[u8]) -> f32 {
    if rgba.len() < 4 {
        return 0.0;
    }
    let n = rgba.chunks_exact(4).filter(|p| p[3] > 40).count();
    n as f32 / (rgba.len() / 4) as f32
}

/// 两张同尺寸 straight RGBA 图标的差异（0 完全一致，1 完全不像）。
/// 只统计两边都不透明的像素，避免透明边缘把分数拉偏。
fn icon_diff(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    let mut sum = 0f32;
    let mut wsum = 0f32;
    for i in (0..a.len()).step_by(4) {
        let w = a[i + 3].min(b[i + 3]) as f32 / 255.0;
        if w <= 0.02 {
            continue;
        }
        let d = (a[i] as f32 - b[i] as f32).abs()
            + (a[i + 1] as f32 - b[i + 1] as f32).abs()
            + (a[i + 2] as f32 - b[i + 2] as f32).abs();
        sum += w * (d / 765.0);
        wsum += w;
    }
    if wsum <= 0.0 {
        return 1.0;
    }
    sum / wsum
}

/// PrintWindow 抓整条任务栏（隐藏成 alpha=1 时照样能抓到）
unsafe fn capture_taskbar(tb: HWND) -> Option<Surface> {
    let mut r = RECT::default();
    GetWindowRect(tb, &mut r).ok()?;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    let screen = GetDC(HWND::default());
    let mem = CreateCompatibleDC(screen);
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: 0,
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
    let (Some(hbmp), false) = (hbmp, bits.is_null()) else {
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        return None;
    };
    let old = SelectObject(mem, hbmp);
    let _ = PrintWindow(tb, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
    let mut px = vec![0u8; (w * h * 4) as usize];
    std::ptr::copy_nonoverlapping(bits as *const u8, px.as_mut_ptr(), px.len());
    let _ = SelectObject(mem, old);
    let _ = DeleteObject(hbmp);
    let _ = DeleteDC(mem);
    ReleaseDC(HWND::default(), screen);
    Some(Surface { w, h, origin: (r.left, r.top), px })
}

/// 抓不到像素时的占位：一枚暗色圆点（图标位与命中区仍然对齐）
fn placeholder(px: i32) -> Vec<u8> {
    let mut out = vec![0u8; (px * px * 4) as usize];
    let r = px as f32 / 2.0;
    for y in 0..px {
        for x in 0..px {
            let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
            let d = (dx * dx + dy * dy).sqrt();
            let a = ((r - d).clamp(0.0, 1.0) * 0.55 * 255.0) as u8;
            let i = ((y * px + x) * 4) as usize;
            out[i] = 200;
            out[i + 1] = 200;
            out[i + 2] = 200;
            out[i + 3] = a;
        }
    }
    out
}

// ---------- 点击 ----------

/// 左键：UIA 默认动作（不移动鼠标，任务栏隐藏时也能用）
fn default_action(e: &IUIAutomationElement) -> bool {
    unsafe {
        match e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        ) {
            Ok(p) => p.DoDefaultAction().is_ok(),
            Err(_) => false,
        }
    }
}

fn center(r: RECT) -> POINT {
    POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }
}

/// 右键：托盘菜单由 explorer 在图标坐标上弹出，所以必须把鼠标真的移过去再点，
/// 点完立刻把指针放回原位（菜单已经创建，不会跟着跑）。
fn right_click(r: RECT) {
    send_click(r, true);
}

fn send_click(r: RECT, right: bool) {
    let mut old = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut old);
        let p = center(r);
        let _ = SetCursorPos(p.x, p.y);
        let (down, up) = if right {
            (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
        } else {
            (
                windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_LEFTDOWN,
                windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_LEFTUP,
            )
        };
        for flag in [down, up] {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
        let _ = SetCursorPos(old.x, old.y);
    }
}
