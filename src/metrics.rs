use anyhow::Result;
use metrics_exporter_prometheus::PrometheusBuilder;

pub fn init_metrics() -> Result<()> {
    let builder = PrometheusBuilder::new();
    builder.install()?;
    Ok(())
}
