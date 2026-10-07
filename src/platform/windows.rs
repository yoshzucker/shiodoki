//! Windows: the lock and the session from the Terminal Services API, the
//! Wi-Fi network from the WLAN API.

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WiFi::{
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces,
    WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, wlan_interface_state_connected,
    wlan_intf_opcode_current_connection,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_INFO_CLASS, WTSFreeMemory, WTSINFOEXW,
    WTSINFOW, WTSQuerySessionInformationW, WTSSessionInfo, WTSSessionInfoEx,
};

/// Query this session's information of `class`, as a `T`.
fn query<T: Copy>(class: WTS_INFO_CLASS) -> Option<T> {
    let mut buf = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: on success the API hands back a buffer of `len` bytes that
    // is ours to free with WTSFreeMemory; it is read as a `T` only when it
    // is at least that big.
    unsafe {
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            class,
            &mut buf,
            &mut len,
        ) == 0
        {
            return None;
        }
        let value = (len as usize >= std::mem::size_of::<T>()).then(|| *(buf as *const T));
        WTSFreeMemory(buf.cast());
        value
    }
}

/// Whether the session is locked.  If Windows cannot say, unlocked: a
/// rule that waits for an unlock should not wait for ever on a question
/// that has no answer.
pub fn is_locked() -> bool {
    match query::<WTSINFOEXW>(WTSSessionInfoEx) {
        // SAFETY: level 1 is the only level there is, and says which arm
        // of the union is filled.
        Some(info) if info.Level == 1 => unsafe { info.Data.WTSInfoExLevel1.SessionFlags == 0 },
        _ => false,
    }
}

/// The session by its number and logon time.
pub fn session_id() -> String {
    match query::<WTSINFOW>(WTSSessionInfo) {
        Some(info) => format!("session-{}-{}", info.SessionId, info.LogonTime),
        None => format!("agent-{}", std::process::id()),
    }
}

/// The Wi-Fi network the first connected wireless interface is on.  On
/// Windows 11 24H2 and later this needs desktop apps to be allowed the
/// location; without it the answer is no network.
pub fn ssid() -> Option<String> {
    // SAFETY: the WLAN API's handle and lists are opened, read and freed
    // here; every list is read only within its item count.
    unsafe {
        let mut version = 0u32;
        let mut handle: HANDLE = std::ptr::null_mut();
        if WlanOpenHandle(2, std::ptr::null(), &mut version, &mut handle) != 0 {
            return None;
        }
        let mut found = None;
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(handle, std::ptr::null(), &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            for item in items
                .iter()
                .filter(|i| i.isState == wlan_interface_state_connected)
            {
                let mut size = 0u32;
                let mut data = std::ptr::null_mut();
                let r = WlanQueryInterface(
                    handle,
                    &item.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    std::ptr::null(),
                    &mut size,
                    &mut data,
                    std::ptr::null_mut(),
                );
                if r == 0 && !data.is_null() {
                    let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let ssid = &attrs.wlanAssociationAttributes.dot11Ssid;
                    let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
                    if len > 0 {
                        found = Some(String::from_utf8_lossy(&ssid.ucSSID[..len]).into_owned());
                    }
                    WlanFreeMemory(data);
                }
                if found.is_some() {
                    break;
                }
            }
            WlanFreeMemory(list.cast());
        }
        WlanCloseHandle(handle, std::ptr::null());
        found
    }
}
