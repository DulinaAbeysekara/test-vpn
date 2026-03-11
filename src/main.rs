use clap::Parser;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};
use etherparse::{NetSlice, SlicedPacket, TransportSlice};
use tun_rs::{DeviceBuilder, Layer, SyncDevice};

mod client;
mod server;

#[derive(Clone, Debug)]
pub struct TunParams {
    pub name: String,
    pub address_v4: String,
    pub prefix_v4: u8,
    pub mtu: Option<u16>,
}

pub(crate) fn create_tun(params: &TunParams) -> io::Result<SyncDevice> {
    let mut builder = DeviceBuilder::new()
        .name(params.name.as_str())
        .layer(Layer::L3)
        .ipv4(params.address_v4.as_str(), params.prefix_v4, None);

    if let Some(mtu) = params.mtu {
        builder = builder.mtu(mtu);
    }

    builder.build_sync().map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!(
                "Failed to create TUN device: {e}. {}",
                if cfg!(windows) {
                    "On Windows: run as Administrator and ensure the Wintun driver is installed (https://www.wintun.net/)."
                } else {
                    "On Linux: run as root or ensure CAP_NET_ADMIN and that the `tun` kernel module is loaded (modprobe tun)."
                }
            ),
        )
    })
}

pub(crate) fn ts_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(crate) fn format_packet(packet: &[u8]) -> String {
    let sliced = match SlicedPacket::from_ip(packet) {
        Ok(s) => s,
        Err(_) => return format!("? len={}", packet.len()),
    };

    let (src, dst, proto, ports) = match &sliced.net {
        Some(NetSlice::Ipv4(ip4)) => {
            let h = ip4.header();
            let proto = format!("{:?}", h.protocol());
            (h.source_addr().to_string(), h.destination_addr().to_string(), proto, format_transport(&sliced.transport))
        }
        Some(NetSlice::Ipv6(ip6)) => {
            let h = ip6.header();
            let proto = format!("{:?}", h.next_header());
            (h.source_addr().to_string(), h.destination_addr().to_string(), proto, format_transport(&sliced.transport))
        }
        Some(NetSlice::Arp(_)) => return "ARP".to_string(),
        None => return format!("? len={}", packet.len()),
    };

    if ports.is_empty() {
        format!("{} -> {} {} len={}", src, dst, proto, packet.len())
    } else {
        format!("{} -> {} {} {} len={}", src, dst, proto, ports, packet.len())
    }
}

fn format_transport(transport: &Option<TransportSlice>) -> String {
    match transport {
        Some(TransportSlice::Udp(u)) => format!("{}:{}", u.source_port(), u.destination_port()),
        Some(TransportSlice::Tcp(t)) => format!("{}:{}", t.source_port(), t.destination_port()),
        Some(TransportSlice::Icmpv4(_)) => "icmp".to_string(),
        Some(TransportSlice::Icmpv6(_)) => "icmp6".to_string(),
        _ => String::new(),
    }
}

#[derive(Parser, Debug)]
#[command(name = "ntz-proto", about = "UDP tunnel over a cross-platform TUN")]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(clap::Subcommand, Debug)]
enum Mode {
    /// Client: reads from local TUN, sends packets to UDP server, writes replies back to TUN
    Client {
        /// UDP server address, e.g. 203.0.113.10:9000
        #[arg(long)]
        server: std::net::SocketAddr,

        /// TUN interface name
        #[arg(long, default_value = "ntz0")]
        name: String,

        /// IPv4 address to assign to the interface
        #[arg(long, default_value = "10.0.0.2")]
        address: String,

        /// IPv4 prefix length (CIDR), e.g. 24 for /24
        #[arg(long, default_value_t = 24)]
        prefix: u8,

        /// MTU to set (optional)
        #[arg(long)]
        mtu: Option<u16>,

        /// Extra logs
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },

    /// Server (Linux-only): decapsulates UDP into server TUN; Linux routes/NATs to Internet; replies go back over UDP
    Server {
        /// UDP listen address, e.g. 0.0.0.0:9000
        #[arg(long, default_value = "0.0.0.0:9000")]
        listen: std::net::SocketAddr,

        /// TUN interface name
        #[arg(long, default_value = "ntz0")]
        name: String,

        /// IPv4 address for server TUN
        #[arg(long, default_value = "10.0.0.1")]
        address: String,

        /// IPv4 prefix length (CIDR), e.g. 24 for /24
        #[arg(long, default_value_t = 24)]
        prefix: u8,

        /// MTU to set (optional)
        #[arg(long)]
        mtu: Option<u16>,

        /// WAN interface name for NAT setup (Linux), e.g. eth0
        #[arg(long)]
        nat_iface: Option<String>,

        /// If set, will run sysctl + iptables to enable NAT for the TUN subnet
        #[arg(long, default_value_t = false)]
        setup_nat: bool,

        /// Extra logs
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    match args.mode {
        Mode::Client {
            server,
            name,
            address,
            prefix,
            mtu,
            verbose,
        } => client::run(client::ClientArgs {
            server,
            tun: TunParams {
                name,
                address_v4: address,
                prefix_v4: prefix,
                mtu,
            },
            verbose,
        }),
        Mode::Server {
            listen,
            name,
            address,
            prefix,
            mtu,
            nat_iface,
            setup_nat,
            verbose: _,
        } => server::run(server::ServerArgs {
            listen,
            tun: TunParams {
                name,
                address_v4: address,
                prefix_v4: prefix,
                mtu,
            },
            nat_iface,
            setup_nat,
        }),
    }
}

