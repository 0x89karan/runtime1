use std::sync::Arc;
use tokio::sync::mpsc;

use opentelemetry::{
    metrics::Counter,
    trace::{
        Span, SpanContext, SpanId, SpanKind, Status, TraceContextExt,
        TraceFlags, TraceId, Tracer,
    },
    Context, KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::SdkMeterProvider,
    trace::{BatchConfigBuilder, BatchSpanProcessor, Tracer as SdkTracer, TracerProvider as SdkTracerProvider},
};

use crate::span_builder::{FinishedSpan, SpanAttr};

pub const CHANNEL_CAP: usize = 10_000;

static SPANS_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn spans_dropped() -> u64 {
    SPANS_DROPPED.load(std::sync::atomic::Ordering::Relaxed)
}

fn kv(key: &str, val: &SpanAttr) -> KeyValue {
    match val {
        SpanAttr::Str(s) => KeyValue::new(key.to_owned(), s.clone()),
        SpanAttr::Int(i) => KeyValue::new(key.to_owned(), *i),
        SpanAttr::Float(f) => KeyValue::new(key.to_owned(), *f),
        SpanAttr::Bool(b) => KeyValue::new(key.to_owned(), *b),
    }
}

fn parse_trace_id(s: &str) -> TraceId {
    let s = s.replace('-', "");
    let padded = format!("{:0>32}", s);
    let mut bytes = [0u8; 16];
    for (i, chunk) in padded.as_bytes().chunks(2).enumerate().take(16) {
        if let Ok(b) = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16) {
            bytes[i] = b;
        }
    }
    TraceId::from_bytes(bytes)
}

fn parse_span_id(s: &str) -> SpanId {
    let padded = format!("{:0>16}", s);
    let mut bytes = [0u8; 8];
    for (i, chunk) in padded.as_bytes().chunks(2).enumerate().take(8) {
        if let Ok(b) = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16) {
            bytes[i] = b;
        }
    }
    SpanId::from_bytes(bytes)
}

fn ns_to_systime(ns: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ns)
}

pub fn build_provider(endpoint: &str, service_name: &str, use_grpc: bool, batch_delay_ms: u64) -> anyhow::Result<SdkTracerProvider> {
    let resource = opentelemetry_sdk::Resource::new(vec![
        KeyValue::new("service.name", service_name.to_owned()),
    ]);
    let config = opentelemetry_sdk::trace::Config::default().with_resource(resource);
    let batch_cfg = BatchConfigBuilder::default()
        .with_max_export_batch_size(512)
        .with_scheduled_delay(std::time::Duration::from_millis(batch_delay_ms))
        .with_max_export_timeout(std::time::Duration::from_secs(30))
        .build();

    // grpc-tonic and http-proto are both compiled in; select at runtime.
    if use_grpc {
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(endpoint)
            .build_span_exporter()
            .map_err(|e| anyhow::anyhow!("OTLP gRPC exporter init: {e}"))?;
        let batch = BatchSpanProcessor::builder(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_batch_config(batch_cfg)
            .build();
        return Ok(SdkTracerProvider::builder()
            .with_span_processor(batch)
            .with_config(config)
            .build());
    }

    let exporter = opentelemetry_otlp::new_exporter()
        .http()
        .with_endpoint(endpoint)
        .build_span_exporter()
        .map_err(|e| anyhow::anyhow!("OTLP HTTP exporter init: {e}"))?;
    let batch = BatchSpanProcessor::builder(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_batch_config(batch_cfg)
        .build();
    Ok(SdkTracerProvider::builder()
        .with_span_processor(batch)
        .with_config(config)
        .build())
}

/// Build an OTLP metrics provider (same endpoint as traces).
pub fn build_metrics_provider(endpoint: &str, service_name: &str, use_grpc: bool) -> anyhow::Result<SdkMeterProvider> {
    let resource = opentelemetry_sdk::Resource::new(vec![
        KeyValue::new("service.name", service_name.to_owned()),
    ]);

    if use_grpc {
        return opentelemetry_otlp::new_pipeline()
            .metrics(opentelemetry_sdk::runtime::Tokio)
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint),
            )
            .with_resource(resource)
            .build()
            .map_err(|e| anyhow::anyhow!("OTLP gRPC metrics provider init: {e}"));
    }

    opentelemetry_otlp::new_pipeline()
        .metrics(opentelemetry_sdk::runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .http()
                .with_endpoint(endpoint),
        )
        .with_resource(resource)
        .build()
        .map_err(|e| anyhow::anyhow!("OTLP HTTP metrics provider init: {e}"))
}

/// OTLP metrics: token usage + spans-dropped counters.
pub struct TokenCounter {
    token_counter: Counter<u64>,
    drops_counter: Counter<u64>,
    #[allow(dead_code)] // kept alive to prevent provider shutdown
    meter_provider: SdkMeterProvider,
}

impl TokenCounter {
    pub fn new(meter_provider: SdkMeterProvider) -> Self {
        use opentelemetry::metrics::MeterProvider;
        let meter = meter_provider.meter("agentos-otel");
        let token_counter = meter
            .u64_counter(crate::semconv::METRIC_TOKEN_USAGE)
            .with_description("Cumulative token usage from AgentOS inference spans")
            .with_unit("tokens")
            .init();
        let drops_counter = meter
            .u64_counter(crate::semconv::METRIC_SPANS_DROPPED)
            .with_description("Spans dropped due to backpressure channel full")
            .with_unit("spans")
            .init();
        Self { token_counter, drops_counter, meter_provider }
    }

    pub fn record(&self, count: u64, session_id: &str, model: &str, token_type: &str) {
        self.token_counter.add(
            count,
            &[
                KeyValue::new(crate::semconv::GEN_AI_SYSTEM, crate::semconv::SYSTEM_ANTHROPIC),
                KeyValue::new(crate::semconv::GEN_AI_REQUEST_MODEL, model.to_owned()),
                KeyValue::new("session_id", session_id.to_owned()),
                KeyValue::new("token.type", token_type.to_owned()),
            ],
        );
    }

    pub fn record_drops(&self, count: u64) {
        self.drops_counter.add(count, &[]);
    }
}

/// Emit one `FinishedSpan` through the tracer into the OTLP pipeline.
pub fn emit_span(tracer: &SdkTracer, span: FinishedSpan) {
    let trace_id = parse_trace_id(&span.trace_id);
    let span_id = parse_span_id(&span.span_id);

    let parent_cx = match &span.parent_span_id {
        Some(pid) => {
            let parent_sid = parse_span_id(pid);
            let parent_sc = SpanContext::new(
                trace_id, parent_sid, TraceFlags::SAMPLED, true, Default::default(),
            );
            Context::new().with_remote_span_context(parent_sc)
        }
        None => Context::new(),
    };

    let attrs: Vec<KeyValue> = span.attrs.iter().map(|(k, v)| kv(k, v)).collect();
    let status = if span.status_error {
        Status::Error { description: "error".into() }
    } else {
        Status::Ok
    };
    let end_time = ns_to_systime(span.end_ts_ns);

    let builder = tracer
        .span_builder(span.name)
        .with_trace_id(trace_id)
        .with_span_id(span_id)
        .with_kind(SpanKind::Internal)
        .with_start_time(ns_to_systime(span.start_ts_ns))
        .with_end_time(end_time)
        .with_attributes(attrs)
        .with_status(status);

    let mut otel_span = tracer.build_with_context(builder, &parent_cx);
    otel_span.end_with_timestamp(end_time);
}

pub fn try_send(tx: &mpsc::Sender<FinishedSpan>, span: FinishedSpan) {
    if tx.try_send(span).is_err() {
        SPANS_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub fn spawn_export_worker(tracer: Arc<SdkTracer>) -> mpsc::Sender<FinishedSpan> {
    let (tx, mut rx) = mpsc::channel::<FinishedSpan>(CHANNEL_CAP);
    tokio::spawn(async move {
        while let Some(span) = rx.recv().await {
            emit_span(&tracer, span);
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trace_id() {
        let tid = parse_trace_id("aaaaaaaa000000000000000000000001");
        assert_ne!(tid.to_bytes(), [0u8; 16]);
    }

    #[test]
    fn test_parse_span_id() {
        let sid = parse_span_id("0000000000000001");
        assert_eq!(sid.to_bytes()[7], 1);
    }

    #[test]
    fn test_parse_trace_id_with_hyphens() {
        let tid = parse_trace_id("12345678-1234-1234-1234-123456789abc");
        assert_ne!(tid.to_bytes(), [0u8; 16]);
    }
}
