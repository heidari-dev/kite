//! The tunnel code wants a TCP stream, so a transport that is not one is bridged to a socket pair
//! on the loopback address: one end given back, the other served by a task.

use std::io;
use std::net::Ipv4Addr;
use tokio::net::{TcpListener, TcpStream};

/// `(client, server)`: the end to hand out and the end for the transport's task. It listens on
/// 127.0.0.1 on a free port, connects, accepts, and refuses the accepted connection unless its peer
/// is the socket it just connected, then closes the listener, so nobody else can join.
pub(crate) async fn loopback_pair() -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let (client, accepted) = tokio::try_join!(
        TcpStream::connect(listener.local_addr()?),
        listener.accept()
    )?;
    let (server, peer) = accepted;
    if peer != client.local_addr()? {
        return Err(io::Error::other("another program connected to the bridge"));
    }
    Ok((client, server))
}
