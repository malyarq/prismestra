// SPDX-License-Identifier: GPL-2.0-or-later
//! Windows-native, low-rate Home Assistant whole-light REST output.
//! Independent implementation of the documented API; no HA source copied.
//! References checked 2026-11-08:
//! https://developers.home-assistant.io/docs/api/rest/
//! https://www.home-assistant.io/integrations/light/
//! https://learn.microsoft.com/en-us/windows/win32/winhttp/option-flags
//! https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpsettimeouts
//! No discovery, credentials persistence, proxy, redirects, retries or worker.
//! HTTP exposes Bearer credentials on the configured network; prefer HTTPS.
//! HTTPS uses WinHTTP's certificate validation without security exceptions.
//! A successful service response is submission, not physical light confirmation.

use crate::devices::home_assistant::LightCapability;
use serde_json::Value;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::Instant;
use windows_sys::Win32::Networking::WinHttp::*;

pub const DEFAULT_PORT: u16 = 8123;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_HEADER_BYTES: u32 = 16 * 1024;
const IO_TIMEOUT_MS: i32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    Cancelled,
    Deadline,
    Authentication,
    MissingEntity,
    EntityUnavailable,
    UnsupportedCapabilities,
    Http(u16),
    WinHttp(u32),
    ResponseTooLarge,
    InvalidResponse,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Error variants contain no URL, token, server text or request body.
        write!(f, "Home Assistant: {self:?}")
    }
}
impl std::error::Error for Error {}

pub struct Endpoint {
    host: Vec<u16>,
    port: u16,
    secure: bool,
    base_path: String,
}

impl Endpoint {
    /// Explicit http(s) endpoint, optionally with a reverse-proxy base path.
    /// An omitted port follows URL semantics: HTTP 80, HTTPS 443. HA's direct
    /// port DEFAULT_PORT (8123) must be explicit in the URL. The intentionally
    /// strict URL subset excludes percent escapes, userinfo, query, fragments,
    /// backslashes, non-ASCII and dot segments to avoid ambiguous routing.
    pub fn parse(url: &str) -> Result<Self, Error> {
        if url.len() > 2048
            || !url.is_ascii()
            || url.bytes().any(|b| b <= 32 || b == 127)
            || url.contains(['@', '?', '#', '\\', '%'])
        {
            return Err(Error::InvalidInput);
        }
        let authority = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .ok_or(Error::InvalidInput)?
            .split('/')
            .next()
            .ok_or(Error::InvalidInput)?;
        if authority.is_empty() {
            return Err(Error::InvalidInput);
        }
        let explicit_port = if authority.starts_with('[') {
            let close = authority.find(']').ok_or(Error::InvalidInput)?;
            let suffix = &authority[close + 1..];
            if !suffix.is_empty() && !suffix.starts_with(':') {
                return Err(Error::InvalidInput);
            }
            suffix.strip_prefix(':')
        } else {
            authority.split_once(':').map(|(_, port)| port)
        };
        let explicit_port = match explicit_port {
            Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => Some(
                text.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or(Error::InvalidInput)?,
            ),
            Some(_) => return Err(Error::InvalidInput),
            None => None,
        };
        let url_wide = wide(url);
        let mut parts = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            dwHostNameLength: u32::MAX,
            dwUserNameLength: u32::MAX,
            dwPasswordLength: u32::MAX,
            dwUrlPathLength: u32::MAX,
            dwExtraInfoLength: u32::MAX,
            ..Default::default()
        };
        // Requested component pointers borrow the live `wide` buffer only here.
        if unsafe {
            WinHttpCrackUrl(
                url_wide.as_ptr(),
                (url_wide.len() - 1) as u32,
                0,
                &mut parts,
            )
        } == 0
            || parts.dwHostNameLength == 0
            || parts.dwUserNameLength != 0
            || parts.dwPasswordLength != 0
            || parts.dwExtraInfoLength != 0
            || !matches!(
                parts.nScheme,
                WINHTTP_INTERNET_SCHEME_HTTP | WINHTTP_INTERNET_SCHEME_HTTPS
            )
        {
            return Err(Error::InvalidInput);
        }
        let host = component(parts.lpszHostName, parts.dwHostNameLength)?;
        let path = component(parts.lpszUrlPath, parts.dwUrlPathLength)?;
        if path.split('/').any(|p| p == "." || p == "..") || path.contains("//") {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            host: wide(&host),
            port: explicit_port.unwrap_or(parts.nPort),
            secure: parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS,
            base_path: path.trim_end_matches('/').to_owned(),
        })
    }
}

fn component(ptr: *const u16, length: u32) -> Result<String, Error> {
    if length == 0 {
        return Ok(String::new());
    }
    if ptr.is_null() || length > 2048 {
        return Err(Error::InvalidInput);
    }
    // WinHttpCrackUrl returned a bounded slice inside its input buffer.
    String::from_utf16(unsafe { std::slice::from_raw_parts(ptr, length as usize) })
        .map_err(|_| Error::InvalidInput)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Light {
    pub entity_id: String,
    pub name: Option<String>,
    pub state: Option<String>,
    pub supported_color_modes: Vec<String>,
}

impl Light {
    pub fn capability(&self) -> Option<LightCapability> {
        LightCapability::from_modes(&self.supported_color_modes)
    }
}

// Deliberately no Debug/Clone: the Bearer token is private and never formatted.
pub struct Client {
    endpoint: Endpoint,
    token: String,
    entity: Option<String>,
    capability: Option<LightCapability>,
}

impl Client {
    pub fn new(endpoint: Endpoint, token: String) -> Result<Self, Error> {
        // RFC6750 b64token characters; no whitespace/header injection.
        if token.is_empty()
            || token.len() > 8192
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
        {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            endpoint,
            token,
            entity: None,
            capability: None,
        })
    }

    pub fn lights(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<Light>, Error> {
        let body = self.request("GET", "/api/states", &[], deadline, &mut cancelled, false)?;
        let value: Value = serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)?;
        let states = value.as_array().ok_or(Error::InvalidResponse)?;
        let mut lights = Vec::new();
        for state in states {
            let Some(id) = state["entity_id"].as_str().filter(|id| valid_entity(id)) else {
                continue;
            };
            if lights.len() >= 4096 {
                return Err(Error::ResponseTooLarge);
            }
            let attrs = &state["attributes"];
            lights.push(Light {
                entity_id: id.to_owned(),
                name: bounded_text(&attrs["friendly_name"]),
                state: bounded_text(&state["state"]),
                supported_color_modes: attrs["supported_color_modes"]
                    .as_array()
                    .filter(|modes| modes.len() <= 32)
                    .and_then(|modes| modes.iter().map(bounded_text).collect::<Option<Vec<_>>>())
                    .unwrap_or_default(),
            });
        }
        check(deadline, &mut cancelled)?;
        Ok(lights)
    }

    /// Explicitly bind one entity after GET-state validation. Clears any previous
    /// binding even on failure. Call again to refresh after a suspected removal.
    pub fn bind_light(
        &mut self,
        entity: &str,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.entity = None;
        self.capability = None;
        if !valid_entity(entity) {
            return Err(Error::InvalidInput);
        }
        let state = self.request(
            "GET",
            &format!("/api/states/{entity}"),
            &[],
            deadline,
            &mut cancelled,
            true,
        )?;
        let state: Value = serde_json::from_slice(&state).map_err(|_| Error::InvalidResponse)?;
        if state["entity_id"].as_str() != Some(entity) {
            return Err(Error::InvalidResponse);
        }
        match state["state"].as_str() {
            Some("on" | "off") => (),
            Some("unknown" | "unavailable") => return Err(Error::EntityUnavailable),
            _ => return Err(Error::InvalidResponse),
        }
        let capability = LightCapability::from_attributes(&state["attributes"])
            .ok_or(Error::UnsupportedCapabilities)?;
        check(deadline, &mut cancelled)?;
        self.entity = Some(entity.to_owned());
        self.capability = Some(capability);
        Ok(())
    }

    /// Control the bound whole light, at low rate, with one fixed-service POST.
    /// Black means turn_off; otherwise RGB8 peak becomes brightness (1..255),
    /// with normalized RGB chromaticity for color lights. Dimmable white lights
    /// receive brightness only; on/off lights receive no color or brightness.
    /// HA translates RGB to the supported color format. A later removal can return HTTP
    /// success: this reports only service acceptance, never confirmed state.
    /// An error/cancellation after sending cannot roll back an applied command.
    pub fn set_color(
        &self,
        rgb: [u8; 3],
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), Error> {
        let entity = self.entity.as_deref().ok_or(Error::InvalidInput)?;
        let capability = self.capability.ok_or(Error::InvalidInput)?;
        let (service, data) = capability.command(entity, rgb);
        let body = serde_json::to_vec(&data).map_err(|_| Error::InvalidInput)?;
        self.request(
            "POST",
            &format!("/api/services/light/{service}"),
            &body,
            deadline,
            &mut cancelled,
            false,
        )?;
        Ok(())
    }

    fn request(
        &self,
        method: &str,
        suffix: &str,
        body: &[u8],
        deadline: Instant,
        cancelled: &mut impl FnMut() -> bool,
        entity_lookup: bool,
    ) -> Result<Vec<u8>, Error> {
        check(deadline, cancelled)?;
        let agent = wide("PC-Control/HA");
        let session = Handle::new(unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                null(),
                null(),
                0,
            )
        })?;
        timeouts(&session, deadline, cancelled)?;
        let connection = Handle::new(unsafe {
            WinHttpConnect(
                session.0,
                self.endpoint.host.as_ptr(),
                self.endpoint.port,
                0,
            )
        })?;
        let method = wide(method);
        let path = wide(&format!("{}{suffix}", self.endpoint.base_path));
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                method.as_ptr(),
                path.as_ptr(),
                null(),
                null(),
                null(),
                if self.endpoint.secure {
                    WINHTTP_FLAG_SECURE
                } else {
                    0
                },
            )
        })?;
        option(
            &request,
            WINHTTP_OPTION_DISABLE_FEATURE,
            WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION,
        )?;
        option(
            &request,
            WINHTTP_OPTION_MAX_RESPONSE_HEADER_SIZE,
            MAX_HEADER_BYTES,
        )?;
        let headers = wide(&format!(
            "Authorization: Bearer {}\r\nContent-Type: application/json\r\n",
            self.token
        ));
        timeouts(&request, deadline, cancelled)?;
        let result = unsafe {
            WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                (headers.len() - 1) as u32,
                body.as_ptr().cast(),
                body.len() as u32,
                body.len() as u32,
                0,
            )
        };
        checked(result, deadline, cancelled)?;
        timeouts(&request, deadline, cancelled)?;
        checked(
            unsafe { WinHttpReceiveResponse(request.0, null_mut()) },
            deadline,
            cancelled,
        )?;
        let status =
            numeric_header(&request, WINHTTP_QUERY_STATUS_CODE)?.ok_or(Error::InvalidResponse)?;
        match status {
            401 => return Err(Error::Authentication),
            404 if entity_lookup => return Err(Error::MissingEntity),
            200..=299 => (),
            code => return Err(Error::Http(code as u16)),
        }
        if numeric_header(&request, WINHTTP_QUERY_CONTENT_LENGTH)?
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let mut response = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            timeouts(&request, deadline, cancelled)?;
            let mut read = 0;
            checked(
                unsafe {
                    WinHttpReadData(
                        request.0,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                        &mut read,
                    )
                },
                deadline,
                cancelled,
            )?;
            if read == 0 {
                break;
            }
            if read as usize > MAX_RESPONSE_BYTES - response.len() {
                return Err(Error::ResponseTooLarge);
            }
            response.extend_from_slice(&buffer[..read as usize]);
        }
        Ok(response)
    }
}

fn valid_entity(entity: &str) -> bool {
    entity.len() <= 255
        && entity.strip_prefix("light.").is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}
fn bounded_text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| s.len() <= 256).map(str::to_owned)
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn native_error() -> Error {
    Error::WinHttp(std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32)
}
struct Handle(*mut c_void);
impl Handle {
    fn new(raw: *mut c_void) -> Result<Self, Error> {
        if raw.is_null() {
            Err(native_error())
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
fn option(handle: &Handle, key: u32, value: u32) -> Result<(), Error> {
    if unsafe { WinHttpSetOption(handle.0, key, (&value as *const u32).cast(), 4) } == 0 {
        Err(native_error())
    } else {
        Ok(())
    }
}
fn check(deadline: Instant, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    if cancelled() {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::Deadline)
    } else {
        Ok(())
    }
}
fn checked(
    result: i32,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Error> {
    let error = if result == 0 {
        Some(native_error())
    } else {
        None
    };
    check(deadline, cancelled)?;
    error.map_or(Ok(()), Err)
}
fn timeouts(
    handle: &Handle,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Error> {
    check(deadline, cancelled)?;
    let millis = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .clamp(1, IO_TIMEOUT_MS as u128) as i32;
    if unsafe { WinHttpSetTimeouts(handle.0, millis, millis, millis, millis) } == 0 {
        Err(native_error())
    } else {
        Ok(())
    }
}
fn numeric_header(handle: &Handle, key: u32) -> Result<Option<u64>, Error> {
    // WinHTTP's portable numeric query is DWORD-only. Ask WinHTTP for this
    // individual header in a fixed buffer, then parse its decimal value as u64.
    let mut text = [0u16; 64];
    let mut size = std::mem::size_of_val(&text) as u32;
    if unsafe {
        WinHttpQueryHeaders(
            handle.0,
            key,
            null(),
            text.as_mut_ptr().cast(),
            &mut size,
            null_mut(),
        )
    } != 0
    {
        if size as usize >= std::mem::size_of_val(&text) || !size.is_multiple_of(2) {
            return Err(Error::InvalidResponse);
        }
        let text =
            String::from_utf16(&text[..size as usize / 2]).map_err(|_| Error::InvalidResponse)?;
        Ok(Some(
            text.trim().parse().map_err(|_| Error::InvalidResponse)?,
        ))
    } else if std::io::Error::last_os_error().raw_os_error()
        == Some(ERROR_WINHTTP_HEADER_NOT_FOUND as i32)
    {
        Ok(None)
    } else {
        Err(native_error())
    }
}

// Synchronous WinHTTP operations are bounded individually by <=500ms timeouts.
// Cancellation is cooperative before/after I/O, not an abort of in-flight I/O;
// Windows DNS/multiple-address connection work may overrun an absolute deadline.
#[cfg(test)]
#[path = "home_assistant_tests.rs"]
mod tests;
