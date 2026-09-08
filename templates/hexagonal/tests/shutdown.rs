//! A gRPC client never closes its HTTP/2 channel on its own, so `main`'s drain
//! is bounded by a deadline. Without it the process waits forever on SIGTERM and
//! Kubernetes SIGKILLs the pod at the end of its grace period.
#![cfg(unix)]

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::Child;
use std::time::{Duration, Instant};

/// Two ports nothing is listening on. Both listeners stay bound until both
/// ports are read — dropping the first before binding the second lets the OS
/// hand out the same port twice.
fn free_ports() -> (u16, u16) {
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    (
        a.local_addr().unwrap().port(),
        b.local_addr().unwrap().port(),
    )
}

/// Poll until the port accepts. `Child` has no kill-on-drop, so every failure
/// path returns an error and the one caller kills the child.
fn connect(child: &mut Child, port: u16) -> Result<TcpStream, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            return Ok(stream);
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("server exited early with {status}"));
        }
        if Instant::now() > deadline {
            return Err(format!("port {port} never accepted"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn drain(child: &mut Child, http_port: u16, grpc_port: u16) -> Result<(), String> {
    connect(child, http_port)?;
    // Held open for the whole test — dropping it would let the drain finish on
    // its own and the deadline would go untested.
    let mut grpc = connect(child, grpc_port)?;
    grpc.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .map_err(|e| e.to_string())?;

    let start = Instant::now();
    std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .map_err(|e| e.to_string())?;

    // SHUTDOWN_TIMEOUT (10s) plus slack for a debug build under a loaded CI box.
    let deadline = start + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return status
                .success()
                .then_some(())
                .ok_or_else(|| format!("exited with {status}"));
        }
        if Instant::now() > deadline {
            return Err(format!("still running {:?} after SIGTERM", start.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn sigterm_exits_with_a_grpc_connection_open() {
    let (http_port, grpc_port) = free_ports();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hexagonal-rs"))
        .env("PORT", http_port.to_string())
        .env("GRPC_PORT", grpc_port.to_string())
        .spawn()
        .unwrap();
    if let Err(e) = drain(&mut child, http_port, grpc_port) {
        let _ = child.kill();
        panic!("{e}");
    }
}
