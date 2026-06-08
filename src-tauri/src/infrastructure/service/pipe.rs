//! Win32 named-pipe server and client carrying the IPC protocol. The server
//! (run by the LocalSystem service) creates instances secured by
//! [`PipeSecurity`]; the client (the non-elevated GUI) connects and exchanges
//! one framed request/response per [`PipeConnection`].

use std::io::{self, Read, Write};
use std::iter::once;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW};

use super::security::PipeSecurity;

// Pipe open/mode flags (defined locally to avoid feature churn).
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const PIPE_BUFFER: u32 = 64 * 1024;

// Client open flags.
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000; // SecurityIdentification << 16

// Relevant Win32 error codes.
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PIPE_BUSY: u32 = 231;
const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_PIPE_CONNECTED: u32 = 535;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// A connected pipe endpoint, readable and writable. Closes its handle on drop.
pub struct PipeConnection {
    handle: HANDLE,
}

// Owned handle, used by a single thread at a time; moving it is sound.
unsafe impl Send for PipeConnection {}

impl Read for PipeConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf.len().min(u32::MAX as usize) as u32;
        let mut read: u32 = 0;
        // SAFETY: writing into the valid, in-bounds start of `buf`.
        let ok = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr(),
                want,
                &mut read,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            // A closed peer reads as clean EOF, not an error.
            if unsafe { GetLastError() } == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            return Err(last_error());
        }
        Ok(read as usize)
    }
}

impl Write for PipeConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let want = buf.len().min(u32::MAX as usize) as u32;
        let mut written: u32 = 0;
        // SAFETY: reading from the valid, in-bounds start of `buf`.
        let ok = unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                want,
                &mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateNamedPipeW/CreateFileW, closed once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Hosts the named pipe, creating one instance per accepted connection.
pub struct PipeServer {
    security: PipeSecurity,
    name: Vec<u16>,
    first: bool,
}

// The owned descriptor + name are only used on the accepting thread.
unsafe impl Send for PipeServer {}

impl PipeServer {
    pub fn new(name: &str) -> Result<Self, String> {
        Ok(Self {
            security: PipeSecurity::new()?,
            name: wide(name),
            first: true,
        })
    }

    /// Create a new pipe instance and block until a client connects. The first
    /// instance is created with `FIRST_PIPE_INSTANCE` so a pre-existing pipe of
    /// the same name (a squatter) causes a clean failure rather than chaining on.
    pub fn accept(&mut self) -> io::Result<PipeConnection> {
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if self.first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }

        // SAFETY: name is null-terminated; security attrs live in `self`.
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                self.security.as_ptr(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        self.first = false;

        // SAFETY: valid pipe handle; blocking connect (null overlapped).
        let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
        if connected == 0 {
            let code = unsafe { GetLastError() };
            // A client that connected between create and ConnectNamedPipe is fine.
            if code != ERROR_PIPE_CONNECTED {
                unsafe { CloseHandle(handle) };
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
        Ok(PipeConnection { handle })
    }
}

/// Connect to a named pipe server, waiting briefly for an instance to be ready.
pub fn connect(name: &str) -> io::Result<PipeConnection> {
    let wname = wide(name);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // SAFETY: name is null-terminated; no security attrs needed for the open.
        let handle = unsafe {
            CreateFileW(
                wname.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                // Identify-only impersonation: a malicious server can't act as us.
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(PipeConnection { handle });
        }

        let code = unsafe { GetLastError() };
        if Instant::now() >= deadline {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        match code {
            // All instances busy — wait for one to free up, then retry.
            ERROR_PIPE_BUSY => {
                unsafe { WaitNamedPipeW(wname.as_ptr(), 200) };
            }
            // Server not up yet — brief backoff, then retry.
            ERROR_FILE_NOT_FOUND => thread::sleep(Duration::from_millis(50)),
            other => return Err(io::Error::from_raw_os_error(other as i32)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ipc::{Request, Response, PROTOCOL_VERSION};
    use crate::infrastructure::service::framing::{
        read_frame, write_frame, MAX_REQUEST, MAX_RESPONSE,
    };

    #[test]
    fn pipe_request_response_roundtrip() {
        // A unique name per process avoids colliding with a real installed service.
        let name = format!(r"\\.\pipe\att-test-{}", std::process::id());
        let server_name = name.clone();

        let server = thread::spawn(move || {
            let mut server = PipeServer::new(&server_name).expect("create server");
            let mut conn = server.accept().expect("accept");
            let req: Request = read_frame(&mut conn, MAX_REQUEST).expect("read request");
            assert!(matches!(req, Request::Ping));
            write_frame(
                &mut conn,
                &Response::Pong {
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .expect("write response");
        });

        let mut client = connect(&name).expect("connect");
        write_frame(&mut client, &Request::Ping).expect("write request");
        let resp: Response = read_frame(&mut client, MAX_RESPONSE).expect("read response");
        assert!(
            matches!(resp, Response::Pong { protocol_version } if protocol_version == PROTOCOL_VERSION)
        );

        server.join().expect("server thread");
    }
}
