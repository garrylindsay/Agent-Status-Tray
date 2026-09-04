//! The one place this program talks to the network.
//!
//! Everything else reads local files. This exists so the Cursor cost column can ask Cursor what a
//! conversation was charged, because that number is not written to disk anywhere.
//!
//! It goes through WinHTTP rather than a Rust HTTP crate on purpose: the transport, the TLS and the
//! proxy configuration are already in the operating system, they are already what every other app
//! on the machine uses, and reaching for them adds nothing to the binary and no third party to
//! trust with a session token.
//!
//! Every handle is owned by [`Handle`], which closes it on drop. A leak here would be the same
//! class of bug as the one that used to exhaust GDI handles and blank the menu, except it would
//! exhaust sockets instead.

use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};

/// Long enough for a slow network, short enough that a hung call cannot wedge a poll.
const TIMEOUT_MS: i32 = 15_000;

/// Refuses to grow without bound on a response that never ends.
const MAX_BODY: usize = 8 * 1024 * 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A WinHTTP handle that closes itself.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> Option<Handle> {
        if raw.is_null() { None } else { Some(Handle(raw)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// POSTs a JSON body and reads the whole response.
///
/// `headers` is the raw header block, newline separated, as WinHTTP expects it.
pub fn post_json(host: &str, path: &str, headers: &str, body: &[u8]) -> Result<Response, String> {
    unsafe {
        let session = Handle::new(WinHttpOpen(
            wide("agent-status-tray").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            ptr::null(),
            ptr::null(),
            0,
        ))
        .ok_or("could not start WinHTTP")?;

        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);

        let connection = Handle::new(WinHttpConnect(
            session.0,
            wide(host).as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ))
        .ok_or_else(|| format!("could not reach {host}"))?;

        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            wide("POST").as_ptr(),
            wide(path).as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            WINHTTP_FLAG_SECURE,
        ))
        .ok_or("could not open the request")?;

        let header_block = wide(headers);
        let sent = WinHttpSendRequest(
            request.0,
            header_block.as_ptr(),
            u32::MAX, // count the header block for me
            body.as_ptr() as *const c_void,
            body.len() as u32,
            body.len() as u32,
            0,
        );
        if sent == 0 {
            return Err(format!("send failed ({})", last_error()));
        }

        if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
            return Err(format!("no response ({})", last_error()));
        }

        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut size,
            ptr::null_mut(),
        );

        let mut buf: Vec<u8> = Vec::new();
        loop {
            let mut available: u32 = 0;
            if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                return Err(format!("read failed ({})", last_error()));
            }
            if available == 0 {
                break;
            }
            let start = buf.len();
            if start + available as usize > MAX_BODY {
                return Err("response too large".to_string());
            }
            buf.resize(start + available as usize, 0);

            let mut read: u32 = 0;
            if WinHttpReadData(
                request.0,
                buf.as_mut_ptr().add(start) as *mut c_void,
                available,
                &mut read,
            ) == 0
            {
                return Err(format!("read failed ({})", last_error()));
            }
            buf.truncate(start + read as usize);
            if read == 0 {
                break;
            }
        }

        Ok(Response {
            status: status as u16,
            body: String::from_utf8_lossy(&buf).into_owned(),
        })
    }
}

fn last_error() -> String {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    match code {
        12002 => "timed out".to_string(),
        12007 => "host not found".to_string(),
        12029 => "could not connect".to_string(),
        12175 => "TLS error".to_string(),
        other => format!("error {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hostname that cannot resolve must come back as an error rather than hanging or panicking,
    /// because this runs on the same thread as the tray's message loop.
    #[test]
    fn an_unreachable_host_fails_rather_than_hangs() {
        let result = post_json(
            "no-such-host.invalid",
            "/",
            "Content-Type: application/json",
            b"{}",
        );
        assert!(result.is_err(), "expected an error, got {:?}", result.is_ok());
    }
}
