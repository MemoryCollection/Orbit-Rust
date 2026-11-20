//! 托盘图标的高分辨率来源。
//!
//! 托盘按钮里的图标只有 16px（从任务栏位图抠出来再放大，插值再好也是糊的）。
//! Win11 会把每个托盘图标登记在 `HKCU\Control Panel\NotifyIconSettings\<hash>` 下，
//! 里面既有 `ExecutablePath`（谁注册的）也有 `InitialTooltip`（叫什么），
//! 于是可以把 tooltip 名字映射到 exe，再从 exe 里取 256px 图标缩下来。
//! 注册表里没有的（动态 tooltip、非标准程序）退回到"进程名包含关系"匹配。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    REG_SZ, REG_VALUE_TYPE,
};

/// 名字表的有效期：新装的程序要能被认出来，也不用每次轮询都扫注册表/进程
const TABLE_TTL: Duration = Duration::from_secs(60);

pub struct IconCache {
    /// 归一化名字 → exe 全路径（小写）
    table: Vec<(String, String)>,
    built: Option<Instant>,
    /// (exe, px) → 图标（straight RGBA）
    icons: HashMap<(String, i32), Vec<u8>>,
}

impl Default for IconCache {
    fn default() -> Self {
        Self { table: Vec::new(), built: None, icons: HashMap::new() }
    }
}

impl IconCache {
    /// 按托盘 tooltip 名字找程序，返回 (exe, 该 exe 的高分辨率图标)。
    /// 名字认不出或 exe 里没有图标时返回 None，调用方退回抓图。
    pub fn icon_for(&mut self, name: &str, px: i32) -> Option<(String, Vec<u8>)> {
        let exe = self.exe_for(name)?;
        let key = (exe.clone(), px);
        if let Some(v) = self.icons.get(&key) {
            return (!v.is_empty()).then(|| (exe, v.clone()));
        }
        let v = crate::dock::large_icon_of_path(std::path::Path::new(&exe), px);
        let hit = (!v.is_empty()).then(|| (exe.clone(), v.clone()));
        self.icons.insert(key, v);
        hit
    }

    /// tooltip 名字 → exe：先精确命中 `InitialTooltip`，再按包含关系在
    /// 名字/进程名里找最接近的一个。
    pub(crate) fn exe_for(&mut self, name: &str) -> Option<String> {
        let norm = crate::dock::normalize_name(name);
        if norm.len() < 2 {
            return None;
        }
        self.refresh();
        if let Some((_, exe)) = self.table.iter().find(|(k, _)| *k == norm) {
            return Some(exe.clone());
        }
        // 包含匹配：`lxmusic` ←→ `lxmusicdesktop`。长度差太大的不认，
        // 否则"微信"会命中"微信输入法"这类近亲。
        let mut best: Option<(&String, usize)> = None;
        for (k, exe) in &self.table {
            if k.len() < 3 || k.len() + 12 < norm.len() {
                continue;
            }
            if !(norm.contains(k.as_str()) || k.contains(&norm)) {
                continue;
            }
            let score = k.len().abs_diff(norm.len());
            if best.map_or(true, |(_, s)| score < s) {
                best = Some((exe, score));
            }
        }
        best.map(|(exe, _)| exe.clone())
    }

    fn refresh(&mut self) {
        if self.built.map_or(false, |t| t.elapsed() < TABLE_TTL) {
            return;
        }
        let mut table = registry_table();
        table.extend(process_table());
        let mut seen = HashSet::new();
        table.retain(|(k, _)| !k.is_empty() && seen.insert(k.clone()));
        crate::vlog!("[icon] 名字表 {} 条：{:?}", table.len(), table.iter().take(12).collect::<Vec<_>>());
        self.table = table;
        self.built = Some(Instant::now());
    }
}

/// `HKCU\Control Panel\NotifyIconSettings` 下每个托盘图标的登记信息
fn registry_table() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Control Panel\\NotifyIconSettings"), 0, KEY_READ, &mut root)
            != ERROR_SUCCESS
        {
            return out;
        }
        let mut i = 0u32;
        loop {
            let mut nb = [0u16; 128];
            let mut nlen = nb.len() as u32;
            let rc = RegEnumKeyExW(
                root, i, PWSTR(nb.as_mut_ptr()), &mut nlen, None, PWSTR::null(), None, None,
            );
            if rc != ERROR_SUCCESS {
                break;
            }
            i += 1;
            let sub: Vec<u16> = nb[..nlen as usize].iter().copied().chain(std::iter::once(0)).collect();
            let mut hk = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut hk) != ERROR_SUCCESS {
                continue;
            }
            let tooltip = reg_string(hk, w!("InitialTooltip"));
            let exe = reg_string(hk, w!("ExecutablePath"));
            let _ = RegCloseKey(hk);
            let Some(exe) = exe else { continue };
            let exe = exe.to_lowercase();
            if let Some(t) = tooltip {
                out.push((crate::dock::normalize_name(&t), exe.clone()));
            }
            if let Some(stem) = std::path::Path::new(&exe).file_stem().and_then(|s| s.to_str()) {
                out.push((crate::dock::normalize_name(stem), exe));
            }
        }
        let _ = RegCloseKey(root);
    }
    out
}

/// 读一个 REG_SZ 值（UTF-16 直接按 u16 解释）
unsafe fn reg_string(hk: HKEY, name: PCWSTR) -> Option<String> {
    let mut ty = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    if RegQueryValueExW(hk, name, None, Some(&mut ty), None, Some(&mut size)) != ERROR_SUCCESS
        || size < 2
    {
        return None;
    }
    if ty != REG_SZ {
        return None;
    }
    let mut buf = vec![0u8; size as usize + 2];
    let mut got = buf.len() as u32;
    if RegQueryValueExW(hk, name, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut got))
        != ERROR_SUCCESS
    {
        return None;
    }
    let units: Vec<u16> = buf[..got as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    let s = String::from_utf16_lossy(&units).trim().to_string();
    (!s.is_empty() && !s.contains('%')).then_some(s)
}

/// 进程兜底：exe 文件名 → 全路径（注册表里没有的程序靠它认）
fn process_table() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            let exe = crate::tray::exe_of_pid(pe.th32ProcessID);
            if !exe.is_empty() && seen.insert(exe.clone()) {
                if let Some(stem) = std::path::Path::new(&exe).file_stem().and_then(|s| s.to_str()) {
                    out.push((crate::dock::normalize_name(stem), exe));
                }
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}
