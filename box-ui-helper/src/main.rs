mod handler;
#[cfg(unix)]
mod unix;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("box-ui-helper starting");

    #[cfg(unix)]
    unix::run().await;

    #[cfg(windows)]
    {
        tracing::error!("Windows support not yet implemented");
        std::process::exit(1);
    }

    #[cfg(not(any(unix, windows)))]
    {
        tracing::error!("Unsupported platform");
        std::process::exit(1);
    }
}
