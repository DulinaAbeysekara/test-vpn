//! Client-side tunnel implementation.
//!

use crate::TunParams;
use std::io;
use std::net::IpAddr;
use std::net::{SocketAddr, UdpSocket};
use std::process::Command;
use std::sync::Arc;
use std::thread;

pub struct ClientArgs {
    pub server: SocketAddr,
    pub tun: TunParams,
    pub verbose: bool,
}

/// Runs the client tunnel loops.

pub fn run(args: ClientArgs) -> io::Result<()> {
    let dev = Arc::new(crate::create_tun(&args.tun)?);

    let udp = UdpSocket::bind("0.0.0.0:0")?;
    udp.connect(args.server)?;
    udp.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

    println!("Client up");
    println!(
        "  TUN: {} {}/{}",
        args.tun.name, args.tun.address_v4, args.tun.prefix_v4
    );
    println!("  UDP: {} -> {}", udp.local_addr()?, args.server);

    setup_routes(&args.tun, args.server, args.verbose)?;

    let dev_tx = Arc::clone(&dev);
    let udp_tx = udp.try_clone()?;
    let tun_to_udp = thread::spawn(move || -> io::Result<()> {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = dev_tx.recv(&mut buf)?;
            println!(
                "{} CLIENT TUN->SRV {}",
                crate::ts_ms(),
                crate::format_packet(&buf[..n])
            );
            udp_tx.send(&buf[..n])?;
        }
    });

    let dev_rx = Arc::clone(&dev);
    let udp_rx = udp;
    let udp_to_tun = thread::spawn(move || -> io::Result<()> {
        let mut buf = vec![0u8; 65535];
        loop {
            match udp_rx.recv(&mut buf) {
                Ok(n) => {
                    println!(
                        "{} CLIENT SRV->TUN {}",
                        crate::ts_ms(),
                        crate::format_packet(&buf[..n])
                    );
                    dev_rx.send(&buf[..n])?;
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
    });

    // If either thread exits with an error, propagate it.
    tun_to_udp.join().unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "tun->udp thread panicked",
        ))
    })?;
    udp_to_tun.join().unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "udp->tun thread panicked",
        ))
    })?;

    Ok(())
}

fn setup_routes(tun: &TunParams, server: SocketAddr, verbose: bool) -> io::Result<()> {
    match server.ip() {
        IpAddr::V4(v4) => setup_routes_v4(tun, v4.to_string().as_str(), verbose),
        IpAddr::V6(_) => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn setup_routes_v4(tun: &TunParams, server_ip: &str, verbose: bool) -> io::Result<()> {
    // Bring the interface up (it may start down on some systems).
    run_cmd(
        "ip",
        &["link", "set", "dev", tun.name.as_str(), "up"],
        verbose,
    )?;

    // Pin a /32 route to the UDP server *before* installing split-default routes.
    //
    // Without this, once default routes point into the tunnel, the UDP transport to the server
    // can accidentally route into the tunnel itself (which deadlocks the tunnel).
    if let Some((dev, via)) = linux_route_get(server_ip, verbose)? {
        if let Some(via) = via {
            run_cmd(
                "ip",
                &[
                    "route",
                    "replace",
                    &format!("{}/32", server_ip),
                    "via",
                    via.as_str(),
                    "dev",
                    dev.as_str(),
                ],
                verbose,
            )?;
        } else {
            run_cmd(
                "ip",
                &[
                    "route",
                    "replace",
                    &format!("{}/32", server_ip),
                    "dev",
                    dev.as_str(),
                ],
                verbose,
            )?;
        }
    }

    // Split default routes via TUN.
    run_cmd(
        "ip",
        &["route", "replace", "0.0.0.0/1", "dev", tun.name.as_str()],
        verbose,
    )?;
    run_cmd(
        "ip",
        &["route", "replace", "128.0.0.0/1", "dev", tun.name.as_str()],
        verbose,
    )?;

    Ok(())
}

#[cfg(windows)]
fn setup_routes_v4(tun: &TunParams, _server_ip: &str, verbose: bool) -> io::Result<()> {
    let gateway =
        derive_gateway_v4(&tun.address_v4, tun.prefix_v4).unwrap_or_else(|| "10.0.0.1".to_string());

    let if_index = powershell_capture(
        &format!(
            "(Get-NetAdapter -Name '{}').ifIndex",
            tun.name.replace('\'', "''")
        ),
        verbose,
    )?
    .trim()
    .to_string();

    if if_index.is_empty() {
        return Ok(());
    }

    run_cmd(
        "route",
        &[
            "ADD",
            "0.0.0.0",
            "MASK",
            "128.0.0.0",
            &gateway,
            "IF",
            &if_index,
        ],
        verbose,
    )?;
    run_cmd(
        "route",
        &[
            "ADD",
            "128.0.0.0",
            "MASK",
            "128.0.0.0",
            &gateway,
            "IF",
            &if_index,
        ],
        verbose,
    )?;

    Ok(())
}

fn run_cmd(program: &str, args: &[&str], verbose: bool) -> io::Result<()> {
    if verbose {
        eprintln!("[client] exec: {} {}", program, args.join(" "));
    }
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Command failed: {} {}", program, args.join(" ")),
        ))
    }
}

#[cfg(target_os = "linux")]
fn linux_route_get(ip: &str, verbose: bool) -> io::Result<Option<(String, Option<String>)>> {
    if verbose {
        eprintln!("[client] exec: ip route get {}", ip);
    }
    // Parse `ip route get` output to discover the current outbound interface and gateway.
    let out = Command::new("ip").args(["route", "get", ip]).output()?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // Example: "1.1.1.1 via 192.168.1.1 dev eth0 src 192.168.1.10 uid 1000\n"
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let mut dev: Option<String> = None;
    let mut via: Option<String> = None;
    let mut i = 0usize;
    while i < tokens.len() {
        match tokens[i] {
            "dev" if i + 1 < tokens.len() => {
                dev = Some(tokens[i + 1].to_string());
                i += 2;
            }
            "via" if i + 1 < tokens.len() => {
                via = Some(tokens[i + 1].to_string());
                i += 2;
            }
            _ => i += 1,
        }
    }
    Ok(dev.map(|d| (d, via)))
}

#[cfg(windows)]
fn powershell_capture(command: &str, verbose: bool) -> io::Result<String> {
    if verbose {
        eprintln!("[client] exec: powershell -Command {}", command);
    }
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", command])
        .output()?;
    if !out.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(windows)]
fn derive_gateway_v4(addr: &str, prefix: u8) -> Option<String> {
    let ip: std::net::Ipv4Addr = addr.parse().ok()?;
    let mask: u32 = if prefix == 0 {
        0
    } else {
        (!0u32) << (32 - prefix)
    };
    let net = u32::from(ip) & mask;
    let gw = std::net::Ipv4Addr::from(net.wrapping_add(1));
    Some(gw.to_string())
}
