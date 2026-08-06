//! Temporary diagnostic: connect to a tc server, print its Welcome {version, protocol}.
//! Usage: cargo run -p tc-client --example probe_version -- 13.140.128.194:7100

use std::collections::HashMap;

use tc_client::tls::{tls_connector, TofuState};
use tc_shared::{try_decode_frame, ServerMessage};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "13.140.128.194:7100".to_string());

    let tofu = TofuState::new(HashMap::new());
    tofu.set_current_server(&addr);

    let stream = TcpStream::connect(&addr).await?;
    let connector = tls_connector(&tofu);
    let server_name = rustls_pki_types::ServerName::try_from("tc-server")
        .unwrap()
        .to_owned();
    let mut tls = connector.connect(server_name, stream).await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = tls.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("server closed before Welcome");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some((frame, _consumed)) = try_decode_frame(&buf)? {
            let msg: ServerMessage = bincode::deserialize(&frame)?;
            match msg {
                ServerMessage::Welcome { version, protocol } => {
                    println!("server version: {version}, protocol: {protocol}");
                }
                other => println!("unexpected first message: {other:?}"),
            }
            return Ok(());
        }
    }
}
