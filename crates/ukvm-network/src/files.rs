//! Independently negotiated, bounded file streams over pinned mutual TLS.
use crate::identity::{certificate_id, client_config, load_peer_certificate, Identity};
use bincode::Options;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, TlsStream};
use ukvm_core::files::{FileReceiver, FileSource};
use ukvm_protocol::files::{self, Kind, Message, Offer, CHUNK_BYTES, MAX_FRAME};
pub const ALPN: &[u8] = b"ukvm-files/1";
pub type Check = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
#[derive(Clone, Debug)]
pub struct Progress {
    pub state: &'static str,
    pub bytes: u64,
    pub destination: Option<String>,
}
pub type Observer = Arc<dyn Fn(Progress) -> Result<(), String> + Send + Sync>;
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME as u64)
        .reject_trailing_bytes()
}
pub fn encode(message: &Message) -> Result<Vec<u8>, String> {
    message.validate()?;
    let bytes = codec()
        .serialize(message)
        .map_err(|_| "file message encoding failed")?;
    if bytes.len() > MAX_FRAME {
        return Err("file message exceeds frame limit".into());
    }
    Ok(bytes)
}
pub fn decode(bytes: &[u8]) -> Result<Message, String> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("invalid file frame length".into());
    }
    let message: Message = codec()
        .deserialize(bytes)
        .map_err(|_| "malformed file message")?;
    message.validate()?;
    Ok(message)
}
pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Message, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let length = stream.read_u32().await.map_err(|_| "file channel closed")? as usize;
        if length == 0 || length > MAX_FRAME {
            return Err("invalid file frame length".into());
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "file channel interrupted")?;
        decode(&bytes)
    })
    .await
    .map_err(|_| "file channel read timed out")?
}
pub async fn write<S: AsyncWrite + Unpin>(stream: &mut S, message: &Message) -> Result<(), String> {
    let bytes = encode(message)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| "file channel write timed out")?
    .map_err(|_| "file channel closed".into())
}
pub async fn guarded<F: std::future::Future>(
    future: F,
    check: &Check,
) -> Result<F::Output, String> {
    check()?;
    tokio::pin!(future);
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {result=&mut future=>return Ok(result),_=ticks.tick()=>check()?}
    }
}
async fn io<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    message: &Message,
    check: &Check,
) -> Result<Message, String> {
    guarded(write(stream, message), check).await??;
    let response = guarded(read(stream), check).await??;
    if response == Message::Rejected {
        return Err("receiver rejected transfer; check receive-files permission, destination and transfer history".into());
    }
    Ok(response)
}
async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    check: &Check,
) -> Result<(), String> {
    if io(stream, &Message::hello(), check).await? != Message::hello() {
        return Err("file capability unavailable or receive-files permission denied".into());
    }
    Ok(())
}
struct Reporter {
    observer: Observer,
    last: Instant,
}
impl Reporter {
    async fn report(
        &mut self,
        state: &'static str,
        bytes: u64,
        destination: Option<String>,
        force: bool,
    ) -> Result<(), String> {
        if !force && self.last.elapsed() < Duration::from_millis(500) {
            return Ok(());
        }
        let callback = Arc::clone(&self.observer);
        tokio::task::spawn_blocking(move || {
            callback(Progress {
                state,
                bytes,
                destination,
            })
        })
        .await
        .map_err(|_| "file progress worker failed")??;
        self.last = Instant::now();
        Ok(())
    }
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
            .map_err(|_| "file peer unavailable")?;
        tcp.set_nodelay(true)
            .map_err(|_| "file socket setup failed")?;
        let name =
            rustls::pki_types::ServerName::try_from("ukvm-node").map_err(|_| "invalid TLS name")?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|_| "file TLS authentication/capability failed")?;
        if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
            return Err("peer does not support File Protocol 1".into());
        }
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or("file peer certificate missing")?;
        if certificate_id(cert.as_ref()) != peer_id {
            return Err("file peer identity mismatch".into());
        }
        Ok(TlsStream::from(tls))
    })
    .await
    .map_err(|_| "file connection timed out")?
}
pub fn new_id() -> Result<String, String> {
    let mut bytes = [0; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| "cannot initialize transfer identifier")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    source: Box<dyn FileSource>,
    check: Check,
    observer: Observer,
) -> Result<String, String> {
    let offer = source.offer().clone();
    offer.validate()?;
    let source = Arc::new(Mutex::new(source));
    let mut report = Reporter {
        observer,
        last: Instant::now(),
    };
    negotiate(stream, &check).await?;
    if io(stream, &Message::Offer(offer.clone()), &check).await? != Message::Accepted {
        return Err("peer rejected file offer or receive-files permission".into());
    }
    let mut bytes = 0u64;
    report.report("transferring", 0, None, true).await?;
    for (index, entry) in offer.entries.iter().enumerate() {
        if entry.kind == Kind::Directory {
            continue;
        }
        let mut offset = 0u64;
        let mut hash = Sha256::new();
        loop {
            let src = Arc::clone(&source);
            let payload = guarded(
                tokio::task::spawn_blocking(move || {
                    src.lock()
                        .map_err(|_| "source lock failed")?
                        .read_chunk(index)
                }),
                &check,
            )
            .await?
            .map_err(|_| "source worker failed")??;
            if payload.len() > CHUNK_BYTES {
                return Err("source exceeded chunk bound".into());
            }
            if payload.is_empty() {
                break;
            }
            if offset.saturating_add(payload.len() as u64) > entry.size {
                return Err("source size changed while sending".into());
            }
            hash.update(&payload);
            let length = payload.len() as u64;
            let reply = io(
                stream,
                &Message::Chunk {
                    index: index as u32,
                    offset,
                    bytes: payload,
                },
                &check,
            )
            .await?;
            offset += length;
            bytes += length;
            if reply != (Message::Ack { bytes }) {
                return Err("invalid file progress acknowledgement".into());
            }
            report.report("transferring", bytes, None, false).await?;
            // El límite cooperativo deja margen para las conexiones de entrada y portapapeles.
            guarded(
                tokio::time::sleep(Duration::from_secs_f64(
                    length as f64 / files::BYTES_PER_SECOND as f64,
                )),
                &check,
            )
            .await?;
        }
        if offset != entry.size {
            return Err("source size changed while sending".into());
        }
        let reply = io(
            stream,
            &Message::FileEnd {
                index: index as u32,
                size: offset,
                sha256: hash.finalize().into(),
            },
            &check,
        )
        .await?;
        if reply != (Message::Ack { bytes }) {
            return Err("peer did not verify file integrity".into());
        }
    }
    report.report("verifying", bytes, None, true).await?;
    let Message::Result { destination } = io(stream, &Message::Complete, &check).await? else {
        return Err("peer did not finalize transfer".into());
    };
    report
        .report("completed", bytes, Some(destination.clone()), true)
        .await?;
    Ok(destination)
}
pub async fn receive<S, F>(
    stream: &mut S,
    check: Check,
    observer: Observer,
    prepare: F,
) -> Result<String, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(Offer) -> Result<Box<dyn FileReceiver>, String> + Send + 'static,
{
    let result = receive_session(stream, check, observer, prepare).await;
    if result.is_err() {
        let _ =
            tokio::time::timeout(Duration::from_secs(1), write(stream, &Message::Rejected)).await;
    }
    result
}
async fn receive_session<S, F>(
    stream: &mut S,
    check: Check,
    observer: Observer,
    prepare: F,
) -> Result<String, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(Offer) -> Result<Box<dyn FileReceiver>, String> + Send + 'static,
{
    negotiate(stream, &check).await?;
    let Message::Offer(offer) = guarded(read(stream), &check).await?? else {
        return Err("file transfer offer required".into());
    };
    offer.validate()?;
    let copied = offer.clone();
    let destination = tokio::task::spawn_blocking(move || prepare(copied))
        .await
        .map_err(|_| "destination worker failed")??;
    let destination = Arc::new(Mutex::new(destination));
    let result = receive_inner(stream, &offer, &destination, &check, observer).await;
    // La limpieza se espera fuera del hilo que atiende eventos de entrada.
    tokio::task::spawn_blocking(move || drop(destination))
        .await
        .map_err(|_| "transfer cleanup failed")?;
    result
}
async fn receive_inner<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    offer: &Offer,
    destination: &Arc<Mutex<Box<dyn FileReceiver>>>,
    check: &Check,
    observer: Observer,
) -> Result<String, String> {
    guarded(write(stream, &Message::Accepted), check).await??;
    let mut report = Reporter {
        observer,
        last: Instant::now(),
    };
    let mut bytes = 0u64;
    report.report("transferring", 0, None, true).await?;
    for (index, entry) in offer.entries.iter().enumerate() {
        if entry.kind == Kind::Directory {
            continue;
        }
        let mut offset = 0u64;
        let mut hash = Sha256::new();
        loop {
            match guarded(read(stream), check).await?? {
                Message::Chunk {
                    index: received,
                    offset: position,
                    bytes: payload,
                } if received as usize == index
                    && position == offset
                    && offset.saturating_add(payload.len() as u64) <= entry.size =>
                {
                    hash.update(&payload);
                    offset += payload.len() as u64;
                    bytes += payload.len() as u64;
                    let output = Arc::clone(destination);
                    let policy = Arc::clone(check);
                    tokio::task::spawn_blocking(move || {
                        policy()?;
                        output
                            .lock()
                            .map_err(|_| "destination lock failed")?
                            .write_chunk(index, &payload)
                    })
                    .await
                    .map_err(|_| "destination worker failed")??;
                    guarded(write(stream, &Message::Ack { bytes }), check).await??;
                    report.report("transferring", bytes, None, false).await?;
                }
                Message::FileEnd {
                    index: received,
                    size,
                    sha256,
                } if received as usize == index && size == offset && offset == entry.size => {
                    let expected: [u8; 32] = hash.finalize().into();
                    if expected != sha256 {
                        return Err("file integrity SHA-256 mismatch".into());
                    }
                    let output = Arc::clone(destination);
                    let policy = Arc::clone(check);
                    tokio::task::spawn_blocking(move || {
                        policy()?;
                        output
                            .lock()
                            .map_err(|_| "destination lock failed")?
                            .finish_file(index)
                    })
                    .await
                    .map_err(|_| "destination worker failed")??;
                    guarded(write(stream, &Message::Ack { bytes }), check).await??;
                    break;
                }
                Message::Cancel => return Err("transfer cancelled by sender".into()),
                _ => return Err("malformed chunk, offset, order or declared size mismatch".into()),
            }
        }
    }
    if bytes != offer.total_bytes || guarded(read(stream), check).await?? != Message::Complete {
        return Err("transfer completion mismatch".into());
    }
    report.report("verifying", bytes, None, true).await?;
    let output = Arc::clone(destination);
    let policy = Arc::clone(check);
    let name = tokio::task::spawn_blocking(move || {
        policy()?;
        output
            .lock()
            .map_err(|_| "destination lock failed")?
            .commit()
    })
    .await
    .map_err(|_| "destination worker failed")??;
    report
        .report("completed", bytes, Some(name.clone()), true)
        .await?;
    guarded(
        write(
            stream,
            &Message::Result {
                destination: name.clone(),
            },
        ),
        check,
    )
    .await??;
    Ok(name)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bad_frames() {
        for b in [vec![], vec![255; 8], vec![0; MAX_FRAME + 1]] {
            assert!(decode(&b).is_err());
        }
        let mut b = encode(&Message::hello()).unwrap();
        b.push(1);
        assert!(decode(&b).is_err());
    }
}
