use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    println!(
        "RustDrop core v{} (protocol {})",
        rustdrop_core::VERSION,
        rustdrop_core::PROTOCOL_VERSION
    );
}
