use crate::identity::{certificate_id, client_config, load_peer_certificate, Identity};
use bincode::Options;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, TlsStream};
use ukvm_protocol::clipboard::{ClipboardMessage, MAX_CLIPBOARD_BYTES};

pub const ALPN: &[u8] = b"ukvm-clipboard/1";
const MAX_FRAME: usize = MAX_CLIPBOARD_BYTES + 256;
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME as u64)
        .reject_trailing_bytes()
}
pub fn encode(message: &ClipboardMessage) -> Result<Vec<u8>, String> {
    message.validate()?;
    codec()
        .serialize(message)
        .map_err(|_| "clipboard encoding failed".into())
}
pub fn decode(payload: &[u8]) -> Result<ClipboardMessage, String> {
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err("invalid clipboard frame size".into());
    }
    let message: ClipboardMessage = codec()
        .deserialize(payload)
        .map_err(|_| "malformed clipboard frame".to_string())?;
    message.validate()?;
    Ok(message)
}
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<ClipboardMessage, String> {
    let length = reader
        .read_u32()
        .await
        .map_err(|_| "clipboard channel closed".to_string())? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("invalid clipboard frame size".into());
    }
    let mut bytes = vec![0; length];
    tokio::time::timeout(Duration::from_secs(4), reader.read_exact(&mut bytes))
        .await
        .map_err(|_| "clipboard read timed out".to_string())?
        .map_err(|_| "clipboard channel closed".to_string())?;
    decode(&bytes)
}
pub async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &ClipboardMessage,
) -> Result<(), String> {
    let bytes = encode(message)?;
    tokio::time::timeout(Duration::from_secs(4), async {
        writer.write_u32(bytes.len() as u32).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| "clipboard write timed out".to_string())?
    .map_err(|_| "clipboard channel closed".into())
}
pub async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(8), async {
        write(stream, &ClipboardMessage::hello()).await?;
        if read(stream).await? != ClipboardMessage::hello() {
            return Err("clipboard capability negotiation failed".into());
        }
        Ok(())
    })
    .await
    .map_err(|_| "clipboard negotiation timed out".to_string())?
}
pub async fn connect(
    address: &str,
    identity: &Identity,
    certificate: &Path,
    peer_id: &str,
) -> Result<TlsStream<TcpStream>, String> {
    let mut config = (*client_config(identity, load_peer_certificate(certificate)?)?).clone();
    config.alpn_protocols = vec![ALPN.to_vec()];
    tokio::time::timeout(Duration::from_secs(8), async {
        let tcp = TcpStream::connect(address)
            .await
            .map_err(|_| "clipboard peer unavailable".to_string())?;
        tcp.set_nodelay(true)
            .map_err(|_| "clipboard socket setup failed".to_string())?;
        let name = rustls::pki_types::ServerName::try_from("ukvm-node")
            .map_err(|_| "clipboard TLS name invalid".to_string())?;
        let tls = TlsConnector::from(std::sync::Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|_| "clipboard TLS authentication failed".to_string())?;
        if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
            return Err("peer does not support Clipboard Protocol 1".into());
        }
        let certificate = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or("clipboard peer certificate missing")?;
        if certificate_id(certificate.as_ref()) != peer_id {
            return Err("clipboard peer identity mismatch".into());
        }
        Ok(TlsStream::from(tls))
    })
    .await
    .map_err(|_| "clipboard connection timed out".to_string())?
}
pub fn epoch() -> Result<[u8; 16], String> {
    let mut value = [0; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut value)
        .map_err(|_| "clipboard generation initialization failed".to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_utf8_oversize_trailing_and_secrets() {
        for payload in [vec![], vec![0xff; 20], vec![0; MAX_FRAME + 1]] {
            assert!(decode(&payload).is_err());
        }
        let message = ClipboardMessage::Text {
            origin: "a".repeat(64),
            epoch: [1; 16],
            generation: 1,
            byte_length: 6,
            text: "secret".into(),
        };
        let mut bytes = encode(&message).unwrap();
        assert_eq!(decode(&bytes).unwrap(), message);
        bytes.push(1);
        assert!(decode(&bytes).is_err());
        bytes.pop();
        *bytes.last_mut().unwrap() = 0xff;
        let error = decode(&bytes).unwrap_err();
        assert!(!error.contains("secret"));
    }
    #[tokio::test]
    async fn negotiation_and_bounded_frame() {
        let (mut a, mut b) = tokio::io::duplex(MAX_FRAME + 4);
        let (a_result, b_result) = tokio::join!(negotiate(&mut a), negotiate(&mut b));
        assert!(a_result.is_ok() && b_result.is_ok());
        a.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read(&mut b).await.is_err());
    }
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod integration_tests;
