// 一次性诊断工具：打印卷接口列表、卷名、磁盘接口打开情况
// cargo run --release --manifest-path tools/usbdiag/Cargo.toml
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW,
    CM_Get_Device_Interface_PropertyW, CM_Request_Device_EjectW,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
};
use windows::Win32::Devices::Properties::{DEVPROPTYPE, DEVPKEY_Device_InstanceId};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_FLAGS_AND_ATTRIBUTES, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::WindowsProgramming::DRIVE_REMOVABLE;

const GUID_DISK: GUID = GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);
const GUID_VOLUME: GUID = GUID::from_u128(0x53f5630d_b6bf_11d0_94f2_00a0c91efb8b);

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn terminated(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn list_interfaces(guid: &GUID) -> Vec<String> {
    unsafe {
        let mut len = 0u32;
        let cr = CM_Get_Device_Interface_List_SizeW(&mut len, guid, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
        if cr.0 != 0 || len == 0 {
            println!("  List_SizeW CR({}) len={len}", cr.0);
            return vec![];
        }
        let mut buf = vec![0u16; len as usize];
        let cr = CM_Get_Device_Interface_ListW(guid, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
        if cr.0 != 0 {
            println!("  ListW CR({})", cr.0);
            return vec![];
        }
        let mut out = vec![];
        let mut rest: &[u16] = &buf;
        loop {
            let Some(pos) = rest.iter().position(|&c| c == 0) else { break };
            let seg = &rest[..pos];
            rest = &rest[pos + 1..];
            if seg.is_empty() { break; }
            out.push(String::from_utf16_lossy(seg));
        }
        out
    }
}

fn instance_id(path: &str) -> Option<String> {
    unsafe {
        let p = u16s(path);
        let mut ptype = DEVPROPTYPE(0);
        let mut need = 0u32;
        let cr = CM_Get_Device_Interface_PropertyW(
            PCWSTR::from_raw(p.as_ptr()), &DEVPKEY_Device_InstanceId, &mut ptype, None, &mut need, 0);
        if (cr.0 != 0 && cr.0 != 26) || need == 0 { return None; }
        let mut buf = vec![0u8; need as usize];
        let cr = CM_Get_Device_Interface_PropertyW(
            PCWSTR::from_raw(p.as_ptr()), &DEVPKEY_Device_InstanceId, &mut ptype,
            Some(buf.as_mut_ptr()), &mut need, 0);
        if cr.0 != 0 { return None; }
        let idb: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(terminated(&idb))
    }
}

fn device_number_of(h: windows::Win32::Foundation::HANDLE) -> Option<(u32, u32)> {
    #[repr(C)] struct N { dt: u32, dn: u32, pn: u32 }
    unsafe {
        let mut n = N { dt: 0, dn: u32::MAX, pn: 0 };
        let mut ret = 0u32;
        DeviceIoControl(
            h, 0x002D_1080, None, 0,
            Some(&mut n as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<N>() as u32, Some(&mut ret), None,
        ).ok()?;
        Some((n.dt, n.dn))
    }
}

fn terminate(buf: &[u16]) -> String {
    terminated(buf)
}

/// --eject：完整弹出流程，逐步打印父链
fn eject_flow() {
    unsafe {
        use windows::Win32::Devices::DeviceAndDriverInstallation::PNP_VETO_TYPE;
        // 找可移动盘
        let mask = unsafe { GetLogicalDrives() };
        let mut letter = None;
        for i in 0..26u32 {
            if mask & (1 << i) == 0 { continue; }
            let l = (b'A' + i as u8) as char;
            let root = format!("{l}:\\");
            let rw = u16s(&root);
            if unsafe { GetDriveTypeW(PCWSTR::from_raw(rw.as_ptr())) } == DRIVE_REMOVABLE {
                letter = Some(l);
                break;
            }
        }
        let Some(letter) = letter else { println!("没有可移动盘"); return; };
        println!("目标 {letter}:");

        // 磁盘号
        let dp = u16s(&format!("\\\\.\\{letter}:"));
        let h = CreateFileW(PCWSTR::from_raw(dp.as_ptr()), 0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0), None,
            OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None).unwrap();
        let (dt, dn) = device_number_of(h).unwrap();
        CloseHandle(h).unwrap();
        println!("磁盘号 = {dn}（type {dt}）");

        // 磁盘接口按磁盘号匹配（卷节点挂在 ROOT\VOLMGR 下，父链上不去，弃用）
        let mut devinst: Option<u32> = None;
        for p in list_interfaces(&GUID_DISK) {
            let pw = u16s(&p);
            let Ok(h) = CreateFileW(PCWSTR::from_raw(pw.as_ptr()), 0,
                FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0), None,
                OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) else {
                println!("卷接口打不开 {p}");
                continue;
            };
            let n = device_number_of(h);
            CloseHandle(h).unwrap();
            if n != Some((dt, dn)) { continue; }
            let Some(id) = instance_id(&p) else { continue; };
            println!("卷接口匹配：{p}\n  id={id}");
            let idw = u16s(&id);
            let mut d = 0u32;
            if CM_Locate_DevNodeW_pub(&mut d, &idw) == 0 {
                devinst = Some(d);
            }
            break;
        }
        let Some(mut cur) = devinst else { println!("没定位到设备节点"); return; };

        // 父链逐步打印
        let mut target = cur;
        for level in 1..=4 {
            let mut parent = 0u32;
            let cr = CM_Get_Parent_pub(&mut parent, cur);
            if cr != 0 { println!("第{level}层 CM_Get_Parent CR({cr}) 停"); break; }
            let mut idbuf = [0u16; 400];
            let cri = CM_Get_Device_IDW_pub(parent, &mut idbuf);
            if cri != 0 { println!("第{level}层 CM_Get_Device_IDW CR({cri}) 停"); break; }
            let id = terminate(&idbuf);
            println!("第{level}层父节点 = {id}");
            let up = id.to_ascii_uppercase();
            if up.starts_with("USB\\") { target = parent; println!("  ↑ 命中 USB 设备，作为弹出目标"); break; }
            if up.starts_with("USBSTOR\\") && target == devinst.unwrap() {
                target = parent; // 先记着，有 USB\ 再换
            }
            cur = parent;
        }
        println!("弹出目标 = {target}");

        let mut veto = PNP_VETO_TYPE(0);
        let mut veto_name = [0u16; 256];
        let cr = CM_Request_Device_EjectW(target, Some(&mut veto), Some(&mut veto_name), 0);
        println!("CM_Request_Device_EjectW CR({}) veto({}) name={}", cr.0, veto.0, terminate(&veto_name));
    }
}

// 手动声明几个原型的包装（与主程序相同的链接方式）
#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Locate_DevNodeW(dndevinst: *mut u32, pdeviceid: *const u16, ulflags: u32) -> u32;
    fn CM_Get_Parent(pdndevinst: *mut u32, dndevinst: u32, ulflags: u32) -> u32;
    fn CM_Get_Device_IDW(dndevinst: u32, buffer: *mut u16, bufferlen: u32, ulflags: u32) -> u32;
}
unsafe fn CM_Locate_DevNodeW_pub(d: &mut u32, id: &[u16]) -> u32 {
    CM_Locate_DevNodeW(d, id.as_ptr(), 0)
}
unsafe fn CM_Get_Parent_pub(parent: &mut u32, cur: u32) -> u32 {
    CM_Get_Parent(parent, cur, 0)
}
unsafe fn CM_Get_Device_IDW_pub(dev: u32, buf: &mut [u16]) -> u32 {
    CM_Get_Device_IDW(dev, buf.as_mut_ptr(), 400, 0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--eject") {
        eject_flow();
        return;
    }
    // 1. 可移动盘符
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 { continue; }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let rw = u16s(&root);
        unsafe {
            if GetDriveTypeW(PCWSTR::from_raw(rw.as_ptr())) != DRIVE_REMOVABLE { continue; }
            let mut label = [0u16; 64];
            if GetVolumeInformationW(PCWSTR::from_raw(rw.as_ptr()), Some(&mut label), None, None, None, None).is_err() {
                label[0] = 0;
            }
            println!("盘符 {letter}: 标签={}", terminated(&label));
            let mut name = [0u16; 128];
            match GetVolumeNameForVolumeMountPointW(PCWSTR::from_raw(rw.as_ptr()), &mut name) {
                Ok(_) => println!("  卷名 = {}", terminated(&name)),
                Err(e) => println!("  卷名获取失败 {e}"),
            }
            // 卷句柄 → 磁盘号
            let dp = u16s(&format!("\\\\.\\{letter}:"));
            match CreateFileW(PCWSTR::from_raw(dp.as_ptr()), GENERIC_READ.0,
                FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0), None, OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0), None) {
                Ok(h) => {
                    let dn = device_number_of(h);
                    let _ = CloseHandle(h);
                    println!("  卷句柄打开成功，磁盘号 = {dn:?}");
                }
                Err(e) => println!("  卷句柄打开失败 {e}"),
            }
        }
    }

    // 2. 卷接口列表
    println!("\n== GUID_DEVINTERFACE_VOLUME 接口 ==");
    for p in list_interfaces(&GUID_VOLUME) {
        let id = instance_id(&p).unwrap_or_default();
        println!("  {p}\n    id={id}");
    }

    // 3. 磁盘接口列表 + 各种打开方式
    println!("\n== GUID_DEVINTERFACE_DISK 接口 ==");
    for p in list_interfaces(&GUID_DISK) {
        let id = instance_id(&p).unwrap_or_default();
        println!("  {p}\n    id={id}");
        let pw = u16s(&p);
        unsafe {
            for (tag, access) in [("GENERIC_READ", GENERIC_READ.0), ("0", 0u32)] {
                match CreateFileW(PCWSTR::from_raw(pw.as_ptr()), access,
                    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0), None,
                    OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) {
                    Ok(h) => {
                        let dn = device_number_of(h);
                        let _ = CloseHandle(h);
                        println!("    [{tag}] 打开成功 磁盘号={dn:?}");
                    }
                    Err(e) => println!("    [{tag}] 打开失败 {e}"),
                }
            }
        }
    }
}
