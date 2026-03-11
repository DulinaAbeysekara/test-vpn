use crate::TunParams;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

pub struct ServerArgs {
    pub listen: SocketAddr,
    pub tun: TunParams,
    pub nat_iface: Option<String>,
    pub setup_nat: bool,
}

#[cfg(not(target_os = "linux"))]
pub fn run(_args: ServerArgs) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Other,
        "Server mode is Linux-only.",
    ))
}

#[cfg(target_os = "linux")]
pub fn run(args: ServerArgs) -> io::Result<()> {
    let dev = Arc::new(crate::create_tun(&args.tun)?);

    if args.setup_nat {
        if let Some(iface) = args.nat_iface.as_deref() {
            setup_nat(
                &args.tun.name,
                &args.tun.address_v4,
                args.tun.prefix_v4,
                iface,
            )?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--setup-nat requires --nat-iface",
            ));
        }
    }

    let udp = UdpSocket::bind(args.listen)?;
    udp.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

    let client_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

    println!("Server up (Linux)");
    println!("  TUN: {} {}/{}", args.tun.name, args.tun.address_v4, args.tun.prefix_v4);
    println!("  UDP listen: {}", udp.local_addr()?);
    if args.setup_nat {
        println!("  NAT/forwarding: configured automatically (--setup-nat)");
    } else {
        println!("  NAT/forwarding: not configured (run with --setup-nat --nat-iface <WAN>)");
    }

    let dev_tx = Arc::clone(&dev);
    let udp_tx = udp.try_clone()?;
    let client_tx = Arc::clone(&client_addr);
    let tun_to_udp = thread::spawn(move || -> io::Result<()> {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = dev_tx.recv(&mut buf)?;
            let to = *client_tx.lock().unwrap();
            if let Some(addr) = to {
                println!(
                    "{} SERVER NET->SRV {}",
                    crate::ts_ms(),
                    crate::format_packet(&buf[..n])
                );
                println!(
                    "{} SERVER SRV->CLI {} to={}",
                    crate::ts_ms(),
                    crate::format_packet(&buf[..n]),
                    addr
                );
                udp_tx.send_to(&buf[..n], addr)?;
            }
        }
    });

    let dev_rx = Arc::clone(&dev);
    let client_rx = Arc::clone(&client_addr);
    let udp_to_tun = thread::spawn(move || -> io::Result<()> {
        let mut buf = vec![0u8; 65535];
        loop {
            match udp.recv_from(&mut buf) {
                Ok((n, from)) => {
                    *client_rx.lock().unwrap() = Some(from);
                    println!(
                        "{} SERVER CLI->SRV {} from={}",
                        crate::ts_ms(),
                        crate::format_packet(&buf[..n]),
                        from
                    );
                    println!(
                        "{} SERVER SRV->NET {}",
                        crate::ts_ms(),
                        crate::format_packet(&buf[..n])
                    );
                    dev_rx.send(&buf[..n])?;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
    });

    tun_to_udp
        .join()
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::Other, "tun->udp thread panicked")))?;
    udp_to_tun
        .join()
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::Other, "udp->tun thread panicked")))?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn setup_nat(tun_name: &str, tun_addr: &str, prefix: u8, wan_iface: &str) -> io::Result<()> {
    let cidr = format!("{}/{}", network_cidr(tun_addr, prefix)?, prefix);

    // Bring the interface up (tun-rs doesn't always do this on Linux).
    run_cmd("ip", &["link", "set", "dev", tun_name, "up"])?;

    // Ensure kernel has a route to the TUN subnet.
    run_cmd("ip", &["route", "replace", &cidr, "dev", tun_name])?;

    // Enable IPv4 forwarding.
    run_cmd("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;

    // Allow forwarding between TUN and WAN.
    ensure_iptables_rule(&[
        "-C",
        "FORWARD",
        "-i",
        tun_name,
        "-o",
        wan_iface,
        "-j",
        "ACCEPT",
    ])?;
    ensure_iptables_rule(&[
        "-C",
        "FORWARD",
        "-i",
        wan_iface,
        "-o",
        tun_name,
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ])?;

    // NAT traffic leaving via WAN.
    ensure_iptables_nat_rule(&[
        "-C",
        "POSTROUTING",
        "-s",
        &cidr,
        "-o",
        wan_iface,
        "-j",
        "MASQUERADE",
    ])?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn run_cmd(program: &str, args: &[&str]) -> io::Result<()> {
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
fn ensure_iptables_rule(check_args: &[&str]) -> io::Result<()> {
    // check_args is a full `iptables` invocation starting at "-C ...".
    // If check succeeds, rule exists; if it fails, add the same rule with "-A".
    let status = Command::new("iptables").args(check_args).status()?;
    if status.success() {
        return Ok(());
    }
    let mut add_args = check_args.to_vec();
    if let Some(first) = add_args.first_mut() {
        *first = "-A";
    }
    run_cmd("iptables", &add_args)
}

#[cfg(target_os = "linux")]
fn ensure_iptables_nat_rule(check_args: &[&str]) -> io::Result<()> {
    // check_args is a full `iptables -t nat` invocation starting at "-C POSTROUTING ...".
    let status = Command::new("iptables")
        .args(["-t", "nat"])
        .args(check_args)
        .status()?;
    if status.success() {
        return Ok(());
    }
    let mut add_args = check_args.to_vec();
    if let Some(first) = add_args.first_mut() {
        *first = "-A";
    }
    let status = Command::new("iptables")
        .args(["-t", "nat"])
        .args(add_args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "iptables NAT failed"))
    }
}

fn network_cidr(addr: &str, prefix: u8) -> io::Result<String> {
    let ip: Ipv4Addr = addr.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Invalid IPv4 address: {}", addr),
        )
    })?;
    let mask: u32 = if prefix == 0 { 0 } else { (!0u32) << (32 - prefix) };
    let ip_u32 = u32::from(ip);
    let net = Ipv4Addr::from(ip_u32 & mask);
    Ok(net.to_string())
}

