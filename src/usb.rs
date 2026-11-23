//! U 盘模块：可移动磁盘的发现 / 打开 / 安全弹出。
//!
//! 检测不用 WM_DEVICECHANGE（设备广播只发给注册了通知的窗口，时序还和托盘宿主
//! 抢注纠缠），改为 500ms 轮询 `GetLogicalDrives` + `GetDriveTypeW`：与胶囊时钟
//! 定时器同频，26 个盘符一次位图查询的开销可以忽略。
//!
//! 「安全弹出」走 CfgMgr，与资源管理器右键"弹出"同一条路径：
//! 盘符 → `IOCTL_STORAGE_GET_DEVICE_NUMBER` 拿物理磁盘号 →
//! 枚举 `GUID_DEVINTERFACE_DISK` 接口匹配同一块盘 → 接口属性拿设备实例 ID →
//! `CM_Locate_DevNodeW` 得到设备节点 → 若父节点是 USB 设备（USB\VID_...）则
//! 弹出父节点（整块盘连同多分区一起走）→ `CM_Request_Device_EjectW`。
//! 弹出失败时把系统的否决原因（文件被占用等）翻译成人话回给面板。

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW,
    CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_Locate_DevNodeW,
    CM_Request_Device_EjectW, CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_LOCATE_DEVNODE_NORMAL,
    CR_BUFFER_SMALL, CR_REMOVE_VETOED, CR_SUCCESS, PNP_VETO_TYPE,
};
use windows::Win32::Devices::Properties::{DEVPROPTYPE, DEVPKEY_Device_InstanceId};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_FLAGS_AND_ATTRIBUTES, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::WindowsProgramming::DRIVE_REMOVABLE;

// windows crate 没把这几个小东西导出来（IOCTL、存储设备号结构、PNP_VETO 枚举值），
// 手写一份，数值来自 winioctl.h / cfg.h，都是稳定 ABI
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
/// FILE_DEVICE_DISK
const FILE_DEVICE_DISK: u32 = 0x0000_0007;
#[repr(C)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

// PNP_VETO_TYPE（cfg.h）：只挑弹出场景会遇到的几个
const VETO_PENDING_CLOSE: i32 = 1;
const VETO_WINDOWS_APP: i32 = 2;
const VETO_DISK_IN_USE: i32 = 4;
const VETO_LEGACY_DRIVER: i32 = 0;
const VETO_UNKNOWN: i32 = 14;

/// GUID_DEVINTERFACE_DISK（devguid.h 的 {53f56307-b6bf-11d0-94f2-00a0c91efb8b}）
const GUID_DEVINTERFACE_DISK: GUID =
    GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// 一个可移动磁盘卷（面板一行的数据）
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UsbVol {
    /// 盘符带冒号，如 "E:"
    pub letter: String,
    /// 卷标（空 = 显示"可移动磁盘"）
    pub label: String,
    /// 容量行，如 "剩 12.3 GB · 共 32.0 GB"（拿不到就说明原因）
    pub cap: String,
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 重新扫描可移动磁盘，与 `vols` 不同才替换并返回 true（驱动 500ms 轮询）
pub fn refresh(vols: &mut Vec<UsbVol>) -> bool {
    let cur = scan();
    let changed = *vols != cur;
    if changed {
        *vols = cur;
    }
    changed
}

/// 枚举所有"可移动磁盘"类型的盘符，读卷标与容量
fn scan() -> Vec<UsbVol> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let root_w = u16s(&root);
        unsafe {
            if GetDriveTypeW(PCWSTR::from_raw(root_w.as_ptr())) != DRIVE_REMOVABLE {
                continue;
            }
            // 卷标（失败不拦，U 盘没写卷标很常见）
            let mut label = [0u16; 64];
            if GetVolumeInformationW(
                PCWSTR::from_raw(root_w.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                None,
            )
            .is_err()
            {
                label[0] = 0;
            }
            let label = terminated(&label);
            // 容量（没插好 / 独占打开时可能拿不到）
            let (mut total, mut free) = (0u64, 0u64);
            let cap = if GetDiskFreeSpaceExW(
                PCWSTR::from_raw(root_w.as_ptr()),
                None,
                Some(&mut total),
                Some(&mut free),
            )
            .is_ok()
                && total > 0
            {
                format!("剩 {:.1} GB · 共 {:.1} GB", free as f64 / GB, total as f64 / GB)
            } else {
                "无法读取容量".to_string()
            };
            out.push(UsbVol {
                letter: format!("{letter}:"),
                label: label.trim().to_string(),
                cap,
            });
        }
    }
    out
}

/// 资源管理器打开该盘符根目录
pub fn open(letter: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let root = u16s(&format!("{letter}\\"));
    unsafe {
        let _ = ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR::from_raw(root.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// 安全弹出。成功 = 已向系统请求移除；失败 = 返回否决原因（人话）。
pub fn eject(letter: &str) -> Result<(), String> {
    unsafe {
        // 盘符 → 物理磁盘号（卷句柄 IOCTL，这台机器实测可用）
        let num = volume_disk_number(letter).ok_or("无法访问磁盘（可能被独占打开）")?;
        // 必须走磁盘接口：STORAGE\Volume 卷节点挂在 ROOT\VOLMGR 根下，
        // 父链上溯永远到不了 USB 设备（实测）；磁盘接口的节点才是 USBSTOR\Disk
        let devinst = interface_by_number(&GUID_DEVINTERFACE_DISK, num)
            .ok_or("找不到对应的磁盘设备")?;

        // 沿父链往上找第一个 USB\ 设备（卷 → 磁盘 → USB 设备，最多 3 层），
        // 找到就弹它（整块盘连同多分区一起走）；没走到 USB 就弹当前节点本身
        let mut target = devinst;
        let mut cur = devinst;
        for _ in 0..3 {
            let mut parent = 0u32;
            if CM_Get_Parent(&mut parent, cur, 0) != CR_SUCCESS {
                break;
            }
            let mut idbuf = [0u16; 400];
            if CM_Get_Device_IDW(parent, &mut idbuf, 0) != CR_SUCCESS {
                break;
            }
            let id = terminated(&idbuf);
            if id.to_ascii_uppercase().starts_with("USB\\") {
                target = parent;
                break;
            }
            cur = parent;
        }

        let mut veto_name = [0u16; 256];
        let mut veto = PNP_VETO_TYPE(0);
        let cr = CM_Request_Device_EjectW(target, Some(&mut veto), Some(&mut veto_name), 0);
        let name = terminated(&veto_name);
        // 有的系统路径上否决也会带着 CR_SUCCESS 返回，所以成功与否要看两处
        let vetoed = cr == CR_REMOVE_VETOED
            || (cr == CR_SUCCESS && veto.0 != VETO_UNKNOWN && !name.is_empty());
        if !vetoed {
            crate::tlog!("[usb] 弹出 {letter}：CR({}) veto({}) name={}", cr.0, veto.0, name);
            return Ok(());
        }
        Err(veto_text(veto.0, &name))
    }
}

/// 系统否决原因 → 人话（name 是驱动报的文件/进程名，有就带上）
fn veto_text(veto: i32, name: &str) -> String {
    let why = match veto {
        VETO_DISK_IN_USE => "有文件正被打开或占用",
        VETO_PENDING_CLOSE => "有程序刚访问过，稍等一下再试",
        VETO_WINDOWS_APP => "被 Windows 应用占用",
        VETO_LEGACY_DRIVER => "被旧版驱动占用",
        _ => "被系统否决",
    };
    if name.is_empty() {
        format!("{why}")
    } else {
        format!("{why}（{name}）")
    }
}

/// UTF-16 缓冲里的首个 NUL 结尾串
fn terminated(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 盘符 → 物理磁盘号（IOCTL_STORAGE_GET_DEVICE_NUMBER）。
/// 0 权限打开就够这个 IOCTL 用；GENERIC_READ 在部分系统上会被拒（0x80070005）
fn volume_disk_number(letter: &str) -> Option<u32> {
    unsafe {
        let path = u16s(&format!("\\\\.\\{letter}"));
        let h = open_device(PCWSTR::from_raw(path.as_ptr()))?;
        let n = disk_number_of(h);
        let _ = CloseHandle(h);
        n
    }
}

/// 0 权限打开设备（这个 IOCTL 不需要读权限；0 被拒再退回 GENERIC_READ）
fn open_device(path: PCWSTR) -> Option<HANDLE> {
    unsafe {
        let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0);
        match CreateFileW(path, 0, share, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) {
            Ok(h) => Some(h),
            Err(_) => CreateFileW(
                path,
                GENERIC_READ.0,
                share,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
            .ok(),
        }
    }
}

/// 对一个已打开的句柄问磁盘号（卷句柄和磁盘接口句柄都走这个）
fn disk_number_of(h: HANDLE) -> Option<u32> {
    unsafe {
        let mut num = StorageDeviceNumber {
            device_type: 0,
            device_number: u32::MAX,
            partition_number: 0,
        };
        let mut ret = 0u32;
        DeviceIoControl(
            h,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut num as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<StorageDeviceNumber>() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
        (num.device_type == FILE_DEVICE_DISK && num.device_number != u32::MAX)
            .then_some(num.device_number)
    }
}

/// 在"当前存在的接口"里按物理磁盘号找出设备节点。
///
/// 卷接口 / 磁盘接口通吃：路径格式系统间不一致（这台 Win11 的卷接口是
/// `\\?\STORAGE#Volume#...`，不是 `\\?\Volume{GUID}`），所以不比对路径文本，
/// 统一 0 权限打开句柄 → IOCTL 拿磁盘号 → 匹配 → 接口属性拿设备节点。
fn interface_by_number(guid: &GUID, disk_number: u32) -> Option<u32> {
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(
            &mut len,
            guid,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != CR_SUCCESS
            || len == 0
        {
            crate::tlog!("[usb] 枚举接口失败（len={len}）");
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(
            guid,
            PCWSTR::null(),
            &mut buf,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != CR_SUCCESS
        {
            crate::tlog!("[usb] 读接口列表失败");
            return None;
        }
        // 多重 NUL 分隔的接口路径列表，逐段打开比对磁盘号
        let mut rest: &[u16] = &buf;
        loop {
            let Some(pos) = rest.iter().position(|&c| c == 0) else { break };
            let seg = &rest[..pos];
            rest = &rest[pos + 1..];
            if seg.is_empty() {
                break; // 连续 NUL = 列表结束
            }
            let path = PCWSTR::from_raw(seg.as_ptr());
            let Some(h) = open_device(path) else {
                let p = String::from_utf16_lossy(seg);
                crate::tlog!("[usb] 打不开接口 {p}");
                continue;
            };
            let n = disk_number_of(h);
            let _ = CloseHandle(h);
            if n != Some(disk_number) {
                continue;
            }
            if let Some(d) = interface_devinst(path) {
                return Some(d);
            }
        }
        None
    }
}

/// 设备接口路径 → 设备节点：接口属性拿实例 ID，再定位设备树节点
fn interface_devinst(path: PCWSTR) -> Option<u32> {
    unsafe {
        let mut ptype = DEVPROPTYPE(0);
        let mut need = 0u32;
        // 先查大小：buffer 传 None 时系统按约定返回 CR_BUFFER_SMALL 并把长度填进 need，
        // 只有参数非法等才是真失败
        let cr = CM_Get_Device_Interface_PropertyW(
            path,
            &DEVPKEY_Device_InstanceId,
            &mut ptype,
            None,
            &mut need,
            0,
        );
        if (cr != CR_SUCCESS && cr != CR_BUFFER_SMALL) || need == 0 {
            crate::tlog!("[usb] 查实例 ID 长度失败 CR({})", cr.0);
            return None;
        }
        let mut idbuf = vec![0u8; need as usize];
        if CM_Get_Device_Interface_PropertyW(
            path,
            &DEVPKEY_Device_InstanceId,
            &mut ptype,
            Some(idbuf.as_mut_ptr()),
            &mut need,
            0,
        ) != CR_SUCCESS
        {
            crate::tlog!("[usb] 读实例 ID 失败（need={need}）");
            return None;
        }
        let idbytes: Vec<u16> = idbuf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let id = terminated(&idbytes);
        let mut devinst = 0u32;
        let idw = u16s(&id);
        if CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR::from_raw(idw.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        ) == CR_SUCCESS
        {
            return Some(devinst);
        }
        crate::tlog!("[usb] 定位设备节点失败 id={id}");
        None
    }
}
