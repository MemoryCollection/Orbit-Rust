//! Windows 系统通知（Toast）：给"通知通道 = Windows 通知"的消息源用。
//!
//! 未打包（unpackaged）桌面应用发 Toast 需要一个已注册的 AUMID。这里首次调用时在
//! `HKCU\Software\Classes\AppUserModelId\Orbit.Notify` 注册一个 DisplayName，
//! 然后用 WinRT `ToastNotificationManager::CreateToastNotifierWithId` +
//! `ToastText02` 模板发通知。
//!
//! WinRT 调用放进专用 MTA 线程（各自 CoInitializeEx），避免与主 UI 线程的 STA
//! 公寓冲突。发失败静默忽略并记日志（Orbit 横幅通道仍可用），不阻塞主流程。

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_SZ,
};
use windows::UI::Notifications::{
    ToastNotification, ToastNotificationManager, ToastTemplateType,
};
use windows::Data::Xml::Dom::XmlDocument;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;

const AUMID: &str = "Orbit.Notify";

struct ToastReq {
    title: String,
    body: String,
}

static TX: OnceLock<Sender<ToastReq>> = OnceLock::new();

/// 在 HKCU 注册 AUMID（DisplayName），未打包应用发 Toast 的前提
fn register_aumid() -> bool {
    let subkey: Vec<u16> = "Software\\Classes\\AppUserModelId\\Orbit.Notify"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let name: Vec<u16> = "DisplayName\0".encode_utf16().collect();
    let value: Vec<u16> = "Orbit 通知\0".encode_utf16().collect();
    unsafe {
        let mut hk = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            0,
            PCWSTR::null(),
            windows::Win32::System::Registry::REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hk,
            None,
        )
        .is_err()
        {
            return false;
        }
        let bytes: Vec<u8> = value
            .iter()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        let rc = RegSetValueExW(hk, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(hk);
        rc.is_ok()
    }
}

/// 发一条 Windows 系统通知（异步，失败静默）
pub fn show(title: &str, body: &str) {
    let tx = TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<ToastReq>();
        let _ = std::thread::Builder::new()
            .name("orbit-toast".into())
            .spawn(move || worker(rx));
        tx
    });
    let _ = tx.send(ToastReq {
        title: title.to_string(),
        body: body.to_string(),
    });
}

fn worker(rx: Receiver<ToastReq>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if !register_aumid() {
            crate::tlog!("[toast] AUMID 注册失败，Windows 通知可能不显示");
        }
        while let Ok(req) = rx.recv() {
            if let Err(e) = send_toast(&req) {
                crate::tlog!("[toast] 发送失败：{e:?}");
            }
        }
        CoUninitialize();
    }
}

fn send_toast(req: &ToastReq) -> windows::core::Result<()> {
    let doc: XmlDocument =
        ToastNotificationManager::GetTemplateContent(ToastTemplateType::ToastText02)?;
    let texts = doc.GetElementsByTagName(&HSTRING::from("text"))?;
    if texts.Length()? >= 1 {
        texts.Item(0)?.SetInnerText(&HSTRING::from(req.title.as_str()))?;
    }
    if texts.Length()? >= 2 {
        texts.Item(1)?.SetInnerText(&HSTRING::from(req.body.as_str()))?;
    }
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let aumid = HSTRING::from(AUMID);
    let manager = ToastNotificationManager::CreateToastNotifierWithId(&aumid)?;
    manager.Show(&toast)?;
    Ok(())
}
