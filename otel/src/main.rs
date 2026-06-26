mod exporter;
mod semconv;
mod span_builder;
mod tail;

use std::{path::PathBuf, sync::Arc};
use span_builder::SpanBuilder;

const DEFAULT_POLL_MS: u64 = 500;
const DEFAULT_IDLE_SECS: u64 = 30;
const DEFAULT_STATS_SECS: u64 = 60;
const DEFAULT_BATCH_DELAY_MS: u64 = 5000;

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
         OTEL_TAIL_FROM_BEGINNING     Set 'true' or '1' to replay entire file (default: false)\n\
         OTEL_POLL_INTERVAL_MS        File poll interval in ms (default: 500)\n\
         OTEL_IDLE_TIMEOUT_SECS       Watchdog: close open spans after N idle secs (default: 30)\n\
         OTEL_REDACT_PREVIEWS         Set 'false' or '0' to include *.preview span attrs (default: true)\n\
         OTEL_SESSION_ID              Optional session label added to all spans\n\
         OTEL_EXPORT_PROTOCOL         'http/protobuf' or 'grpc' (default: http/protobuf)\n\
         OTEL_EXPORT_BATCH_DELAY_MS   Batch flush interval in ms (default: 5000)\n"
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

fn env_u64_min(key: &str, default: u64, min: u64) -> u64 {
    env_u64(key, default).max(min)
}

/// FLIGHT_LOG_PATH: absolute, .jsonl, not world-writable.
fn validate_log_path(path: &std::path::Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.is_absolute(),
        "FLIGHT_LOG_PATH must be absolute: {path:?} (e.g. /var/log/agentos/flight.jsonl)"
    );
    anyhow::ensure!(
        path.extension().and_then(|e| e.to_str()) == Some("jsonl"),
        "FLIGHT_LOG_PATH must end in .jsonl: {path:?}"
    );
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        anyhow::ensure!(
            (mode & 0o002) == 0,
            "FLIGHT_LOG_PATH {path:?} is world-writable (injection risk) \
             (fix: chmod o-w {path:?})"
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
        !ep.contains('@') && !ep.to_ascii_lowercase().contains("%40"),
        "OTEL_EXPORTER_OTLP_ENDPOINT must not contain embedded credentials \
         (use OTEL_EXPORTER_OTLP_HEADERS for auth instead)"
    );
    Ok(())
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
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
    // Default true: tool output previews can contain secrets (e.g. read_file on a .env).
    // Set OTEL_REDACT_PREVIEWS=false to opt in to exporting previews verbatim.
    let redact_previews = std::env::var("OTEL_REDACT_PREVIEWS")
        .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
        .unwrap_or(true);
    let session_id = std::env::var("OTEL_SESSION_ID").ok();
    let use_grpc = std::env::var("OTEL_EXPORT_PROTOCOL")
        .map(|v| v.eq_ignore_ascii_case("grpc"))
        .unwrap_or(false);
    let batch_delay_ms = env_u64_min("OTEL_EXPORT_BATCH_DELAY_MS", DEFAULT_BATCH_DELAY_MS, 100);

    eprintln!(
        "agentos-otel: tailing {log_path:?} → {endpoint} \
         (service={service_name}, from_beginning={from_beginning}, \
          poll_ms={poll_ms}, idle_secs={idle_secs}, redact_previews={redact_previews}, \
          protocol={}, batch_delay_ms={batch_delay_ms})",
        if use_grpc { "grpc" } else { "http/protobuf" }
    );

    let provider = exporter::build_provider(&endpoint, &service_name, use_grpc, batch_delay_ms)?;
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
    let mut flushed_on_rotation: u64 = 0;
    // export_drops counts flush-attempt failures, not spans; one error may represent many lost spans.
    let mut export_drops: u64 = 0;
    let mut last_stats_print = std::time::Instant::now();
    let mut last_reported_drops: u64 = 0;

    let poll_interval = tokio::time::Duration::from_millis(poll_ms);
    let idle_timeout = tokio::time::Duration::from_secs(idle_secs);
    let stats_interval = tokio::time::Duration::from_secs(DEFAULT_STATS_SECS);

    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate()
    )?;
    let mut sigint = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::interrupt()
    )?;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = sigterm.recv() => {
                let ts = now_ns();
                let mut n = 0;
                for span in sb.drain_all(ts, "shutdown") {
                    n += 1;
                    exported_count += 1;
                    exporter::try_send(&span_tx, span);
                }
                eprintln!("agentos-otel: shutdown — flushed {n} open spans");
                let p = provider.clone();
                let flush_results = tokio::task::spawn_blocking(move || p.force_flush())
                    .await
                    .unwrap_or_else(|e| { eprintln!("agentos-otel: force_flush panic on shutdown: {e}"); vec![] });
                let mut shutdown_drops: u64 = 0;
                for r in flush_results {
                    if let Err(e) = r {
                        export_drops += 1;
                        shutdown_drops += 1;
                        eprintln!("agentos-otel: export error on shutdown: {e}");
                    }
                }
                if shutdown_drops > 0 {
                    token_counter.record_export_drops(shutdown_drops);
                }
                let dropped = exporter::spans_dropped();
                eprintln!(
                    "agentos-otel: exported={exported_count} open={} dropped={dropped} export_drops={export_drops} flushed_on_rotation={flushed_on_rotation}",
                    sb.open_span_count()
                );
                break;
            }
            _ = sigint.recv() => {
                let ts = now_ns();
                let mut n = 0;
                for span in sb.drain_all(ts, "shutdown") {
                    n += 1;
                    exported_count += 1;
                    exporter::try_send(&span_tx, span);
                }
                eprintln!("agentos-otel: interrupt — flushed {n} open spans");
                let p = provider.clone();
                let flush_results = tokio::task::spawn_blocking(move || p.force_flush())
                    .await
                    .unwrap_or_else(|e| { eprintln!("agentos-otel: force_flush panic on interrupt: {e}"); vec![] });
                let mut interrupt_drops: u64 = 0;
                for r in flush_results {
                    if let Err(e) = r {
                        export_drops += 1;
                        interrupt_drops += 1;
                        eprintln!("agentos-otel: export error on interrupt: {e}");
                    }
                }
                if interrupt_drops > 0 {
                    token_counter.record_export_drops(interrupt_drops);
                }
                let dropped = exporter::spans_dropped();
                eprintln!(
                    "agentos-otel: exported={exported_count} open={} dropped={dropped} export_drops={export_drops} flushed_on_rotation={flushed_on_rotation}",
                    sb.open_span_count()
                );
                break;
            }
        }

        let (lines, rotated) = tailer.poll().await?;

        if rotated {
            let ts = now_ns();
            let rotation_spans = sb.reset_for_rotation(ts);
            let n = rotation_spans.len();
            eprintln!(
                "agentos-otel: log rotation detected — flushed {n} stale spans (marked forced_close)"
            );
            for mut span in rotation_spans {
                span.attrs.push((
                    "forced_close".to_owned(),
                    span_builder::SpanAttr::Str("log_rotated".to_owned()),
                ));
                if let Some(sid) = &session_id {
                    span.attrs.push((
                        semconv::AGENTOS_SESSION_ID.to_owned(),
                        span_builder::SpanAttr::Str(sid.clone()),
                    ));
                }
                flushed_on_rotation += 1;
                exporter::try_send(&span_tx, span);
            }
        }

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
            let ts = now_ns();
            for span in sb.drain_all(ts, "watchdog_timeout") {
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
            // export_drops counts flush-attempt failures, not spans; one error may represent many lost spans.
            let p = provider.clone();
            let flush_results = tokio::task::spawn_blocking(move || p.force_flush())
                .await
                .unwrap_or_else(|e| { eprintln!("agentos-otel: force_flush panic: {e}"); vec![] });
            let mut new_export_drops: u64 = 0;
            for r in flush_results {
                if let Err(e) = r {
                    new_export_drops += 1;
                    eprintln!("agentos-otel: export error: {e}");
                }
            }
            if new_export_drops > 0 {
                export_drops += new_export_drops;
                token_counter.record_export_drops(new_export_drops);
            }
            eprintln!(
                "agentos-otel: exported={exported_count} open={} dropped={dropped} export_drops={export_drops} flushed_on_rotation={flushed_on_rotation}",
                sb.open_span_count()
            );
            last_stats_print = std::time::Instant::now();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // --- validate_log_path ---

    #[test]
    fn validate_log_path_rejects_relative() {
        let p = Path::new("relative/path/flight.jsonl");
        assert!(validate_log_path(p).is_err(), "relative path must be rejected");
    }

    #[test]
    fn validate_log_path_rejects_non_jsonl() {
        let p = Path::new("/var/log/agentos/flight.log");
        assert!(validate_log_path(p).is_err(), "non-.jsonl extension must be rejected");
    }

    #[test]
    fn validate_log_path_accepts_valid_missing_file() {
        // A path that is absolute and ends in .jsonl but does not exist yet is OK.
        let p = Path::new("/var/log/agentos/flight.jsonl");
        assert!(validate_log_path(p).is_ok(), "absolute .jsonl path must be accepted");
    }

    #[cfg(unix)]
    #[test]
    fn validate_log_path_rejects_world_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("flight.jsonl");
        std::fs::File::create(&p).unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o666); // world-writable
        std::fs::set_permissions(&p, perms).unwrap();
        assert!(validate_log_path(&p).is_err(), "world-writable file must be rejected");
    }

    // --- validate_endpoint ---

    #[test]
    fn validate_endpoint_rejects_non_http() {
        assert!(validate_endpoint("ftp://example.com").is_err());
        assert!(validate_endpoint("grpc://localhost:4317").is_err());
        assert!(validate_endpoint("localhost:4318").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_embedded_credentials() {
        assert!(validate_endpoint("https://user:pass@otel.example.com/v1/traces").is_err());
        assert!(validate_endpoint("https://user:pass%40otel.example.com/v1/traces").is_err());
    }

    #[test]
    fn validate_endpoint_accepts_http() {
        assert!(validate_endpoint("http://localhost:4318").is_ok());
    }

    #[test]
    fn validate_endpoint_accepts_https() {
        assert!(validate_endpoint("https://otel.example.com/v1/traces").is_ok());
    }

    // --- OTEL_REDACT_PREVIEWS parsing ---
    // Verifies that the env var inversion logic (default true, opt-out via "false"/"0") is correct.

    fn parse_redact_previews(val: Option<&str>) -> bool {
        match val {
            Some(v) => !v.eq_ignore_ascii_case("false") && v != "0",
            None => true,
        }
    }

    #[test]
    fn redact_previews_defaults_true_when_unset() {
        assert!(parse_redact_previews(None), "unset must default to true (safe)");
    }

    #[test]
    fn redact_previews_false_for_explicit_false() {
        assert!(!parse_redact_previews(Some("false")));
        assert!(!parse_redact_previews(Some("FALSE")));
        assert!(!parse_redact_previews(Some("False")));
    }

    #[test]
    fn redact_previews_false_for_zero() {
        assert!(!parse_redact_previews(Some("0")));
    }

    #[test]
    fn redact_previews_true_for_any_other_value() {
        assert!(parse_redact_previews(Some("true")));
        assert!(parse_redact_previews(Some("1")));
        assert!(parse_redact_previews(Some("yes")));
        assert!(parse_redact_previews(Some(""))); // empty string is truthy under this logic
    }
}
