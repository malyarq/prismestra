//! Shared completion lifetime for native feature and PawnIO requests.
use crate::runtime::output::Cancellation;
use std::{
    ptr::null,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::{CreateEventW, ResetEvent, Sleep, WaitForSingleObject},
    },
};
struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// # Safety
/// `handle` must stay valid until this returns. Every pointer passed to the
/// start callback must remain valid until completion/drain; the callback must
/// return the immediate OS result, not retain pointers or panic after submission.
/// Nominal250ms, but final cancellation drain can wait on a broken kernel.
pub(crate) unsafe fn run(
    handle: HANDLE,
    cancel: &Cancellation,
    begin: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
) -> Result<usize, String> {
    unsafe { run_for(handle, cancel, Duration::from_millis(250), begin) }
}

/// Same pointer/handle lifetime requirements as run. Final cancellation drain
/// still depends on the kernel; this is a nominal deadline, not a hard bound.
pub(crate) unsafe fn run_for(
    handle: HANDLE,
    cancel: &Cancellation,
    timeout: Duration,
    begin: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
) -> Result<usize, String> {
    if timeout < Duration::from_millis(1) || timeout > Duration::from_millis(2500) {
        return Err("Native request timeout outside1..2500ms".into());
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE || cancel.cancelled() {
        return Err("Invalid/cancelled native request".into());
    }
    let raw = unsafe { CreateEventW(null(), 1, 0, null()) };
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(format!(
            "Create native request event: {}",
            std::io::Error::last_os_error()
        ));
    }
    let event = Event(raw);
    let mut op = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    let start = Instant::now();
    let begun = begin(&mut op);
    if begun == 0 {
        if unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(format!(
                "Start native request: {}",
                std::io::Error::last_os_error()
            ));
        }
        loop {
            // Reset before querying completion: a completion between these two
            // calls is observed by the query, and a later one signals the event.
            // An event signal alone does not release OVERLAPPED or its buffers.
            unsafe { ResetEvent(event.0) };
            if let Some(count) = unsafe { completion(handle, &op) }? {
                return Ok(count);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if cancel.cancelled() || remaining.is_zero() {
                unsafe { drain(handle, &op) };
                return Err("Native request cancelled/timed out; request drained".into());
            }
            let waited =
                unsafe { WaitForSingleObject(event.0, remaining.as_millis().clamp(1, 25) as u32) };
            if waited != WAIT_OBJECT_0 && waited != WAIT_TIMEOUT {
                unsafe { drain(handle, &op) };
                return Err("Native request wait failed; request drained".into());
            }
        }
    }
    // A conforming starter returns TRUE only for synchronous completion. Still
    // retain the borrowed storage if it unexpectedly reports a pending result.
    match unsafe { completion(handle, &op) }? {
        Some(count) => Ok(count),
        None => {
            unsafe { drain(handle, &op) };
            Err("Native starter reported success before completion; request drained".into())
        }
    }
}

unsafe fn completion(handle: HANDLE, op: &OVERLAPPED) -> Result<Option<usize>, String> {
    let mut count = 0;
    if unsafe { GetOverlappedResult(handle, op, &mut count, 0) } != 0 {
        return Ok(Some(count as usize));
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_IO_INCOMPLETE || error == ERROR_IO_PENDING {
        Ok(None)
    } else {
        Err(format!(
            "Complete native request: {}",
            std::io::Error::from_raw_os_error(error as i32)
        ))
    }
}

// Cancellation is a request, not a completion receipt. Even a signalled event
// can precede completion; keep all borrowed storage until the OS result is final.
// This establishes only this request's lifetime, not device-wide quiescence.
unsafe fn drain(handle: HANDLE, op: &OVERLAPPED) {
    unsafe { CancelIoEx(handle, op) };
    loop {
        unsafe { ResetEvent(op.hEvent) };
        if !matches!(unsafe { completion(handle, op) }, Ok(None)) {
            return;
        }
        if unsafe { WaitForSingleObject(op.hEvent, 25) } == WAIT_FAILED {
            unsafe { Sleep(1) };
        }
    }
}
