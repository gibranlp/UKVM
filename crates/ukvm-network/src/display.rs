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
pub const ALPN: &[u8] = b"ukvm-display/1";
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
        decode(&bytes)
    })
    .await
    .map_err(|_| "display read timed out")?
}
pub async fn write<S: AsyncWrite + Unpin>(s: &mut S, m: &Message) -> Result<(), String> {
    m.validate()?;
    let bytes = codec()
        .serialize(m)
        .map_err(|_| "display encoding failed")?;
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
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        let name =
            rustls::pki_types::ServerName::try_from("ukvm-node").map_err(|e| e.to_string())?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|_| "display TLS authentication failed")?;
        if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
            return Err("peer does not support display v1".into());
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
    async fn rejects_length_before_allocation_and_malformed_data() {
        let (mut a, mut b) = tokio::io::duplex(8);
        a.write_u32(u32::MAX).await.unwrap();
        assert!(read(&mut b).await.is_err());
        assert!(decode(&[255; 16]).is_err());
        assert!(decode(&[]).is_err());
        let m = Message::Hello(1);
        let bytes = codec().serialize(&m).unwrap();
        assert_eq!(decode(&bytes).unwrap(), m);
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }
}
