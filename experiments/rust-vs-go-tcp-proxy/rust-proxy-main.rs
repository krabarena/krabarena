use tokio::net::{TcpListener, TcpStream};
use tokio::io;

#[tokio::main]
async fn main() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8001").await?;
    eprintln!("rust proxy listening on :8001 -> :9000");
    loop {
        let (client, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = proxy(client).await {
                eprintln!("proxy error: {e}");
            }
        });
    }
}

async fn proxy(mut client: TcpStream) -> io::Result<()> {
    let mut upstream = TcpStream::connect("127.0.0.1:9000").await?;
    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = upstream.split();
    tokio::select! {
        r = io::copy(&mut cr, &mut uw) => r.map(|_| ()),
        r = io::copy(&mut ur, &mut cw) => r.map(|_| ()),
    }
}
