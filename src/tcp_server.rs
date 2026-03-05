use std::io::{Read, Write};
use std::net::TcpListener;

pub fn server_main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:7856")?;
    println!("Server listening on 7856.......");

    for stream in listener.incoming() {
        let mut stream = stream?;
        println!("new connection: {:?}", stream.peer_addr()?);

        let mut buffer = [0; 512];
        let byte_read = stream.read(&mut buffer)?;
        println!("Recevied:{}", String::from_utf8_lossy(&buffer[..byte_read]));

        stream.write_all(b"hello from server")?;
    }

    Ok(())
}
