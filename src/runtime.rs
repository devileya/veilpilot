//! A small Tokio runtime beside GPUI's executor. Network requests and child
//! processes run here; results travel back to the UI over channels, which
//! GPUI's foreground tasks can await.

use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn handle() -> tokio::runtime::Handle {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("veilpilot-io")
                .enable_all()
                .build()
                .expect("starting the I/O runtime")
        })
        .handle()
        .clone()
}

pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(future)
}

/// One shared client: connection pooling and TLS setup are paid once.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("VeilPilot/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("building the HTTP client")
    })
}
