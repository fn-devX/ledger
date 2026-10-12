pub mod metrics;
pub mod propagation;

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub const OTLP_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

const QUIET: &str = "h2=warn,hyper=warn,hyper_util=warn,tower=warn,tonic=warn,opentelemetry=warn";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

impl LogFormat {
    pub fn from_env() -> Self {
        match std::env::var("LOG_FORMAT").as_deref() {
            Ok("json") => Self::Json,
            _ => Self::Pretty,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub service: &'static str,
    pub default_filter: &'static str,
    pub log_format: LogFormat,
    pub otlp_endpoint: Option<String>,
}

impl TelemetryConfig {
    pub fn from_env(service: &'static str, default_filter: &'static str) -> Self {
        Self {
            service,
            default_filter,
            log_format: LogFormat::from_env(),
            otlp_endpoint: std::env::var(OTLP_ENDPOINT_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }
}

#[derive(Debug)]
pub struct Telemetry {
    provider: SdkTracerProvider,
    exporting: Option<String>,
}

impl Telemetry {
    pub fn otlp_endpoint(&self) -> Option<&str> {
        self.exporting.as_deref()
    }

    pub fn shutdown(self) {
        if let Err(e) = self.provider.shutdown() {
            eprintln!("failed to flush traces: {e}");
        }
    }
}

pub fn tracer_provider(
    service: &str,
    otlp_endpoint: Option<&str>,
) -> Result<SdkTracerProvider, BoxError> {
    let name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| service.to_owned());
    let resource = Resource::builder()
        .with_service_name(name)
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let mut builder = SdkTracerProvider::builder().with_resource(resource);
    if let Some(endpoint) = otlp_endpoint {
        use opentelemetry_otlp::WithExportConfig;
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?;
        builder = builder.with_batch_exporter(exporter);
    }
    Ok(builder.build())
}

pub fn init(config: TelemetryConfig) -> Result<Telemetry, BoxError> {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let provider = tracer_provider(config.service, config.otlp_endpoint.as_deref())?;
    global::set_tracer_provider(provider.clone());
    let otel = tracing_opentelemetry::layer()
        .with_error_events_to_status(false)
        .with_tracer(provider.tracer(config.service));

    let base = std::env::var(EnvFilter::DEFAULT_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| config.default_filter.to_owned());
    let filter = EnvFilter::try_new(format!("{QUIET},{base}"))?;
    let registry = tracing_subscriber::registry().with(filter).with(otel);
    match config.log_format {
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()?,
        LogFormat::Pretty => registry.with(tracing_subscriber::fmt::layer()).try_init()?,
    }
    if let Some(endpoint) = &config.otlp_endpoint {
        tracing::info!(endpoint, "exporting traces over OTLP");
    } else {
        tracing::info!("{OTLP_ENDPOINT_ENV} is not set: traces are not exported");
    }
    Ok(Telemetry {
        provider,
        exporting: config.otlp_endpoint,
    })
}

pub fn install_local_subscriber() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(local_subscriber("local"));
    });
}

fn local_subscriber(service: &'static str) -> impl tracing::Subscriber + Send + Sync {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = SdkTracerProvider::builder().build().tracer(service);
    tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer))
}
