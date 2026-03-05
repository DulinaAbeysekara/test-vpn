use std::io::{Read, Write};
use std::net::TcpStream;

pub fn client_main() -> std::io::Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:7878")?;
    println!("connect to server");

    stream.write_all(b"hello from me")?;

    let mut buffer = [0; 512];
    let bytes_read = stream.read(&mut buffer)?;
    println!(
        "server say {}",
        String::from_utf8_lossy(&buffer[..bytes_read])
    );

    Ok(())
}
