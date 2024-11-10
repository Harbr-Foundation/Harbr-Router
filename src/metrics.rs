use metrics_exporter_prometheus::PrometheusBuilder;
use anyhow::Result;

pub fn init_metrics() -> Result<()> {
    let builder = PrometheusBuilder::new();
    builder.install()?;
    Ok(())
}