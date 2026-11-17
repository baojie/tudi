//! `tudi-indexd`: the per-user daemon that owns the path index (milestone M5).

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::info!("tudi-indexd is not implemented yet (milestone M5)");
    Ok(())
}
