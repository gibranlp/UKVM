//! Display-only pinned TLS channel with strict allocation and IO bounds.
use crate::identity::{certificate_id, client_config, load_peer_certificate, Identity};
use bincode::Options;
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, TlsStream};
use ukvm_protocol::display::{Message, MAX_FRAME};
pub const ALPN: &[u8] = b"ukvm-display/2";
/// Keep video socket backlog small. CS1 is a best-effort lower-priority
/// marking; actual wire priority depends on the host/router queue discipline.
pub(crate) fn configure_socket(tcp: &TcpStream) {
    let socket = socket2::SockRef::from(tcp);
    let _ = socket.set_send_buffer_size(128 * 1024);
    let _ = socket.set_tos_v4(0x20);
    #[cfg(target_os = "linux")]
    {
        let _ = socket.set_tclass_v6(0x20);
        let _ = socket.set_tcp_notsent_lowat(32 * 1024);
    }
}
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME as u64)
        .reject_trailing_bytes()
}
pub fn decode(bytes: &[u8]) -> Result<Message, String> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("invalid display frame size".into());
    }
    let m: Message = codec()
        .deserialize(bytes)
        .map_err(|_| "malformed display message")?;
    m.validate()?;
    Ok(m)
}
pub async fn read<S: AsyncRead + Unpin>(s: &mut S) -> Result<Message, String> {
    read_until(s, Duration::from_secs(5)).await
}
pub async fn read_until<S: AsyncRead + Unpin>(
    s: &mut S,
    timeout: Duration,
) -> Result<Message, String> {
    tokio::time::timeout(timeout, async {
        let n = s.read_u32().await.map_err(|_| "display channel closed")? as usize;
        if n == 0 || n > MAX_FRAME {
            return Err("invalid display frame size".into());
        }
        let mut bytes = vec![0; n];
        s.read_exact(&mut bytes)
            .await
            .map_err(|_| "display channel interrupted")?;
        tokio::task::spawn_blocking(move || decode(&bytes))
            .await
            .map_err(|_| "display decoding worker failed")?
    })
    .await
    .map_err(|_| "display read timed out")?
}
pub async fn write<S: AsyncWrite + Unpin>(s: &mut S, m: &Message) -> Result<(), String> {
    m.validate()?;
    let message = m.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        codec()
            .serialize(&message)
            .map_err(|_| "display encoding failed")
    })
    .await
    .map_err(|_| "display encoding worker failed")??;
    if bytes.len() > MAX_FRAME {
        return Err("display frame too large".into());
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        s.write_u32(bytes.len() as u32).await?;
        s.write_all(&bytes).await?;
        s.flush().await
    })
    .await
    .map_err(|_| "display write timed out")?
    .map_err(|_| "display channel closed".into())
}
pub async fn connect(
    address: &str,
    identity: &Identity,
    certificate: &Path,
    expected: &str,
) -> Result<TlsStream<TcpStream>, String> {
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut config = (*client_config(identity, load_peer_certificate(certificate)?)?).clone();
        config.alpn_protocols = vec![ALPN.to_vec()];
        let tcp = TcpStream::connect(address)
            .await
            .map_err(|e| e.to_string())?;
        configure_socket(&tcp);
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        let name =
            rustls::pki_types::ServerName::try_from("ukvm-node").map_err(|e| e.to_string())?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|_| "display TLS authentication failed")?;
        if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
            return Err("peer does not support display v2".into());
        }
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or("missing display certificate")?;
        if certificate_id(cert.as_ref()) != expected {
            return Err("display peer identity mismatch".into());
        }
        Ok(TlsStream::from(tls))
    })
    .await
    .map_err(|_| "display connect timed out")?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn performance_metadata_roundtrips_without_pixel_logging() {
        let (mut sender, mut receiver) = tokio::io::duplex(2048);
        let frame = ukvm_protocol::display::Frame {
            sequence: 9,
            telemetry: ukvm_protocol::display::Telemetry {
                sent_us: 1000,
                clock: Some(ukvm_protocol::display::ClockEstimate {
                    offset_us: 50,
                    uncertainty_us: 25,
                }),
                ..Default::default()
            },
            width: 1366,
            height: 768,
            screen: None,
            origin_known: false,
            mapping_id: None,
            jpeg: Arc::new(vec![1, 2, 3]),
        };
        assert!(!format!("{frame:?}").contains("1, 2, 3"));
        let message = Message::Frame(frame);
        write(&mut sender, &message).await.unwrap();
        assert_eq!(read(&mut receiver).await.unwrap(), message);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn display_socket_has_bounded_backlog_and_lower_priority_marking() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        configure_socket(&socket);
        let reference = socket2::SockRef::from(&socket);
        assert_eq!(reference.tos_v4().unwrap(), 0x20);
        assert_eq!(reference.tcp_notsent_lowat().unwrap(), 32 * 1024);
        assert!(reference.send_buffer_size().unwrap() <= 256 * 1024);
    }
    #[tokio::test]
    async fn rejects_length_before_allocation_and_malformed_data() {
        let (mut a, mut b) = tokio::io::duplex(8);
        a.write_u32(u32::MAX).await.unwrap();
        assert!(read(&mut b).await.is_err());
        assert!(decode(&[255; 16]).is_err());
        assert!(decode(&[]).is_err());
        let m = Message::Hello(ukvm_protocol::display::VERSION);
        let bytes = codec().serialize(&m).unwrap();
        assert_eq!(decode(&bytes).unwrap(), m);
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }
}
