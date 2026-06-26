use std::collections::HashMap;
use serde_json::Value;

/// A completed span ready to export.
#[derive(Debug, Clone)]
pub struct FinishedSpan {
    pub trace_id: String,   // 32 hex chars (from run_id, hyphens stripped)
    pub span_id: String,    // 16 hex chars
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_ts_ns: u64,
    pub end_ts_ns: u64,
    pub attrs: Vec<(String, SpanAttr)>,
    #[allow(dead_code)]
    pub events: Vec<SpanEvent>,
    pub status_error: bool,
}

#[derive(Debug, Clone)]
pub enum SpanAttr {
    Str(String),
    Int(i64),
    #[allow(dead_code)]
    Float(f64),
    Bool(bool),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SpanEvent {
    pub name: String,
    pub ts_ns: u64,
    pub attrs: Vec<(String, SpanAttr)>,
}

/// Key for looking up an in-progress span.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SpanKey {
    Agent(String),               // agent span (keyed on agent_id)
    Inference(String, u64),      // inference span (agent_id, turn)
    Tool(String, u64, String),   // tool span (agent_id, turn, tool_name)
}

/// An in-progress span (not yet closed).
#[derive(Debug)]
struct OpenSpan {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    name: String,
    start_ts_ns: u64,
    attrs: Vec<(String, SpanAttr)>,
    events: Vec<SpanEvent>,
    synthesized: bool,
}

/// Reconstructs OTel spans from a stream of flight-recorder events.
pub struct SpanBuilder {
    trace_id: Option<String>,   // set on scheduler_started
    run_id: Option<String>,
    redact_previews: bool,

    // span_id for the run-level (scheduler) span
    run_span_id: Option<String>,
    // map agent_id -> span_id for agent spans
    agent_span_ids: HashMap<String, String>,

    open: HashMap<SpanKey, OpenSpan>,
    span_counter: u64,
}

impl SpanBuilder {
    pub fn new(redact_previews: bool) -> Self {
        Self {
            trace_id: None,
            run_id: None,
            redact_previews,
            run_span_id: None,
            agent_span_ids: HashMap::new(),
            open: HashMap::new(),
            span_counter: 0,
        }
    }

    fn next_span_id(&mut self) -> String {
        self.span_counter += 1;
        format!("{:016x}", self.span_counter)
    }

    /// Parse RFC3339 / ISO8601 timestamp string → nanoseconds since Unix epoch.
    fn parse_ts(ts: &str) -> u64 {
        // chrono would be ideal, but we avoid adding deps; parse manually.
        // chrono::Utc::now().to_rfc3339() emits "+00:00" suffix (not "Z").
        // Fallback: return 0 on parse error (best-effort, must not crash).
        let ts = ts.trim_end_matches('Z');
        // Strip RFC3339 timezone offset (+HH:MM).
        let ts = match ts.rfind('+') {
            Some(pos) => &ts[..pos],
            None => ts,
        };
        // Strip negative timezone offset (-HH:MM), only after the date portion.
        let ts = match ts.rfind('-') {
            Some(pos) if pos > 13 => &ts[..pos],
            _ => ts,
        };
        let parts: Vec<&str> = ts.splitn(2, 'T').collect();
        if parts.len() != 2 {
            return 0;
        }
        let date_parts: Vec<u32> = parts[0].splitn(3, '-')
            .filter_map(|s| s.parse().ok()).collect();
        if date_parts.len() != 3 {
            return 0;
        }
        let time_str = parts[1];
        let time_parts: Vec<&str> = time_str.splitn(3, ':').collect();
        if time_parts.len() < 2 {
            return 0;
        }
        let hour: u64 = time_parts[0].parse().unwrap_or(0);
        let min: u64 = time_parts[1].parse().unwrap_or(0);
        let sec_frac = time_parts.get(2).unwrap_or(&"0");
        let (sec_str, frac_str) = if let Some(pos) = sec_frac.find('.') {
            (&sec_frac[..pos], &sec_frac[pos+1..])
        } else {
            (*sec_frac, "")
        };
        let sec: u64 = sec_str.parse().unwrap_or(0);
        let frac_ns: u64 = if frac_str.is_empty() {
            0
        } else {
            let padded = format!("{:0<9}", frac_str);
            padded[..9.min(padded.len())].parse().unwrap_or(0)
        };

        // Days since Unix epoch (rough approximation — good enough for span ordering)
        let y = date_parts[0] as u64;
        let m = date_parts[1] as u64;
        let d = date_parts[2] as u64;
        // Zeller-style day count (simplified, not astronomically precise)
        let year = if m <= 2 { y - 1 } else { y };
        let month = if m <= 2 { m + 9 } else { m - 3 };
        let c = year / 100;
        let yy = year % 100;
        let jdn = (146097 * c) / 4 + (1461 * yy) / 4 + (153 * month + 2) / 5
            + d + 1721119;
        let unix_day = jdn.saturating_sub(2440588);
        let unix_sec = unix_day
            .saturating_mul(86400)
            .saturating_add(hour.saturating_mul(3600))
            .saturating_add(min.saturating_mul(60))
            .saturating_add(sec);
        unix_sec.saturating_mul(1_000_000_000).saturating_add(frac_ns)
    }

    /// Convert run_id (UUID v4, with hyphens) to 32-hex OTLP trace ID.
    fn run_id_to_trace_id(run_id: &str) -> String {
        run_id.replace('-', "")
    }

    /// Process one flight-log JSON line. Returns any spans that were completed.
    pub fn process_line(&mut self, line: &str) -> Vec<FinishedSpan> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
        let ts_str = v.get("ts").and_then(Value::as_str).unwrap_or("");
        let ts_ns = Self::parse_ts(ts_str);
        let data = v.get("data").cloned().unwrap_or(Value::Object(Default::default()));
        let agent = v.get("agent").and_then(Value::as_str).unwrap_or("");
        let turn = v.get("turn").and_then(Value::as_u64).unwrap_or(0);

        match kind {
            "scheduler_started" => {
                let rid = data.get("run_id").and_then(Value::as_str).unwrap_or("").to_owned();
                let trace_id = Self::run_id_to_trace_id(&rid);
                self.trace_id = Some(trace_id.clone());
                self.run_id = Some(rid);
                let span_id = self.next_span_id();
                self.run_span_id = Some(span_id.clone());
                let mut attrs = vec![
                    (crate::semconv::AGENTOS_RUN_ID.to_owned(),
                     SpanAttr::Str(self.run_id.clone().unwrap_or_default())),
                ];
                if let Some(ch) = data.get("config_hash").and_then(Value::as_str) {
                    attrs.push(("agentos.config_hash".to_owned(), SpanAttr::Str(ch.to_owned())));
                }
                self.open.insert(SpanKey::Agent("__scheduler__".to_owned()), OpenSpan {
                    trace_id,
                    span_id,
                    parent_span_id: None,
                    name: "agentd.run".to_owned(),
                    start_ts_ns: ts_ns,
                    attrs,
                    events: vec![],
                    synthesized: false,
                });
                vec![]
            }

            "scheduler_stopped" => {
                let count = data.get("agent_count").and_then(Value::as_u64).unwrap_or(0);
                if let Some(span) = self.open.remove(&SpanKey::Agent("__scheduler__".to_owned())) {
                    vec![self.finish(span, ts_ns,
                        vec![("agentos.agent_count".to_owned(), SpanAttr::Int(count as i64))],
                        false)]
                } else {
                    vec![]
                }
            }

            "agent_spawned" => {
                self.ensure_trace(ts_ns);
                let trace_id = self.trace_id.clone().unwrap_or_default();
                let agent_id = agent.to_owned();
                let span_id = self.next_span_id();
                let parent = self.run_span_id.clone();
                self.agent_span_ids.insert(agent_id.clone(), span_id.clone());

                let key = SpanKey::Agent(agent_id.clone());
                if let Some(existing) = self.open.remove(&key) {
                    let closed = self.finish(existing, ts_ns,
                        vec![("agentos.close_reason".to_owned(),
                              SpanAttr::Str("duplicate_open".to_owned()))], false);
                    self.open.insert(key.clone(), OpenSpan {
                        trace_id,
                        span_id,
                        parent_span_id: parent,
                        name: format!("agent.{agent_id}"),
                        start_ts_ns: ts_ns,
                        attrs: vec![(crate::semconv::AGENTOS_AGENT_ID.to_owned(),
                                     SpanAttr::Str(agent_id))],
                        events: vec![],
                        synthesized: false,
                    });
                    return vec![closed];
                }
                self.open.insert(key.clone(), OpenSpan {
                    trace_id,
                    span_id,
                    parent_span_id: parent,
                    name: format!("agent.{agent_id}"),
                    start_ts_ns: ts_ns,
                    attrs: vec![(crate::semconv::AGENTOS_AGENT_ID.to_owned(),
                                 SpanAttr::Str(agent_id))],
                    events: vec![],
                    synthesized: false,
                });
                vec![]
            }

            "agent_completed" | "agent_failed" => {
                let agent_id = agent.to_owned();
                let key = SpanKey::Agent(agent_id.clone());
                let is_error = kind == "agent_failed";
                // Prune span ID map to prevent unbounded growth in long-running multi-agent runs.
                self.agent_span_ids.remove(&agent_id);
                if let Some(span) = self.open.remove(&key) {
                    vec![self.finish(span, ts_ns, vec![], is_error)]
                } else {
                    vec![]
                }
            }

            "inference_request" | "inference_stream_started" => {
                self.ensure_agent_span(agent, ts_ns);
                let trace_id = self.trace_id.clone().unwrap_or_default();
                let agent_id = agent.to_owned();
                let model = data.get("model").and_then(Value::as_str).unwrap_or("").to_owned();
                let parent = self.agent_span_ids.get(agent).cloned();
                let span_id = self.next_span_id();

                let key = SpanKey::Inference(agent_id.clone(), turn);
                let mut attrs = vec![
                    (crate::semconv::AGENTOS_AGENT_ID.to_owned(), SpanAttr::Str(agent_id)),
                    (crate::semconv::AGENTOS_AGENT_TURN.to_owned(), SpanAttr::Int(turn as i64)),
                    (crate::semconv::GEN_AI_OPERATION_NAME.to_owned(),
                     SpanAttr::Str(crate::semconv::OP_CHAT.to_owned())),
                    (crate::semconv::GEN_AI_SYSTEM.to_owned(),
                     SpanAttr::Str(crate::semconv::SYSTEM_ANTHROPIC.to_owned())),
                ];
                if !model.is_empty() {
                    attrs.push((crate::semconv::GEN_AI_REQUEST_MODEL.to_owned(),
                                SpanAttr::Str(model)));
                }
                if let Some(mt) = data.get("max_tokens").and_then(Value::as_i64) {
                    attrs.push((crate::semconv::GEN_AI_REQUEST_MAX_TOKENS.to_owned(),
                                SpanAttr::Int(mt)));
                }

                if let Some(existing) = self.open.remove(&key) {
                    let closed = self.finish(existing, ts_ns,
                        vec![("agentos.close_reason".to_owned(),
                              SpanAttr::Str("duplicate_open".to_owned()))], false);
                    self.open.insert(key, OpenSpan {
                        trace_id, span_id, parent_span_id: parent,
                        name: "gen_ai.chat".to_owned(),
                        start_ts_ns: ts_ns, attrs, events: vec![], synthesized: false,
                    });
                    return vec![closed];
                }
                self.open.insert(key, OpenSpan {
                    trace_id, span_id, parent_span_id: parent,
                    name: "gen_ai.chat".to_owned(),
                    start_ts_ns: ts_ns, attrs, events: vec![], synthesized: false,
                });
                vec![]
            }

            "inference_response" | "inference_stream_completed" => {
                let key = SpanKey::Inference(agent.to_owned(), turn);
                let mut extra = vec![];
                if let Some(it) = data.get("input_tokens").and_then(Value::as_i64) {
                    extra.push((crate::semconv::GEN_AI_USAGE_INPUT_TOKENS.to_owned(),
                                SpanAttr::Int(it)));
                }
                if let Some(ot) = data.get("output_tokens").and_then(Value::as_i64) {
                    extra.push((crate::semconv::GEN_AI_USAGE_OUTPUT_TOKENS.to_owned(),
                                SpanAttr::Int(ot)));
                }
                if let Some(m) = data.get("model").and_then(Value::as_str) {
                    extra.push((crate::semconv::GEN_AI_RESPONSE_MODEL.to_owned(),
                                SpanAttr::Str(m.to_owned())));
                }
                if let Some(span) = self.open.remove(&key) {
                    vec![self.finish(span, ts_ns, extra, false)]
                } else {
                    vec![]
                }
            }

            "tool_call" => {
                self.ensure_agent_span(agent, ts_ns);
                let trace_id = self.trace_id.clone().unwrap_or_default();
                let agent_id = agent.to_owned();
                let tool_name = data.get("tool").and_then(Value::as_str)
                    .or_else(|| data.get("name").and_then(Value::as_str))
                    .unwrap_or("unknown").to_owned();
                // Use tool_use_id as key (unique per call); fall back to name so tests
                // written before tool IDs exist still work.
                let tool_key = data.get("id").and_then(Value::as_str)
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| tool_name.clone());
                let parent = self.agent_span_ids.get(agent).cloned();
                let span_id = self.next_span_id();
                let key = SpanKey::Tool(agent_id.clone(), turn, tool_key);
                let mut attrs = vec![
                    (crate::semconv::AGENTOS_AGENT_ID.to_owned(), SpanAttr::Str(agent_id)),
                    (crate::semconv::AGENTOS_AGENT_TURN.to_owned(), SpanAttr::Int(turn as i64)),
                    (crate::semconv::AGENTOS_TOOL_NAME.to_owned(), SpanAttr::Str(tool_name.clone())),
                    (crate::semconv::GEN_AI_OPERATION_NAME.to_owned(),
                     SpanAttr::Str(crate::semconv::OP_TOOL_CALL.to_owned())),
                ];
                if !self.redact_previews {
                    if let Some(prev) = data.get("input_preview").and_then(Value::as_str) {
                        attrs.push(("agentos.tool_input.preview".to_owned(),
                                    SpanAttr::Str(prev.to_owned())));
                    }
                }
                self.open.insert(key, OpenSpan {
                    trace_id, span_id, parent_span_id: parent,
                    name: format!("tool.{tool_name}"),
                    start_ts_ns: ts_ns, attrs, events: vec![], synthesized: false,
                });
                vec![]
            }

            "tool_result" => {
                // Match on tool_use_id first (same fallback as tool_call).
                let tool_key = data.get("id").and_then(Value::as_str)
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| {
                        data.get("tool").and_then(Value::as_str)
                            .or_else(|| data.get("name").and_then(Value::as_str))
                            .unwrap_or("unknown").to_owned()
                    });
                let key = SpanKey::Tool(agent.to_owned(), turn, tool_key);
                let is_error = data.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                let mut extra = vec![];
                if !self.redact_previews {
                    // agentd emits the field as "preview" (not "output_preview")
                    if let Some(prev) = data.get("preview").and_then(Value::as_str) {
                        extra.push(("agentos.tool_output.preview".to_owned(),
                                    SpanAttr::Str(prev.to_owned())));
                    }
                }
                if let Some(span) = self.open.remove(&key) {
                    vec![self.finish(span, ts_ns, extra, is_error)]
                } else {
                    vec![]
                }
            }

            "egress_brokered" => {
                // Span event only (no egress_completed to measure duration).
                let agent_key = SpanKey::Agent(agent.to_owned());
                if let Some(span) = self.open.get_mut(&agent_key) {
                    let mut attrs = vec![];
                    if let Some(dest) = data.get("dest").and_then(Value::as_str) {
                        attrs.push(("agentos.egress.dest".to_owned(),
                                    SpanAttr::Str(dest.to_owned())));
                    }
                    span.events.push(SpanEvent {
                        name: "egress_brokered".to_owned(),
                        ts_ns,
                        attrs,
                    });
                }
                vec![]
            }

            _ => vec![],
        }
    }

    /// Force-close all open spans (watchdog timeout or end-of-file).
    pub fn drain_all(&mut self, end_ts_ns: u64, reason: &str) -> Vec<FinishedSpan> {
        let keys: Vec<SpanKey> = self.open.keys().cloned().collect();
        let mut out = Vec::new();
        for key in keys {
            if let Some(span) = self.open.remove(&key) {
                out.push(self.finish(span, end_ts_ns,
                    vec![(crate::semconv::AGENTOS_CLOSE_REASON.to_owned(),
                          SpanAttr::Str(reason.to_owned()))], false));
            }
        }
        out
    }

    fn finish(&mut self, mut span: OpenSpan, end_ts_ns: u64,
              extra_attrs: Vec<(String, SpanAttr)>, status_error: bool) -> FinishedSpan {
        span.attrs.extend(extra_attrs);
        if span.synthesized {
            span.attrs.push((crate::semconv::AGENTOS_SPAN_SYNTHESIZED.to_owned(),
                             SpanAttr::Bool(true)));
        }
        FinishedSpan {
            trace_id: span.trace_id,
            span_id: span.span_id,
            parent_span_id: span.parent_span_id,
            name: span.name,
            start_ts_ns: span.start_ts_ns,
            end_ts_ns,
            attrs: span.attrs,
            events: span.events,
            status_error,
        }
    }

    /// Ensure there is a valid trace context (synthesize if scheduler_started was missed).
    fn ensure_trace(&mut self, ts_ns: u64) {
        if self.trace_id.is_none() {
            let synthetic_run_id = uuid::Uuid::new_v4().to_string();
            let trace_id = Self::run_id_to_trace_id(&synthetic_run_id);
            self.trace_id = Some(trace_id.clone());
            self.run_id = Some(synthetic_run_id);
            let span_id = self.next_span_id();
            self.run_span_id = Some(span_id.clone());
            self.open.insert(SpanKey::Agent("__scheduler__".to_owned()), OpenSpan {
                trace_id,
                span_id,
                parent_span_id: None,
                name: "agentd.run".to_owned(),
                start_ts_ns: ts_ns,
                attrs: vec![(crate::semconv::AGENTOS_SPAN_SYNTHESIZED.to_owned(),
                             SpanAttr::Bool(true))],
                events: vec![],
                synthesized: true,
            });
        }
    }

    /// Ensure an agent span exists (synthesize orphan if needed).
    fn ensure_agent_span(&mut self, agent_id: &str, ts_ns: u64) {
        self.ensure_trace(ts_ns);
        if !self.agent_span_ids.contains_key(agent_id) {
            let trace_id = self.trace_id.clone().unwrap_or_default();
            let span_id = self.next_span_id();
            let parent = self.run_span_id.clone();
            self.agent_span_ids.insert(agent_id.to_owned(), span_id.clone());
            self.open.insert(SpanKey::Agent(agent_id.to_owned()), OpenSpan {
                trace_id,
                span_id,
                parent_span_id: parent,
                name: format!("agent.{agent_id}"),
                start_ts_ns: ts_ns,
                attrs: vec![
                    (crate::semconv::AGENTOS_AGENT_ID.to_owned(),
                     SpanAttr::Str(agent_id.to_owned())),
                    (crate::semconv::AGENTOS_SPAN_SYNTHESIZED.to_owned(),
                     SpanAttr::Bool(true)),
                ],
                events: vec![],
                synthesized: true,
            });
        }
    }

    #[cfg(test)]
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    pub fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    pub fn open_span_count(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(kind: &str, agent: &str, turn: u64, data: serde_json::Value) -> String {
        serde_json::json!({
            "ts": "2024-01-15T12:00:00.000000000Z",
            "kind": kind,
            "agent": agent,
            "turn": turn,
            "data": data,
        }).to_string()
    }

    #[test]
    fn test_scheduler_started_sets_trace_id() {
        let mut sb = SpanBuilder::new(false);
        let line = make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "12345678-1234-1234-1234-123456789abc", "config_hash": "abc123"}));
        sb.process_line(&line);
        assert_eq!(sb.trace_id(), Some("12345678123412341234123456789abc"));
    }

    #[test]
    fn test_agent_spawn_and_complete() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000001", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "scout", 0,
            serde_json::json!({})));
        let spans = sb.process_line(&make_event("agent_completed", "scout", 0,
            serde_json::json!({})));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "agent.scout");
        assert!(!spans[0].status_error);
    }

    #[test]
    fn test_inference_span() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000002", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "scout", 0, serde_json::json!({})));
        sb.process_line(&make_event("inference_request", "scout", 1,
            serde_json::json!({"model": "claude-sonnet-4-5", "max_tokens": 4096})));
        let spans = sb.process_line(&make_event("inference_response", "scout", 1,
            serde_json::json!({"input_tokens": 100, "output_tokens": 50, "model": "claude-sonnet-4-5"})));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "gen_ai.chat");
        let has_input = spans[0].attrs.iter().any(|(k, _)| k == "gen_ai.usage.input_tokens");
        assert!(has_input);
    }

    #[test]
    fn test_tool_span() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000003", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "scout", 0, serde_json::json!({})));
        sb.process_line(&make_event("tool_call", "scout", 1,
            serde_json::json!({"tool": "read_file", "input_preview": "path.txt"})));
        let spans = sb.process_line(&make_event("tool_result", "scout", 1,
            serde_json::json!({"tool": "read_file", "is_error": false})));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "tool.read_file");
    }

    #[test]
    fn test_orphan_synthesis() {
        let mut sb = SpanBuilder::new(false);
        // No scheduler_started, no agent_spawned — orphan inference
        sb.process_line(&make_event("inference_request", "orphan", 1,
            serde_json::json!({"model": "x"})));
        // trace_id was synthesized
        assert!(sb.trace_id().is_some());
        // orphan agent span was synthesized
        assert!(sb.open_span_count() >= 2); // scheduler + agent + inference
    }

    #[test]
    fn test_duplicate_open_force_closes() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000004", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "alice", 0, serde_json::json!({})));
        // Second spawn without close — should force-close the first
        let spans = sb.process_line(&make_event("agent_spawned", "alice", 0, serde_json::json!({})));
        assert_eq!(spans.len(), 1);
        let has_reason = spans[0].attrs.iter()
            .any(|(k, v)| k == "agentos.close_reason"
                 && matches!(v, SpanAttr::Str(s) if s == "duplicate_open"));
        assert!(has_reason);
    }

    #[test]
    fn test_drain_all_watchdog() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000005", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "agent1", 0, serde_json::json!({})));
        let drained = sb.drain_all(9_000_000_000, "watchdog_timeout");
        assert!(drained.len() >= 2); // scheduler + agent
        let has_reason = drained.iter().any(|s|
            s.attrs.iter().any(|(k, v)| k == "agentos.close_reason"
                && matches!(v, SpanAttr::Str(r) if r == "watchdog_timeout")));
        assert!(has_reason);
    }

    #[test]
    fn test_streaming_events_as_alias() {
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000006", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "s", 0, serde_json::json!({})));
        sb.process_line(&make_event("inference_stream_started", "s", 1,
            serde_json::json!({"model": "claude-opus-4-6"})));
        let spans = sb.process_line(&make_event("inference_stream_completed", "s", 1,
            serde_json::json!({"input_tokens": 10, "output_tokens": 20, "model": "claude-opus-4-6"})));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "gen_ai.chat");
    }

    #[test]
    fn test_redact_previews() {
        // With redact=true: no preview attrs should appear.
        let mut sb = SpanBuilder::new(true);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000007", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "x", 0, serde_json::json!({})));
        sb.process_line(&make_event("tool_call", "x", 1,
            serde_json::json!({"tool": "read_file", "input_preview": "secret/path"})));
        let spans = sb.process_line(&make_event("tool_result", "x", 1,
            // "preview" is the field agentd emits (not "output_preview")
            serde_json::json!({"tool": "read_file", "is_error": false, "preview": "secret data"})));
        let has_preview = spans[0].attrs.iter().any(|(k, _)| k.contains("preview"));
        assert!(!has_preview, "previews should be redacted");
    }

    #[test]
    fn test_preview_present_when_not_redacted() {
        // With redact=false: preview attrs must appear.
        let mut sb = SpanBuilder::new(false);
        sb.process_line(&make_event("scheduler_started", "agentd", 0,
            serde_json::json!({"run_id": "aaaaaaaa-0000-0000-0000-000000000008", "config_hash": "x"})));
        sb.process_line(&make_event("agent_spawned", "x", 0, serde_json::json!({})));
        sb.process_line(&make_event("tool_call", "x", 1,
            serde_json::json!({"tool": "read_file", "id": "tu_abc", "input_preview": "my/path"})));
        let spans = sb.process_line(&make_event("tool_result", "x", 1,
            serde_json::json!({"id": "tu_abc", "is_error": false, "preview": "file contents"})));
        let has_input_preview = spans[0].attrs.iter()
            .any(|(k, _)| k == "agentos.tool_input.preview");
        let has_output_preview = spans[0].attrs.iter()
            .any(|(k, _)| k == "agentos.tool_output.preview");
        assert!(has_input_preview, "input_preview should be present when not redacted");
        assert!(has_output_preview, "output preview should be present when not redacted");
    }

    #[test]
    fn test_ts_parse() {
        // Z suffix (tests use this format)
        let ns = SpanBuilder::parse_ts("2024-01-15T12:00:00.000000000Z");
        assert!(ns > 0, "timestamp should parse to nonzero");
    }

    #[test]
    fn test_ts_parse_offset_format() {
        // chrono::Utc::now().to_rfc3339() emits "+00:00", not "Z"
        let with_z   = SpanBuilder::parse_ts("2024-01-15T12:00:00.000000000Z");
        let with_off = SpanBuilder::parse_ts("2024-01-15T12:00:00.000000000+00:00");
        assert_eq!(with_z, with_off, "Z and +00:00 should parse identically");

        // Exact-second with +00:00 (no sub-second component — the bug case)
        let exact = SpanBuilder::parse_ts("2024-01-15T12:00:00+00:00");
        assert_eq!(with_z, exact, "exact-second +00:00 should match Z variant");
    }
}
