mod tcp_client;
mod tcp_server;

use std::io::Result;
use std::thread::{self, JoinHandle};

fn main() -> Result<()> {
    // Spawn server in a separate thread
    let server_handle: JoinHandle<()> = thread::spawn(|| match tcp_server::server_main() {
        Ok(_) => println!("server finished"),
        Err(e) => println!("server error: {}", e),
    });

    // Run client on main thread
    match tcp_client::client_main() {
        Ok(_) => println!("Client finished success"),
        Err(e) => eprintln!("Client error: {}", e),
    }

    // Wait for server thread to finish
    server_handle.join().expect("could not join server thread");

    println!("done all !");
    Ok(())
}
