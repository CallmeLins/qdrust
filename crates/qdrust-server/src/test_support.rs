//! Helpers shared by the tests of the outbound paths.
//!
//! Four paths reach the network — a template run, a plain task URL, a
//! notification channel, a library source — and each now has a test proving the
//! request either reaches this test's own socket or is refused before it leaves
//! the machine. Two markers make that provable rather than likely:
//!
//! - a body only this server writes, for the paths that read one;
//! - an unassigned status code and a request count, for the paths whose only
//!   observable is the status.
//!
//! Without them a `200` from a proxy or a captive portal would let a test pass
//! with the request never reaching the server it claims to talk to.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Body every response carries. Read directly, or matched through a template's
/// `extract_variables`.
pub(crate) const LOOPBACK_MARKER: &str = "from-the-loopback-test-server";

/// Status every response carries. 218 is unassigned, so nothing else on the
/// network answers with it.
pub(crate) const LOOPBACK_STATUS: u16 = 218;

/// A loopback HTTP server answering every request with the marker body and the
/// marker status.
///
/// One request per connection, which is all any caller sends, and no parsing:
/// the request is read and discarded, because what is being tested is whether
/// it arrived at all.
pub(crate) async fn serve_loopback() -> SocketAddr {
    serve_counting_loopback().await.0
}

/// The same server, plus a count of the requests that reached it.
///
/// For a path whose only result is a status code — the plain-URL task, a
/// channel delivery — the counter is what turns "it returned something" into
/// "it talked to this socket": a refusal has to leave the count at zero, and an
/// allowed request has to move it. Nothing else on the network can satisfy both
/// directions.
pub(crate) async fn serve_counting_loopback() -> (SocketAddr, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut scratch = [0_u8; 2048];
                let _ = socket.read(&mut scratch).await;
                let response = format!(
                    "HTTP/1.1 {LOOPBACK_STATUS} This Is The Test Server\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    LOOPBACK_MARKER.len(),
                    LOOPBACK_MARKER
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (address, served)
}
