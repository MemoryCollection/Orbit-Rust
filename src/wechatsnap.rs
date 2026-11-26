//! 微信账号快照（登录凭证热切换）。
//!
//! 原理（2026-11-23 实测）：微信 4.x 的「当前登录账号」由
//! `xwechat_files/all_users/config/` 下的 `global_config`、`global_config.crc`、
//! `client_config`、`client_config.crc` 四个文件决定，把登录成功状态下的这组文件
//! 备份起来，之后放回去再启动微信即可免扫码登录对应账号（支持多开共存）。
//!
//! 热替换技巧：微信运行时锁住了这四个文件的**写入**，但**没锁改名**——先把在用文件
//! `mv` 成 `*.locked-<时间戳>` 挪开（运行中实例的句柄跟着旧文件走，不受影响），
//! 再把快照文件复制回原路径，新启动的微信实例就会读到快照凭证。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use windows::core::{GUID, HSTRING, Interface, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HANDLE};
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromFile, GdipDisposeImage, GdipGetImageThumbnail, GdipSaveImageToStream,
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, Status, GpBitmap, GpImage,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    IPersistFile, IStream,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Console::{AllocConsole, GetStdHandle, WriteConsoleW, STD_OUTPUT_HANDLE};
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateMutexW, GetCurrentThreadId, ReleaseMutex, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, ShellLink, IShellLinkW};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowRect,
    GetWindowThreadProcessId, SetForegroundWindow, ShowWindow, SW_RESTORE,
};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};

/// 构成「登录凭证」的四个文件（相对 `all_users/config/`）
const CFG_FILES: [&str; 4] = ["global_config", "global_config.crc", "client_config", "client_config.crc"];

/// PNG 编码器 CLSID（GDI+ 官方定义，windows crate 没导出）
const CLSID_PNG_ENCODER: GUID = GUID::from_values(0x557CF406, 0x1A04, 0x11D3, [
    0x9A, 0x73, 0x00, 0x00, 0xF8, 0x1E, 0xF3, 0x2E,
]);

// ---------- 快照结构 ----------

#[derive(Clone)]
pub struct Snapshot {
    /// 账号数字 ID（= 快照目录名，= head_imgs/0 下的头像文件名）
    pub id: String,
    /// 用户命名的昵称（account.txt 里的 nickname）
    pub nickname: String,
    /// 凭证备份时间（account.txt 里的 backup_time，"YYYY-MM-DD HH:MM:SS"）
    /// ——列表按它从早到晚排（时间最早的是 1 号）
    pub backup_time: String,
}

impl Snapshot {
}

/// 快照根目录：`<exe>/data/wechat_accounts/`
fn accounts_root() -> PathBuf {
    crate::store::data_dir().join("wechat_accounts")
}

// ---------- 路径（全部来自设置页手动填写，不做自动探测） ----------

/// 微信安装目录下的配置目录：`<数据目录>/all_users/config`
fn all_users_dir() -> Option<PathBuf> {
    data_root().map(|r| r.join("all_users"))
}

pub fn config_dir() -> Option<PathBuf> {
    all_users_dir().map(|d| d.join("config"))
}

/// 数据目录（xwechat_files）：只读设置页手动填写的值
pub fn data_root() -> Option<PathBuf> {
    let custom = crate::settings::wechat_data_dir();
    if custom.is_empty() {
        return None;
    }
    let p = PathBuf::from(&custom);
    p.join("all_users/config/global_config").is_file().then_some(p)
}

/// 微信主程序 Weixin.exe：只读设置页手动填写的值
pub fn wechat_exe() -> Option<PathBuf> {
    let custom = crate::settings::wechat_path();
    if custom.is_empty() {
        return None;
    }
    let p = PathBuf::from(&custom);
    p.is_file().then_some(p)
}

// ---------- 当前登录账号 ----------

/// 当前登录账号：`all_users/head_imgs/0/` 下永远只有一个文件，文件名 = 账号数字 ID。
/// 返回 (账号 ID, 头像路径)。
pub fn current_account() -> Option<(String, PathBuf)> {
    let dir = all_users_dir()?.join("head_imgs/0");
    let mut found = None;
    for e in std::fs::read_dir(&dir).ok()? {
        let Ok(e) = e else { continue };
        let p = e.path();
        if p.is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if !name.is_empty() && !name.starts_with('.') {
                    found = Some((name.to_string(), p));
                }
            }
        }
    }
    found
}

// ---------- 快照列表 / 保存 ----------

/// 扫描快照目录，按 account.txt 里的昵称读出列表。
/// 排序：backup_time 从早到晚（时间最早的是 1 号），没写时间的排最后，同刻按 ID。
pub fn list() -> Vec<Snapshot> {
    let root = accounts_root();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()) else {
            continue;
        };
        let mut nickname = String::new();
        let mut backup_time = String::new();
        if let Ok(txt) = std::fs::read_to_string(dir.join("account.txt")) {
            for line in txt.lines() {
                if let Some(v) = line.strip_prefix("nickname=") {
                    nickname = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("backup_time=") {
                    backup_time = v.trim().to_string();
                }
            }
        }
        if nickname.is_empty() {
            nickname = id.clone();
        }
        out.push(Snapshot { id, nickname, backup_time });
    }
    out.sort_by(|a, b| {
        let ka = if a.backup_time.is_empty() { "9999-99-99" } else { a.backup_time.as_str() };
        let kb = if b.backup_time.is_empty() { "9999-99-99" } else { b.backup_time.as_str() };
        (ka, &a.id).cmp(&(kb, &b.id))
    });
    out
}

/// 保存当前登录凭证为快照。要求：global_config 可读；head_imgs/0 有当前账号。
pub fn save(nickname: &str) -> Result<Snapshot, String> {
    let (id, avatar) = current_account().ok_or("检测不到当前登录账号（head_imgs/0 为空，微信可能没登录）")?;
    let cfg = config_dir().ok_or("找不到微信配置目录（可在下方自定义数据目录）")?;
    let dst = accounts_root().join(&id);
    std::fs::create_dir_all(&dst).map_err(|e| format!("创建快照目录失败：{e}"))?;

    // 四个凭证文件：global_config 必须有，client_config 缺了也能存（旧版本兼容）
    for name in CFG_FILES {
        let src = cfg.join(name);
        if src.is_file() {
            std::fs::copy(&src, dst.join(name)).map_err(|e| format!("复制 {name} 失败：{e}"))?;
        }
    }
    if !dst.join("global_config").is_file() {
        return Err("global_config 不存在，凭证不完整".into());
    }
    if avatar.is_file() {
        let _ = std::fs::copy(&avatar, dst.join("avatar.jpg"));
        // 头像转图标（失败不阻塞：快捷方式只是没自定义图标）
        let _ = jpg_to_ico(&dst.join("avatar.jpg"), &dst.join("icon.ico"));
    }
    let time = time_str();
    std::fs::write(
        dst.join("account.txt"),
        format!("nickname={nickname}\naccount_id={id}\nbackup_time={time}\nnote=登录成功后的凭证快照。恢复=热替换回 all_users/config/ 后启动微信。\n"),
    )
    .map_err(|e| format!("写 account.txt 失败：{e}"))?;
    crate::tlog!("[wechatsnap] 已保存快照 {nickname}（账号 {id}）");
    Ok(Snapshot { id, nickname: nickname.to_string(), backup_time: time })
}

fn time_str() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = now / 86400;
    // 简单的 UTC+8 换算（快照时间只是给人看的备注，不追求秒级精确）
    let rem = now % 86400 + 8 * 3600;
    let (h, m, s) = ((rem / 3600) % 24, (rem / 60) % 60, rem % 60);
    // 从 2000-01-01 起粗算年月日
    let mut y = 2000;
    let mut d = days as i64 - 10957; // 2000-01-01 的天数
    while d >= 365 + (leap(y) as i64) {
        d -= 365 + leap(y) as i64;
        y += 1;
    }
    let leap_y = leap(y);
    let md = [
        31,
        if leap_y { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1;
    for len in md {
        if d < len as i64 {
            break;
        }
        d -= len as i64;
        mo += 1;
    }
    format!("{y}-{mo:02}-{:02} {h:02}:{m:02}:{s:02}", d + 1)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// 删除快照
pub fn delete(id: &str) -> Result<(), String> {
    let dir = accounts_root().join(id);
    if !dir.is_dir() {
        return Err("快照不存在".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))
}

// ---------- 热替换 / 启动 ----------

/// 把快照凭证热替换回 `all_users/config/`。
/// 被微信握着句柄的旧文件：先试直接删除（没句柄的一步到位），删不掉（有句柄）才改名
/// 挪成 `*.locked-<时间戳>`（运行中实例的句柄跟着旧文件走，不受影响）。
/// 替换前还会顺手清理一轮历史遗留的 `.locked-*`：句柄已释放的旧文件即被删掉，不会堆积。
pub fn restore(id: &str) -> Result<(), String> {
    let snap = accounts_root().join(id);
    let cfg = config_dir().ok_or("找不到微信配置目录（可在设置里自定义数据目录）")?;
    if !snap.join("global_config").is_file() {
        return Err("快照里没有 global_config，凭证不完整".into());
    }
    std::fs::create_dir_all(&cfg).map_err(|e| format!("配置目录不存在且创建失败：{e}"))?;

    // 先清理一轮历史遗留：之前替换挪开的 .locked-* 旧文件，句柄已释放的直接删掉
    if let Ok(rd) = std::fs::read_dir(&cfg) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.contains(".locked-") || name.ends_with(".locked_test") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    let ts = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let mut errs: Vec<String> = Vec::new();
    for name in CFG_FILES {
        let src = snap.join(name);
        if !src.is_file() {
            continue;
        }
        let dst = cfg.join(name);
        if dst.exists() {
            // ① 先试直接删除：微信没握句柄（没运行/已退出）时一步到位
            if std::fs::remove_file(&dst).is_ok() {
                if let Err(e) = std::fs::copy(&src, &dst) {
                    errs.push(format!("{name} 复制失败：{e}"));
                }
                continue;
            }
            // ② 删不掉 = 被握着句柄：改名挪开（注意不能用 with_extension，
            // 会把 global_config.crc 的 .crc 顶掉导致两个文件改名后撞名）
            let locked = dst.with_file_name(format!("{name}.locked-{ts}"));
            if std::fs::rename(&dst, &locked).is_err() {
                // 改名也失败：试试直接覆盖（个别环境锁写不锁覆盖）
                if std::fs::copy(&src, &dst).is_err() {
                    errs.push(format!("{name} 被锁且覆盖失败"));
                }
                continue;
            }
            if let Err(e) = std::fs::copy(&src, &dst) {
                // 复制失败，把原文件挪回来
                let _ = std::fs::rename(&locked, &dst);
                errs.push(format!("{name} 复制失败：{e}"));
            }
        } else if let Err(e) = std::fs::copy(&src, &dst) {
            errs.push(format!("{name} 复制失败：{e}"));
        }
    }
    if errs.is_empty() {
        crate::tlog!("[wechatsnap] 已热替换快照 {id} 的凭证");
        Ok(())
    } else {
        Err(errs.join("；"))
    }
}

/// 恢复快照凭证并启动微信（快捷方式的目标动作）。
/// 启动成功后把**本次拉起的 Weixin.exe pid** 写进快照的 account.txt：
/// 微信用凭证免扫码登录的就是这个进程，只要它还活着且还是 Weixin.exe，
/// 就说明该账号已登录（见 [`logged_in`]）。
pub fn launch(id: &str) -> Result<(), String> {
    restore(id)?;
    let exe = wechat_exe().ok_or("找不到 Weixin.exe（可在设置里自定义微信程序路径）")?;
    let child = std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| format!("启动微信失败：{e}"))?;
    let pid = child.id();
    write_pid(id, pid);
    crate::tlog!("[wechatsnap] 快照 {id} 凭证已恢复，微信已启动 pid={pid}");
    Ok(())
}

// ---------- 登录状态（account.txt 里的 pid） ----------

/// 把登录 pid 写进快照的 account.txt（保留其余行；没有 pid= 行就追加）
fn write_pid(id: &str, pid: u32) {
    let p = accounts_root().join(id).join("account.txt");
    let mut lines: Vec<String> = std::fs::read_to_string(&p)
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    lines.retain(|l| !l.starts_with("pid="));
    lines.push(format!("pid={pid}"));
    let _ = std::fs::write(&p, lines.join("\n") + "\n");
}

/// account.txt 里记录的登录 pid（没记录过 = None）
pub fn stored_pid(id: &str) -> Option<u32> {
    let txt = std::fs::read_to_string(accounts_root().join(id).join("account.txt")).ok()?;
    for line in txt.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            return v.trim().parse::<u32>().ok().filter(|p| *p != 0);
        }
    }
    None
}

/// pid 是否还活着且主程序名是 Weixin.exe / WeChat.exe
fn pid_is_weixin(pid: u32) -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        let mut hit = false;
        while ok {
            if pe.th32ProcessID == pid {
                let name = String::from_utf16_lossy(&pe.szExeFile);
                let base = name.trim_end_matches('\0');
                hit = base.eq_ignore_ascii_case("Weixin.exe") || base.eq_ignore_ascii_case("WeChat.exe");
                break;
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
        hit
    }
}

/// 该快照账号是否已登录：account.txt 记录的 pid 还活着且是微信主程序 → Some(pid)。
/// 2s 缓存（设置页每帧都会调，进程快照不便宜）。
pub fn logged_in(id: &str) -> Option<u32> {
    static CACHE: LazyLock<Mutex<HashMap<String, (u64, Option<u32>)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    if let Ok(c) = CACHE.lock() {
        if let Some((at, v)) = c.get(id) {
            if now.saturating_sub(*at) < 2000 {
                return *v;
            }
        }
    }
    let v = stored_pid(id).filter(|p| pid_is_weixin(*p));
    if let Ok(mut c) = CACHE.lock() {
        c.insert(id.to_string(), (now, v));
    }
    v
}

// ---------- 桌面快捷方式 ----------

/// 给快照创建桌面快捷方式：目标 = 本程序 `--wechat-launch <id>`，图标 = 快照头像。
/// 返回生成的 .lnk 路径。
pub fn make_shortcut(id: &str) -> Result<PathBuf, String> {
    let snap = accounts_root().join(id);
    if !snap.join("global_config").is_file() {
        return Err("快照凭证不完整".into());
    }
    // 昵称从 account.txt 读（UI 只传 id 过来）
    let nickname = list()
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.nickname)
        .unwrap_or_else(|| id.to_string());
    // 图标不在就补一次
    let icon = snap.join("icon.ico");
    if !icon.is_file() && snap.join("avatar.jpg").is_file() {
        let _ = jpg_to_ico(&snap.join("avatar.jpg"), &icon);
    }
    let exe = std::env::current_exe().map_err(|e| format!("取程序路径失败：{e}"))?;
    let lnk = desktop_dir()?.join(format!("{}.lnk", shortcut_display_name(&nickname, id)));

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let exe_h = HSTRING::from(exe.as_os_str());
        let args = HSTRING::from(format!("--wechat-launch {id}"));
        let workdir = HSTRING::from(exe.parent().unwrap_or(Path::new(".")));
        link.SetPath(PCWSTR(exe_h.as_ptr())).map_err(|e| e.to_string())?;
        link.SetArguments(PCWSTR(args.as_ptr())).map_err(|e| e.to_string())?;
        link.SetWorkingDirectory(PCWSTR(workdir.as_ptr())).map_err(|e| e.to_string())?;
        if icon.is_file() {
            let icon_h = HSTRING::from(icon.as_os_str());
            link.SetIconLocation(PCWSTR(icon_h.as_ptr()), 0).map_err(|e| e.to_string())?;
        }
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let lnk_h = HSTRING::from(lnk.as_os_str());
        persist.Save(PCWSTR(lnk_h.as_ptr()), false).map_err(|e| e.to_string())?;
    }
    crate::tlog!("[wechatsnap] 已创建桌面快捷方式 {}", lnk.display());
    Ok(lnk)
}

// ---------- 多账号批量启动（桌面快捷方式「逐个登录」） ----------

/// 微信窗口类名。实测（2026-11-26，微信 4.x）：登录窗口和登录后的主窗口**同名**，
/// 都是 `Qt51514QWindowIcon`；靠尺寸区分（登录窗约 368×484，主窗约 1241×869）。
/// 保留旧版类名做兼容。
const WX_WND_CLASSES: [&str; 3] = ["Qt51514QWindowIcon", "mmui::LoginWindow", "WeChatLoginWndForPC"];
/// 登录窗口的宽度上限（物理像素）。主窗口远大于此值，据此区分"还在登录"和"已进主界面"。
const LOGIN_WND_MAX_W: i32 = 700;

/// 设置本进程为 DPI 感知：GetWindowRect 才会返回物理坐标（宽度阈值判断需要一致坐标系）。
/// --wechat-batch-launch 分支在 app::run 之前返回，不会经过主程序的 DPI 初始化，必须在这里补上。
fn ensure_dpi_aware() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// 批量启动的跨进程互斥锁：命名 Mutex，保证同一时刻只有一个批次在跑。
/// 凭证目录是全局的，多个批次并发会互相覆盖导致登录错乱（实测已复现）。
/// 用 Drop 自动释放；进程崩溃时系统也会自动释放命名 Mutex。
struct BatchGuard(HANDLE);

impl BatchGuard {
    /// 尝试独占。已有批次在跑时立即返回 Err（等待 0ms，不排队）。
    fn try_acquire() -> Result<Self, String> {
        let name = HSTRING::from(r"Local\Orbit_WeChatBatchLaunch");
        unsafe {
            let h = CreateMutexW(None, false, PCWSTR(name.as_ptr()))
                .map_err(|e| format!("创建批量互斥锁失败：{e}"))?;
            // 0 = 立即返回，不等待。已有人持有则为 WAIT_TIMEOUT。
            let wait = WaitForSingleObject(h, 0);
            if wait != WAIT_OBJECT_0 {
                let _ = windows::Win32::Foundation::CloseHandle(h);
                if wait == WAIT_TIMEOUT {
                    return Err("已有微信批量登录正在进行，请等它完成（或关闭上一个）后再运行".into());
                }
                return Err(format!("获取批量互斥锁失败（wait={wait:?}）"));
            }
            Ok(Self(h))
        }
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

// ---------- 多开进度控制台 ----------
// 发布版是 GUI 子系统（#![windows_subsystem = "windows"]），默认不弹控制台。
// 多开是长时间等待人工登录的流程，给用户弹一个控制台看进度；关掉这个窗口 = 结束本次任务
// （GUI 进程附加控制台后，点窗口 X 会触发默认 CTRL_CLOSE_EVENT → 进程终止）。

/// 弹出进度控制台并打印标题。失败（例如已附加到别的控制台）不影响主流程。
fn init_progress_console(total: usize) {
    unsafe {
        // 已附加控制台时 AllocConsole 会失败，忽略即可
        let _ = AllocConsole();
    }
    console_line(&format!("==== 微信多开 · 共 {total} 个账号 ===="));
    console_line("请按顺序手动点「进入微信」登录；关闭此窗口即结束本次多开。");
    console_line("");
}

/// 输出一行进度到控制台（同时写 orbit.log，便于事后排查）。
fn console_line(msg: &str) {
    crate::tlog!("[batch] {msg}");
    unsafe {
        let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) else { return };
        let t = windows::Win32::System::SystemInformation::GetLocalTime();
        let line = format!("[{:02}:{:02}:{:02}] {msg}\r\n", t.wHour, t.wMinute, t.wSecond);
        let wide: Vec<u16> = line.encode_utf16().collect();
        let _ = WriteConsoleW(h, &wide, None, None);
    }
}

/// 逐个恢复凭证并启动微信：严格串行 —— 第 N 个**由用户手动登录进主界面后**，才处理第 N+1 个。
///
/// 流程（对每个快照 ID 依次执行，任何一步失败立即中止、不再替换后续凭证）：
///   0. 跨进程互斥：只允许一个批次在跑，防止多次双击导致凭证目录被并发覆盖
///   1. 热替换该快照凭证（此刻**只**为启动这一个对象）→ 启动 Weixin.exe
///   2. 弹窗提示用户手动点「进入微信」登录（**不自动点击**）
///   3. 轮询等待该 pid 的主界面出现（用户手动登录完成，最多 5 分钟）
///   4. 主界面出现后再进入下一个快照 —— 保证"一个登好了才开下一个"
///
/// 关键点：凭证目录是全局的。只有等微信真正读取凭证并进入主界面后，替换下一个凭证
/// 才不会干扰它；否则上一个实例可能还在读盘，凭证被挪走就登录失败（实测复现过）。
pub fn batch_launch(ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Err("没有勾选任何凭证".into());
    }
    // 跨进程互斥必须最先拿：拿到之前绝不碰凭证目录
    let _guard = BatchGuard::try_acquire()?;
    init_progress_console(ids.len());
    ensure_dpi_aware();
    let exe = wechat_exe().ok_or("找不到 Weixin.exe（可在设置里自定义微信程序路径）")?;

    let snaps = list();
    let label = |id: &str| -> String {
        snaps.iter().find(|s| s.id == id)
            .map(|s| s.nickname.clone()).unwrap_or_else(|| id.to_string())
    };

    for (i, id) in ids.iter().enumerate() {
        let name = label(id);
        console_line(&format!("[{}/{}] 「{}」— 正在替换凭证…", i + 1, ids.len(), name));

        // 1. 替换凭证（全局），仅用于即将启动的这一个对象
        restore(id).map_err(|e| format!("「{name}」恢复凭证失败：{e}"))?;

        // 2. 启动微信，spawn 的 pid 就是窗口 pid（微信多开是独立主进程，用户实测确认）
        let child = std::process::Command::new(&exe)
            .spawn()
            .map_err(|e| format!("「{name}」启动微信失败：{e}"))?;
        let pid = child.id();
        write_pid(id, pid);
        crate::tlog!("[wechatsnap] 批量启动 [{}/{}] 快照 {id} 微信 pid={pid}", i + 1, ids.len());

        // 3. 提示用户手动登录（不自动点击），等主界面出现才算完成。
        //    失败立即中止，绝不替换下一个凭证。
        console_line(&format!("[{}/{}] 「{}」已启动，请在微信窗口手动点「进入微信」登录…", i + 1, ids.len(), name));
        wait_user_login(pid).map_err(|e| {
            format!("「{name}」等待手动登录失败：{e}；已中止，后续账号凭证未替换")
        })?;
        console_line(&format!("[{}/{}] 「{}」✓ 已登录，进入主界面", i + 1, ids.len(), name));

        // 4. 主界面出现后留缓冲，让凭证彻底落盘再开下一个
        if i + 1 < ids.len() {
            std::thread::sleep(std::time::Duration::from_millis(1200));
        }
    }

    console_line("==== 全部完成，可关闭此窗口 ====");
    crate::tlog!("[wechatsnap] 批量启动完成，共 {} 个账号", ids.len());
    Ok(())
}

/// 把窗口置到最前。
///
/// Windows 有「前台锁定」限制：非前台进程直接调 SetForegroundWindow 往往无效。
/// 这里临时把本线程输入队列挂到当前前台线程上（AttachThreadInput），再置前，成功率最高。
fn bring_to_front(hwnd: HWND) {
    unsafe {
        // 若最小化则还原，否则置前无效
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let my_tid = GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != my_tid;
        if attached {
            let _ = AttachThreadInput(my_tid, fg_tid, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(my_tid, fg_tid, false);
        }
    }
}

/// 等待单个账号**由用户手动**登录完成：不自动点「进入微信」，只轮询该 pid 的主界面是否出现。
/// 登录小窗一出现就把它置到最前，方便用户直接看到并点击「进入微信」。
/// 主界面出现 = 用户已完成登录（点按钮/扫码都算），调用方据此才替换下一个凭证、启动下一个微信。
/// 因为要等人工操作，超时放宽到 5 分钟。
fn wait_user_login(pid: u32) -> Result<(), String> {
    let deadline = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() } + 300_000;
    let mut front_tries = 0u32;
    loop {
        if !find_wx_window(pid, true).0.is_null() {
            return Ok(());
        }
        // 登录小窗出现后，头几次轮询反复置前，确保它稳定停在用户面前
        if front_tries < 8 {
            let login = find_wx_window(pid, false);
            if !login.0.is_null() {
                bring_to_front(login);
                if front_tries == 0 {
                    console_line("  ↳ 已把微信登录窗口置前，请手动点「进入微信」");
                }
                front_tries += 1;
            }
        }
        let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
        if now >= deadline {
            return Err("等待手动登录超时（5 分钟，微信主界面一直未出现）".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// 枚举顶层窗口，找 pid 的微信窗口。`want_main=true` 找大主窗（宽 ≥ LOGIN_WND_MAX_W），
/// 否则找登录小窗。类名命中 WX_WND_CLASSES；实测登录窗与主窗同名，靠宽度区分。
fn find_wx_window(pid: u32, want_main: bool) -> HWND {
    struct Ctx {
        pid: u32,
        want_main: bool,
        hit: HWND,
    }
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut wpid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut wpid));
        if wpid != ctx.pid {
            return windows::Win32::Foundation::TRUE;
        }
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        if len == 0 {
            return windows::Win32::Foundation::TRUE;
        }
        let class = String::from_utf16_lossy(&buf[..len as usize]);
        if !WX_WND_CLASSES.iter().any(|c| *c == class) {
            return windows::Win32::Foundation::TRUE;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return windows::Win32::Foundation::TRUE;
        }
        let is_main = (r.right - r.left) >= LOGIN_WND_MAX_W;
        if is_main == ctx.want_main {
            ctx.hit = hwnd;
            return windows::Win32::Foundation::FALSE;
        }
        windows::Win32::Foundation::TRUE
    }
    let mut ctx = Ctx { pid, want_main, hit: HWND::default() };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.hit
}

/// 给多个快照生成**一个**桌面快捷方式：目标 = `--wechat-batch-launch <id1>,<id2>,...`，
/// 图标用第一个快照的头像。返回生成的 .lnk 路径。
pub fn make_batch_shortcut(ids: &[String]) -> Result<PathBuf, String> {
    if ids.is_empty() {
        return Err("没有勾选任何凭证".into());
    }
    // 校验每个快照凭证完整
    for id in ids {
        let snap = accounts_root().join(id);
        if !snap.join("global_config").is_file() {
            return Err(format!("快照「{id}」凭证不完整"));
        }
    }
    // 快捷方式名字：取勾选账号的昵称拼起来（最多取前 3 个 + 省略）
    let snaps = list();
    let mut names: Vec<String> = Vec::new();
    for id in ids {
        let nick = snaps
            .iter()
            .find(|s| &s.id == id)
            .map(|s| s.nickname.clone())
            .unwrap_or_else(|| id.clone());
        names.push(nick);
    }
    let display = if names.len() > 3 {
        format!("{}-等{}个", names[..3].join("-"), names.len())
    } else {
        names.join("-")
    };
    // 用第一个快照的图标（没有就补一次）
    let icon = accounts_root().join(&ids[0]).join("icon.ico");
    if !icon.is_file() && accounts_root().join(&ids[0]).join("avatar.jpg").is_file() {
        let _ = jpg_to_ico(
            &accounts_root().join(&ids[0]).join("avatar.jpg"),
            &icon,
        );
    }
    let exe = std::env::current_exe().map_err(|e| format!("取程序路径失败：{e}"))?;
    // 多开快捷方式名：直接对拼接昵称做非法字符清洗
    let lnk = desktop_dir()?.join(format!("{}.lnk", sanitize_name(&format!("多开-{display}"))));

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let exe_h = HSTRING::from(exe.as_os_str());
        let args = HSTRING::from(format!("--wechat-batch-launch {}", ids.join(",")));
        let workdir = HSTRING::from(exe.parent().unwrap_or(Path::new(".")));
        link.SetPath(PCWSTR(exe_h.as_ptr())).map_err(|e| e.to_string())?;
        link.SetArguments(PCWSTR(args.as_ptr())).map_err(|e| e.to_string())?;
        link.SetWorkingDirectory(PCWSTR(workdir.as_ptr())).map_err(|e| e.to_string())?;
        if icon.is_file() {
            let icon_h = HSTRING::from(icon.as_os_str());
            link.SetIconLocation(PCWSTR(icon_h.as_ptr()), 0).map_err(|e| e.to_string())?;
        }
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let lnk_h = HSTRING::from(lnk.as_os_str());
        persist.Save(PCWSTR(lnk_h.as_ptr()), false).map_err(|e| e.to_string())?;
    }
    crate::tlog!("[wechatsnap] 已创建多开桌面快捷方式 {}", lnk.display());
    Ok(lnk)
}

/// 清洗快捷方式文件名里的非法字符（Windows 文件名保留字符）
fn sanitize_name(name: &str) -> String {
    let bad = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    name.chars().map(|c| if bad.contains(&c) { '_' } else { c }).collect()
}

fn shortcut_display_name(nickname: &str, id: &str) -> String {
    let name = sanitize_name(nickname).trim().to_string();
    if name.is_empty() {
        format!("微信-{id}")
    } else {
        format!("微信-{name}")
    }
}

fn desktop_dir() -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG;
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), HANDLE::default())
            .map_err(|e| e.to_string())?;
        let len = (0..).take_while(|i| *p.0.add(*i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len));
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(PathBuf::from(s))
    }
}

// ---------- 头像 → 图标（GDI+） ----------

/// 把 JPG 头像缩放到 256×256 存成 PNG 格式的 .ico（快捷方式图标用）。
fn jpg_to_ico(jpg: &Path, ico: &Path) -> Result<(), String> {
    unsafe {
        let mut token = 0usize;
        // GdiplusStartupInput::default() 是全零结构，但 GDI+ 要求版本号必须显式填 1
        let si = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        if GdiplusStartup(&mut token, &si, std::ptr::null_mut()).0 != 0 {
            return Err("GDI+ 初始化失败".into());
        }
        let result = jpg_to_ico_inner(jpg, ico);
        GdiplusShutdown(token);
        result
    }
}

fn jpg_to_ico_inner(jpg: &Path, ico: &Path) -> Result<(), String> {
    unsafe {
        let jpg_h = HSTRING::from(jpg.as_os_str());
        let mut bmp: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromFile(PCWSTR(jpg_h.as_ptr()), &mut bmp) != Status(0) || bmp.is_null() {
            return Err("读取头像失败（GDI+）".into());
        }
        let mut thumb: *mut GpImage = std::ptr::null_mut();
        if GdipGetImageThumbnail(bmp as *mut GpImage, 256, 256, &mut thumb, 0, std::ptr::null_mut())
            != Status(0)
            || thumb.is_null()
        {
            GdipDisposeImage(bmp as *mut GpImage);
            return Err("生成缩略图失败（GDI+）".into());
        }
        // 编码成 PNG 写进内存流
        let stream: IStream = CreateStreamOnHGlobal(HGLOBAL::default(), false).map_err(|e| e.to_string())?;
        let save = GdipSaveImageToStream(
            thumb,
            &stream,
            &CLSID_PNG_ENCODER,
            std::ptr::null(),
        );
        GdipDisposeImage(thumb);
        GdipDisposeImage(bmp as *mut GpImage);
        if save != Status(0) {
            return Err("PNG 编码失败（GDI+）".into());
        }
        // 从流里取 PNG 字节
        let hg: HGLOBAL = GetHGlobalFromStream(&stream).map_err(|e| e.to_string())?;
        let size = GlobalSize(hg);
        if size == 0 {
            return Err("PNG 数据为空".into());
        }
        let ptr = GlobalLock(hg);
        if ptr.is_null() {
            return Err("锁定 PNG 数据失败".into());
        }
        let png = std::slice::from_raw_parts(ptr as *const u8, size);
        let png = png.to_vec();
        let _ = GlobalUnlock(hg);
        // 包上 ICO 容器（6 字节头 + 16 字节目录项 + PNG 数据）
        // 目录项：width/height/colorcount/reserved 各 1 字节（0 宽高 = 256），planes/bitcount 各 2 字节
        let mut ico_bytes = Vec::with_capacity(png.len() + 22);
        ico_bytes.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR: type=1(icon), count=1
        ico_bytes.push(0); // width = 0 表示 256
        ico_bytes.push(0); // height = 0 表示 256
        ico_bytes.push(0); // 调色板色数
        ico_bytes.push(0); // 保留
        ico_bytes.extend_from_slice(&[1, 0]); // planes
        ico_bytes.extend_from_slice(&[32, 0]); // bit count
        ico_bytes.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico_bytes.extend_from_slice(&22u32.to_le_bytes());
        ico_bytes.extend_from_slice(&png);
        std::fs::write(ico, ico_bytes).map_err(|e| format!("写 icon.ico 失败：{e}"))
    }
}

/// 启动失败时弹一个原生 MessageBox（--wechat-launch 是无窗口入口，失败必须可见告知）
pub fn error_popup(msg: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_SETFOREGROUND};
    unsafe {
        let wide = HSTRING::from(msg);
        MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            PCWSTR(HSTRING::from("Orbit 微信账号").as_ptr()),
            MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

// STREAM_SEEK_SET 占位已删：不再需要


#[cfg(test)]
mod tests {
    use super::*;

    /// 头像 → ICO 转换：用真实备份头像跑一遍（GDI+ 在本机可用）
    #[test]
    fn test_jpg_to_ico() {
        let jpg = PathBuf::from("target/release/data/wechat_accounts/2612710356/avatar.jpg");
        if !jpg.is_file() {
            return; // 本机没有备份头像时跳过
        }
        let out = PathBuf::from("target/release/data/test_icon.ico");
        jpg_to_ico(&jpg, &out).expect("jpg_to_ico 失败");
        let meta = std::fs::metadata(&out).expect("ico 未生成");
        assert!(meta.len() > 200, "ico 太小");
        let bytes = std::fs::read(&out).unwrap();
        // ICO 头：count=1，PNG 数据从 22 字节处开始且是 PNG 魔数
        assert_eq!(&bytes[0..4], &[0, 0, 1, 0]);
        assert_eq!(&bytes[22..26], &[0x89, b'P', b'N', b'G']);
        let _ = std::fs::remove_file(out);
    }

    /// 快照列表应能读出手动备份的两个快照（昵称来自 account.txt）
    #[test]
    fn test_list_snapshots() {
        // store::data_dir 基于 current_exe：测试进程在 target/release/deps 下读不到正式
        // 数据目录时跳过（正式运行时 exe 在 target/release，快照就在旁边 data/ 里）
        if !accounts_root().join("2612710356").is_dir() {
            return;
        }
        let snaps = list();
        let ids: Vec<&str> = snaps.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"2612710356"), "应有办执照快照：{ids:?}");
        assert!(ids.contains(&"4190658650"), "应有注册公司快照：{ids:?}");
        assert!(snaps.iter().any(|s| s.nickname.contains("办执照")));
    }
}
