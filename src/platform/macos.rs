//! macOS: the lock from the current session's dictionary, the session from
//! the console login record.

use std::ffi::{CStr, c_char, c_void};

type CFTypeRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
}

const UTF8: u32 = 0x0800_0100;

/// Whether the screen is locked.  With no session dictionary at all --
/// at the login window -- there is nobody to show anything to either.
pub fn is_locked() -> bool {
    // SAFETY: CoreFoundation's create/copy rule: what was created or copied
    // here is released here, and the value read from the dictionary is
    // only borrowed while the dictionary lives.
    unsafe {
        let dict = CGSessionCopyCurrentDictionary();
        if dict.is_null() {
            return true;
        }
        let key =
            CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
        let value = CFDictionaryGetValue(dict, key);
        let locked = !value.is_null()
            && CFGetTypeID(value) == CFBooleanGetTypeID()
            && CFBooleanGetValue(value) != 0;
        CFRelease(key);
        CFRelease(dict);
        locked
    }
}

/// The console login of this user, by its time: the same across restarts
/// of the agent, different at the next login.  If the login record cannot
/// be found, the boot stands in for it.
pub fn session_id() -> String {
    console_login().map_or_else(
        || format!("boot-{}", boot_time()),
        |t| format!("console-{t}"),
    )
}

fn console_login() -> Option<i64> {
    // SAFETY: getpwuid and the utmpx functions return pointers into static
    // storage, read here before the next call; this runs once, at start.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let me = CStr::from_ptr((*pw).pw_name).to_owned();
        libc::setutxent();
        let mut latest = None;
        loop {
            let e = libc::getutxent();
            if e.is_null() {
                break;
            }
            let e = &*e;
            if e.ut_type != libc::USER_PROCESS {
                continue;
            }
            let user = CStr::from_ptr(e.ut_user.as_ptr());
            let line = CStr::from_ptr(e.ut_line.as_ptr());
            if user == me.as_c_str() && line.to_bytes() == b"console" {
                latest = latest.max(Some(e.ut_tv.tv_sec));
            }
        }
        libc::endutxent();
        latest
    }
}

fn boot_time() -> i64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: kern.boottime is a struct timeval, and `len` says how big
    // the buffer is.
    let r = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&mut tv as *mut libc::timeval).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r == 0 { tv.tv_sec } else { 0 }
}

/// Not yet: macOS tells only an app bundle granted Location Services which
/// Wi-Fi network it is on.
pub fn ssid() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_without_crashing() {
        // What the answers are depends on the machine; that there are some
        // is what can be tested anywhere.
        let _ = is_locked();
        let s = session_id();
        assert!(s.starts_with("console-") || s.starts_with("boot-"), "{s}");
        assert_eq!(session_id(), s, "the same session twice");
        assert!(boot_time() > 0);
    }
}
