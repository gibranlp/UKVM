//! Local observation for emergency rearming, without input capture/injection.
//! The portals/EI receiver provide no idle global cursor-position readback.
//! Hyprland documents its user-session IPC cursorpos query. No shell, dispatch,
//! pointer warp, virtual input device, or alternate input backend is used.
use crate::{PlatformError, Point};
use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

const RESPONSE_LIMIT: u64 = 128;
const QUERY_TIMEOUT: Duration = Duration::from_millis(500);

pub async fn local_pointer_position() -> Result<Point, PlatformError> {
    if !super::SessionInfo::detect().wayland_socket_reachable {
        return Err(PlatformError::Unsupported(
            "local pointer observation requires native Wayland",
        ));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(PlatformError::Unsupported(
        "XDG runtime directory unavailable",
    ))?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").ok_or(
        PlatformError::Unsupported("idle pointer position is currently available only on Hyprland"),
    )?;
    let instance = Path::new(&instance);
    let mut parts = instance.components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return Err(PlatformError::Backend(
            "invalid Hyprland instance signature".into(),
        ));
    }
    let socket = PathBuf::from(runtime)
        .join("hypr")
        .join(instance)
        .join(".socket.sock");
    query(&socket).await
}

async fn query(socket: &Path) -> Result<Point, PlatformError> {
    tokio::time::timeout(QUERY_TIMEOUT, async {
        // Open immediately before writing: Hyprland serves requests
        // synchronously. Each sample owns/closes its socket, even on timeout.
        let mut stream = UnixStream::connect(socket).await.map_err(|e| {
            PlatformError::Backend(format!("pointer position query unavailable: {e}"))
        })?;
        stream
            .write_all(b"/cursorpos")
            .await
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        let mut response = Vec::new();
        stream
            .take(RESPONSE_LIMIT + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        parse(&response)
    })
    .await
    .map_err(|_| PlatformError::Backend("pointer position query timed out".into()))?
}

fn parse(response: &[u8]) -> Result<Point, PlatformError> {
    let invalid = || PlatformError::Backend("invalid local pointer position response".into());
    if response.len() as u64 > RESPONSE_LIMIT {
        return Err(invalid());
    }
    let text = std::str::from_utf8(response).map_err(|_| invalid())?;
    let (x, y) = text.split_once(',').ok_or_else(invalid)?;
    let point = Point {
        x: x.trim().parse().map_err(|_| invalid())?,
        y: y.trim().parse().map_err(|_| invalid())?,
    };
    if !point.x.is_finite()
        || !point.y.is_finite()
        || point.x < f64::from(i32::MIN)
        || point.x > f64::from(i32::MAX)
        || point.y < f64::from(i32::MIN)
        || point.y > f64::from(i32::MAX)
    {
        return Err(invalid());
    }
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_logical_coordinates_and_malformed_samples() {
        assert_eq!(
            parse(b"-3840, 1079\n").unwrap(),
            Point {
                x: -3840.0,
                y: 1079.0
            }
        );
        for bytes in [
            b"NaN, 0".as_slice(),
            b"0, inf",
            b"denied",
            b"1, 2, 3",
            b"2147483648, 0",
            &[255],
            &[b'0'; 129],
        ] {
            assert!(parse(bytes).is_err());
        }
    }
    #[tokio::test]
    async fn query_is_fixed_read_only_and_socket_is_closed_after_sample() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pointer.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 10];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"/cursorpos");
            socket.write_all(b"-32, 906").await.unwrap();
            socket.shutdown().await.unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(query(&path), server)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap(), Point { x: -32.0, y: 906.0 });
    }

    #[tokio::test]
    async fn oversized_reply_is_rejected_without_retaining_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pointer.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 10];
            socket.read_exact(&mut request).await.unwrap();
            socket.write_all(&[b'0'; 129]).await.unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(query(&path), server)
        })
        .await
        .unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stalled_observer_times_out_and_closes_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pointer.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 10];
            socket.read_exact(&mut request).await.unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(query(&path), server)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    #[ignore = "requires a live native Hyprland session and access to its IPC socket"]
    async fn live_hyprland_pointer_observation() {
        let position = local_pointer_position().await.unwrap();
        assert!(position.x.is_finite() && position.y.is_finite());
    }
}
