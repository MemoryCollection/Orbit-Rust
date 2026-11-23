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

use std::path::{Path, PathBuf};

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
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, ShellLink, IShellLinkW};

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
    pub dir: PathBuf,
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

/// 扫描快照目录，按 account.txt 里的昵称读出列表
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
        if let Ok(txt) = std::fs::read_to_string(dir.join("account.txt")) {
            for line in txt.lines() {
                if let Some(v) = line.strip_prefix("nickname=") {
                    nickname = v.trim().to_string();
                }
            }
        }
        if nickname.is_empty() {
            nickname = id.clone();
        }
        out.push(Snapshot { id, nickname, dir });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
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
    Ok(Snapshot { id, nickname: nickname.to_string(), dir: dst })
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

/// 恢复快照凭证并启动微信（快捷方式的目标动作）
pub fn launch(id: &str) -> Result<(), String> {
    restore(id)?;
    let exe = wechat_exe().ok_or("找不到 Weixin.exe（可在设置里自定义微信程序路径）")?;
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| format!("启动微信失败：{e}"))?;
    crate::tlog!("[wechatsnap] 快照 {id} 凭证已恢复，微信已启动");
    Ok(())
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

fn shortcut_display_name(nickname: &str, id: &str) -> String {
    let bad = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    let name: String = nickname.chars().map(|c| if bad.contains(&c) { '_' } else { c }).collect();
    let name = name.trim();
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
