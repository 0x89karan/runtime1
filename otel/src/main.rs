mod exporter;
mod semconv;
mod span_builder;
mod tail;

use std::{path::PathBuf, sync::Arc};
use span_builder::SpanBuilder;

const DEFAULT_POLL_MS: u64 = 500;
const DEFAULT_IDLE_SECS: u64 = 30;

fn usage_and_exit() -> ! {
    eprintln!(
        "agentos-otel — export AgentOS flight.jsonl to any OTLP backend\n\
         \n\
         Usage: agentos-otel <flight.jsonl>\n\
         \n\
         Environment variables:\n\
         \n\
         FLIGHT_LOG_PATH              Path to flight.jsonl (overrides positional arg)\n\
         OTEL_EXPORTER_OTLP_ENDPOINT  OTLP endpoint (default: http://localhost:4318)\n\
         OTEL_SERVICE_NAME            Service name reported to OTLP (default: agentos)\n\
         OTEL_TAIL_FROM_BEGINNING     Set 'true' to replay entire file (default: false)\n\
         OTEL_POLL_INTERVAL_MS        File poll interval in ms (default: 500)\n\
         OTEL_IDLE_TIMEOUT_SECS       Watchdog: close open spans after N idle secs (default: 30)\n\
         OTEL_REDACT_PREVIEWS         Set 'true' to strip *.preview span attrs (default: false)\n\
         OTEL_SESSION_ID              Optional session label added to all spans\n\
         OTEL_EXPORT_PROTOCOL         'http/protobuf' or 'grpc' (default: http/protobuf)\n"
    );
    std::process::exit(1);
}

fn env_bool(key: &str) -> bool {
    std::env::var(key).map(|v| v.eq_ignore_ascii_case("true") || v == "1").unwrap_or(false)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// FLIGHT_LOG_PATH: absolute, .jsonl, not world-writable.
fn validate_log_path(path: &std::path::Path) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_absolute(), "FLIGHT_LOG_PATH must be absolute: {path:?}");
    anyhow::ensure!(
        path.extension().and_then(|e| e.to_str()) == Some("jsonl"),
        "FLIGHT_LOG_PATH must end in .jsonl: {path:?}"
    );
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        anyhow::ensure!(
            (mode & 0o002) == 0,
            "FLIGHT_LOG_PATH {path:?} is world-writable (injection risk)"
        );
    }
    Ok(())
}

/// OTEL_EXPORTER_OTLP_ENDPOINT: only http:// or https://, no embedded credentials.
fn validate_endpoint(ep: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        ep.starts_with("http://") || ep.starts_with("https://"),
        "OTEL_EXPORTER_OTLP_ENDPOINT must start with http:// or https://, got: {ep}"
    );
    anyhow::ensure!(
        !ep.contains('@'),
        "OTEL_EXPORTER_OTLP_ENDPOINT must not contain embedded credentials"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let log_path_str = std::env::var("FLIGHT_LOG_PATH").ok().or_else(|| {
        std::env::args().nth(1)
    });
    let log_path = match log_path_str {
        Some(s) => PathBuf::from(s),
        None => usage_and_exit(),
    };
    validate_log_path(&log_path)?;

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4318".to_owned());
    validate_endpoint(&endpoint)?;

    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| "agentos".to_owned());
    let from_beginning = env_bool("OTEL_TAIL_FROM_BEGINNING");
    let poll_ms = env_u64("OTEL_POLL_INTERVAL_MS", DEFAULT_POLL_MS);
    let idle_secs = env_u64("OTEL_IDLE_TIMEOUT_SECS", DEFAULT_IDLE_SECS);
    let redact_previews = env_bool("OTEL_REDACT_PREVIEWS");
    let session_id = std::env::var("OTEL_SESSION_ID").ok();
    let use_grpc = std::env::var("OTEL_EXPORT_PROTOCOL")
        .map(|v| v.eq_ignore_ascii_case("grpc"))
        .unwrap_or(false);

    eprintln!(
        "agentos-otel: tailing {log_path:?} → {endpoint} \
         (service={service_name}, from_beginning={from_beginning}, \
          poll_ms={poll_ms}, idle_secs={idle_secs}, redact_previews={redact_previews}, \
          protocol={})",
        if use_grpc { "grpc" } else { "http/protobuf" }
    );

    let provider = exporter::build_provider(&endpoint, &service_name, use_grpc)?;
    let tracer = {
        use opentelemetry::trace::TracerProvider;
        provider.tracer("agentos-otel")
    };
    let span_tx = exporter::spawn_export_worker(Arc::new(tracer));

    let metrics_provider = exporter::build_metrics_provider(&endpoint, &service_name, use_grpc)?;
    let token_counter = exporter::TokenCounter::new(metrics_provider);

    let mut tailer = tail::FileTailer::open(log_path, from_beginning).await?;
    let mut sb = SpanBuilder::new(redact_previews);
    let mut last_line_ts = std::time::Instant::now();
    let mut exported_count: u64 = 0;
    let mut last_stats_print = std::time::Instant::now();
    let mut last_reported_drops: u64 = 0;

    let poll_interval = tokio::time::Duration::from_millis(poll_ms);
    let idle_timeout = tokio::time::Duration::from_secs(idle_secs);
    let stats_interval = tokio::time::Duration::from_secs(60);

    loop {
        tokio::time::sleep(poll_interval).await;

        let (lines, _rotated) = tailer.poll().await?;

        for line in &lines {
            last_line_ts = std::time::Instant::now();
            let mut spans = sb.process_line(line);

            if let Some(sid) = &session_id {
                for span in &mut spans {
                    span.attrs.push((
                        semconv::AGENTOS_SESSION_ID.to_owned(),
                        span_builder::SpanAttr::Str(sid.clone()),
                    ));
                }
            }

            for span in spans {
                // Extract token usage from closed inference spans and emit metrics.
                if span.name == "gen_ai.chat" {
                    let model = span.attrs.iter()
                        .find(|(k, _)| k == "gen_ai.response.model" || k == "gen_ai.request.model")
                        .and_then(|(_, v)| if let span_builder::SpanAttr::Str(s) = v { Some(s.as_str()) } else { None })
                        .unwrap_or("unknown");
                    let run_id = sb.run_id().unwrap_or("unknown");
                    if let Some((_, span_builder::SpanAttr::Int(it))) = span.attrs.iter()
                        .find(|(k, _)| k == "gen_ai.usage.input_tokens") {
                        token_counter.record(*it as u64, run_id, model, "input");
                    }
                    if let Some((_, span_builder::SpanAttr::Int(ot))) = span.attrs.iter()
                        .find(|(k, _)| k == "gen_ai.usage.output_tokens") {
                        token_counter.record(*ot as u64, run_id, model, "output");
                    }
                }
                exported_count += 1;
                exporter::try_send(&span_tx, span);
            }
        }

        // Watchdog: force-close stale open spans.
        if last_line_ts.elapsed() > idle_timeout && sb.open_span_count() > 0 {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            for span in sb.drain_all(now_ns, "watchdog_timeout") {
                exported_count += 1;
                exporter::try_send(&span_tx, span);
            }
        }

        // Periodic stats line to stderr + export drops delta as OTLP counter.
        if last_stats_print.elapsed() > stats_interval {
            let dropped = exporter::spans_dropped();
            let new_drops = dropped.saturating_sub(last_reported_drops);
            if new_drops > 0 {
                token_counter.record_drops(new_drops);
                last_reported_drops = dropped;
            }
            eprintln!(
                "agentos-otel: exported={exported_count} open={} dropped={dropped}",
                sb.open_span_count()
            );
            last_stats_print = std::time::Instant::now();
        }
    }
}
