//! Synthetic search and a real loopback OTLP HTTP receiver. Never loads/downloads a model.
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use tantivy::doc;

static NEXT: AtomicU64 = AtomicU64::new(0);
const QUERY: &str = "syntheticquerymarker";
const BODY: &str = "syntheticquerymarker privatebodymarker";
const NOTE: &str = "00-Inbox/privatepathmarker.md";

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "vagus-tracing-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let this = Self(path.canonicalize().unwrap());
        for child in ["vault", "home", "data"] {
            fs::create_dir_all(this.0.join(child)).unwrap();
        }
        this
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vagus"));
        // No host endpoint, authorization, or proxy can affect this synthetic test.
        cmd.env_clear()
            .env("HOME", self.0.join("home"))
            .env("VAGUS_VAULT", self.0.join("vault"))
            .env("VAGUS_DATA_DIR", self.0.join("data"))
            .env("VAGUS_CACHE_DIR", self.0.join("cache"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("NO_COLOR", "1");
        cmd
    }

    fn search(&self) -> Command {
        let mut cmd = self.command();
        cmd.args([
            "search",
            QUERY,
            "--mode",
            "bm25",
            "--no-index",
            "--all",
            "--json",
            "--full",
        ]);
        cmd
    }

    fn fixture(&self) {
        // Create Vagus's real schema, then seed a single synthetic lexical hit without embedding.
        assert!(self.search().output().unwrap().status.success());
        let db = rusqlite::Connection::open(self.0.join("data/meta.db")).unwrap();
        db.execute(
            "INSERT INTO files(path,mtime,sha256,indexed_at) VALUES (?1,0,'synthetic',0)",
            [NOTE],
        )
        .unwrap();
        db.execute("INSERT INTO chunks(id,path,ord,kind,heading_path,body) VALUES ('0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',?1,0,0,'privateheadingmarker',?2)", [NOTE, BODY]).unwrap();
        let index = tantivy::Index::open_in_dir(self.0.join("data/tantivy")).unwrap();
        let schema = index.schema();
        let mut writer = index
            .writer::<tantivy::TantivyDocument>(50_000_000)
            .unwrap();
        writer
            .add_document(tantivy::doc!(
                schema.get_field("path").unwrap() => NOTE,
                schema.get_field("chunk_id").unwrap() => "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                schema.get_field("heading").unwrap() => "privateheadingmarker",
                schema.get_field("body").unwrap() => BODY,
            ))
            .unwrap();
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }

    #[cfg(feature = "local-tracing")]
    fn trace(&self, profile: &str) -> PathBuf {
        self.0.join(format!("traces/{profile}.jsonl"))
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn equal(a: &Output, b: &Output) {
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
}

#[cfg(feature = "local-tracing")]
mod enabled {
    use super::*;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{Duration, Instant};

    fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    #[test]
    fn real_search_json_profiles_preserve_output_and_partition_content() {
        let s = Sandbox::new();
        s.fixture();
        let baseline = s.search().output().unwrap();
        assert!(baseline.status.success());
        assert!(String::from_utf8_lossy(&baseline.stdout).contains(NOTE));
        assert!(!s.0.join("state").exists());
        for profile in ["safe", "research"] {
            let output = s
                .search()
                .args(["--trace-profile", profile, "--trace-file"])
                .arg(s.trace(profile))
                .output()
                .unwrap();
            equal(&baseline, &output);
            let values = records(&s.trace(profile));
            let text = fs::read_to_string(s.trace(profile)).unwrap();
            for marker in [
                QUERY,
                "privatebodymarker",
                "privatepathmarker",
                "privateheadingmarker",
            ] {
                assert_eq!(
                    text.contains(marker),
                    profile == "research",
                    "marker {marker}"
                );
            }
            let names: Vec<_> = values
                .iter()
                .filter(|r| r["fields"]["message"] == "new")
                .filter_map(|r| r["span"]["name"].as_str())
                .collect();
            for name in [
                "command",
                "config.load",
                "storage.validate",
                "search",
                "query",
                "lexical.search",
                "hydrate",
                "postprocess",
                "output",
            ] {
                assert!(names.contains(&name), "missing {name}: {names:?}");
            }
            let hydration = values
                .iter()
                .find(|r| r["span"]["name"] == "hydrate" && r["fields"]["message"] == "new")
                .unwrap();
            let parents: Vec<_> = hydration["spans"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|p| p["name"].as_str())
                .collect();
            assert_eq!(parents, ["command", "search", "query"]);
            // Contiguous spans close before output; no long-lived aggregate re-entry accounting.
            let post_end = values
                .iter()
                .position(|r| {
                    r["span"]["name"] == "postprocess" && r["fields"]["message"] == "close"
                })
                .unwrap();
            let output_start = values
                .iter()
                .position(|r| r["span"]["name"] == "output" && r["fields"]["message"] == "new")
                .unwrap();
            assert!(post_end < output_start);
            assert_eq!(values.last().unwrap()["span"]["name"], "command");
        }
    }

    #[test]
    fn eval_query_correlation_does_not_change_report_or_leak_safe_content() {
        let s = Sandbox::new();
        s.fixture();
        let labels = s.0.join("labels.jsonl");
        fs::write(&labels, format!("{{\"query\":\"{QUERY}\",\"relevant\":[\"{NOTE}\"]}}\n{{\"query\":\"absentsyntheticterm\",\"relevant\":[]}}\n")).unwrap();
        let run = |profile: Option<&str>| {
            let mut cmd = s.command();
            cmd.arg("eval")
                .arg(&labels)
                .args(["--mode", "bm25", "--json"]);
            if let Some(profile) = profile {
                cmd.args(["--trace-profile", profile, "--trace-file"])
                    .arg(s.trace(profile));
            }
            cmd.output().unwrap()
        };
        let baseline = run(None);
        assert!(
            baseline.status.success(),
            "{}",
            String::from_utf8_lossy(&baseline.stderr)
        );
        for profile in ["safe", "research"] {
            equal(&baseline, &run(Some(profile)));
            let values = records(&s.trace(profile));
            let queries: Vec<_> = values
                .iter()
                .filter(|r| r["span"]["name"] == "eval.query" && r["fields"]["message"] == "new")
                .collect();
            assert_eq!(queries.len(), 2);
            assert_eq!(queries[0]["span"]["query_id"], 0);
            assert_eq!(queries[1]["span"]["query_id"], 1);
            assert!(queries.iter().all(|r| {
                r["spans"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == "eval")
            }));
            let text = fs::read_to_string(s.trace(profile)).unwrap();
            assert_eq!(text.contains(QUERY), profile == "research");
        }
    }

    #[test]
    fn empty_index_records_contiguous_stages_without_loading_models() {
        let s = Sandbox::new();
        let file = s.trace("index");
        let output = s
            .command()
            .args(["--trace-file"])
            .arg(&file)
            .arg("index")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values = records(&file);
        let names: Vec<_> = values
            .iter()
            .filter(|r| r["fields"]["message"] == "new")
            .filter_map(|r| r["span"]["name"].as_str())
            .collect();
        for name in [
            "index",
            "index.snapshot",
            "index.reconcile",
            "lexical.commit",
            "vector.rebuild",
            "vector.persist",
        ] {
            assert!(names.contains(&name), "missing {name}");
        }
        assert!(!names.contains(&"model.load"));
        assert!(!names.contains(&"model.inference"));
    }

    #[test]
    fn traced_search_preserves_upstream_deferred_auto_refresh() {
        let s = Sandbox::new();
        s.fixture();
        let db = rusqlite::Connection::open(s.0.join("data/meta.db")).unwrap();
        db.execute("INSERT INTO meta(k,v) VALUES ('rebuild_pending','1')", [])
            .unwrap();
        let args = [
            "search", QUERY, "--mode", "bm25", "--all", "--json", "--full",
        ];
        let baseline = s.command().args(args).output().unwrap();
        assert!(baseline.status.success());
        assert!(String::from_utf8_lossy(&baseline.stderr).contains("vagus index"));
        let file = s.trace("deferred");
        let traced = s
            .command()
            .arg("--trace-file")
            .arg(&file)
            .args(args)
            .output()
            .unwrap();
        equal(&baseline, &traced);
        let values = records(&file);
        assert!(
            values
                .iter()
                .any(|r| r["fields"]["message"] == "refresh outcome"
                    && r["fields"]["index_refreshed"] == false)
        );
        assert!(!values.iter().any(|r| r["span"]["name"] == "model.load"));
        assert_eq!(
            db.query_row("SELECT v FROM meta WHERE k='rebuild_pending'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "1"
        );
    }

    #[test]
    fn invalid_file_and_vault_alias_fail_visibly_without_changing_result() {
        let s = Sandbox::new();
        s.fixture();
        let baseline = s.search().output().unwrap();
        symlink(s.0.join("vault"), s.0.join("alias")).unwrap();
        for path in [
            s.0.join("vault/missing/out.jsonl"),
            s.0.join("alias/missing/out.jsonl"),
            s.0.join("data/out.jsonl"),
        ] {
            let output = s.search().arg("--trace-file").arg(path).output().unwrap();
            assert_eq!(output.status.code(), baseline.status.code());
            assert_eq!(output.stdout, baseline.stdout);
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("tracing initialization failed")
            );
        }
        assert!(!s.0.join("vault/missing").exists());
        assert!(!s.0.join("data/out.jsonl").exists());
        let file = s.trace("existing");
        assert!(
            s.search()
                .arg("--trace-file")
                .arg(&file)
                .output()
                .unwrap()
                .status
                .success()
        );
        let before = fs::read(&file).unwrap();
        let output = s.search().arg("--trace-file").arg(&file).output().unwrap();
        assert!(String::from_utf8_lossy(&output.stderr).contains("tracing initialization failed"));
        assert_eq!(before, fs::read(file).unwrap());
    }

    #[test]
    fn parent_components_in_trace_or_vault_paths_reject_before_directory_creation() {
        let s = Sandbox::new();
        fs::create_dir_all(s.0.join("vault/subdir")).unwrap();
        fs::create_dir_all(s.0.join("outside")).unwrap();
        symlink(s.0.join("vault/subdir"), s.0.join("outside/alias")).unwrap();
        for (vault, file, forbidden) in [
            (
                s.0.join("vault"),
                s.0.join("outside/alias/../trace-missing/run.jsonl"),
                s.0.join("vault/trace-missing"),
            ),
            (
                s.0.join("outside/alias/.."),
                s.0.join("vault/vault-missing/run.jsonl"),
                s.0.join("vault/vault-missing"),
            ),
        ] {
            let baseline = s
                .command()
                .env("VAGUS_VAULT", &vault)
                .arg("tutorial")
                .output()
                .unwrap();
            let traced = s
                .command()
                .env("VAGUS_VAULT", &vault)
                .args(["--trace-profile", "research", "--trace-file"])
                .arg(&file)
                .arg("tutorial")
                .output()
                .unwrap();
            assert_eq!(traced.status.code(), baseline.status.code());
            assert_eq!(traced.stdout, baseline.stdout);
            assert!(
                String::from_utf8_lossy(&traced.stderr).contains("tracing initialization failed")
            );
            assert!(
                !forbidden.exists(),
                "must not create directories before rejecting traversal"
            );
        }
        assert!(!s.0.join("outside/trace-missing").exists());
    }

    #[test]
    fn activation_is_explicit_and_legacy_flag_is_safe() {
        let s = Sandbox::new();
        let baseline = s.command().arg("tutorial").output().unwrap();
        // Ambient endpoint/RUST_LOG cannot enable a subscriber or research fields.
        let output = s
            .command()
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
            .env("RUST_LOG", "trace")
            .arg("tutorial")
            .output()
            .unwrap();
        equal(&baseline, &output);
        assert!(!s.0.join("state").exists());
        let output = s.command().args(["--trace", "tutorial"]).output().unwrap();
        equal(&baseline, &output);
        assert_eq!(
            fs::read_dir(s.0.join("state/vasovagal/traces/vagus"))
                .unwrap()
                .count(),
            1
        );
        let output = s
            .command()
            .env("VAGUS_TRACE_PROFILE", "invalid")
            .arg("tutorial")
            .output()
            .unwrap();
        assert_eq!(output.stdout, baseline.stdout);
        assert!(String::from_utf8_lossy(&output.stderr).contains("tracing initialization failed"));
    }

    #[test]
    fn plugin_exact_status_and_command_errors_survive_all_profiles() {
        let s = Sandbox::new();
        let bin = s.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let plugin = bin.join("vagus-probe");
        fs::write(
            &plugin,
            "#!/bin/sh\nprintf '%s' \"$1\"\nprintf 'synthetic-error' >&2\nexit 42\n",
        )
        .unwrap();
        fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
        let baseline = s
            .command()
            .env("PATH", &bin)
            .args(["probe", "synthetic arg"])
            .output()
            .unwrap();
        assert_eq!(baseline.status.code(), Some(42));
        for profile in ["safe", "research"] {
            let output = s
                .command()
                .env("PATH", &bin)
                .args(["--trace-profile", profile, "probe", "synthetic arg"])
                .output()
                .unwrap();
            equal(&baseline, &output);
        }
        let error = s.search().arg("--relevance").output().unwrap();
        assert!(!error.status.success());
        for profile in ["safe", "research"] {
            let output = s
                .search()
                .args(["--trace-profile", profile, "--relevance"])
                .output()
                .unwrap();
            equal(&error, &output);
        }
    }

    /// A real HTTP/protobuf OTLP receiver, decoding standard wire spans (no mock exporter).
    fn receive(listener: TcpListener, expected_path: &str) -> ExportTraceServiceRequest {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut socket, _) = loop {
            if let Ok(pair) = listener.accept() {
                break pair;
            }
            assert!(Instant::now() < deadline, "OTLP request not received");
            std::thread::sleep(Duration::from_millis(10));
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let (end, size) = loop {
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                assert!(headers.starts_with(&format!("post {expected_path} http/1.1")));
                assert!(headers.contains("application/x-protobuf"));
                let size: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                break (end + 4, size);
            }
        };
        while bytes.len() < end + size {
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        ExportTraceServiceRequest::decode(&bytes[end..end + size]).unwrap()
    }

    #[test]
    fn direct_otlp_profiles_have_real_parents_wall_times_and_no_ambient_resource() {
        let s = Sandbox::new();
        s.fixture();
        let baseline = s.search().output().unwrap();
        for profile in ["safe", "research"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let receiver = std::thread::spawn(move || receive(listener, "/v1/traces"));
            let output = s
                .search()
                .args(["--trace-profile", profile, "--trace-otlp"])
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
                .env(
                    "OTEL_RESOURCE_ATTRIBUTES",
                    "privateenvmarker=must-not-export",
                )
                .output()
                .unwrap();
            equal(&baseline, &output);
            let request = receiver.join().unwrap();
            let debug = format!("{request:?}");
            for marker in [QUERY, "privatebodymarker", "privatepathmarker"] {
                assert_eq!(debug.contains(marker), profile == "research");
            }
            assert!(!debug.contains("privateenvmarker"));
            let spans: Vec<_> = request
                .resource_spans
                .iter()
                .flat_map(|r| &r.scope_spans)
                .flat_map(|s| &s.spans)
                .collect();
            let root = spans.iter().find(|s| s.name == "command").unwrap();
            for child in &spans {
                assert!(child.end_time_unix_nano >= child.start_time_unix_nano);
                assert_eq!(child.trace_id, root.trace_id);
                if child.name != "command" {
                    let parent = spans
                        .iter()
                        .find(|p| p.span_id == child.parent_span_id)
                        .unwrap();
                    assert!(parent.start_time_unix_nano <= child.start_time_unix_nano);
                    assert!(parent.end_time_unix_nano >= child.end_time_unix_nano);
                }
            }
            assert!(
                !s.0.join("state").exists(),
                "OTLP-only must not create local files"
            );
        }
    }

    #[test]
    fn otlp_endpoint_precedence_and_base_paths_are_explicit() {
        let s = Sandbox::new();
        let baseline = s.command().arg("tutorial").output().unwrap();
        for (generic_suffix, traces_suffix, expected_path) in [
            (Some("/base"), None, "/base/v1/traces"),
            (Some("/base/"), None, "/base/v1/traces"),
            (None, Some("/complete"), "/complete"),
            (Some("/wrong"), Some("/complete"), "/complete"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let receiver = std::thread::spawn(move || receive(listener, expected_path));
            let mut cmd = s.command();
            cmd.args(["--trace-otlp", "tutorial"]);
            if let Some(suffix) = generic_suffix {
                cmd.env("OTEL_EXPORTER_OTLP_ENDPOINT", format!("{endpoint}{suffix}"));
            }
            if let Some(suffix) = traces_suffix {
                cmd.env(
                    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                    format!("{endpoint}{suffix}"),
                );
            }
            equal(&baseline, &cmd.output().unwrap());
            assert!(!receiver.join().unwrap().resource_spans.is_empty());
        }
    }

    #[test]
    fn malformed_selected_otlp_endpoint_never_falls_back() {
        let s = Sandbox::new();
        let baseline = s.command().arg("tutorial").output().unwrap();
        // A valid lower-priority destination must not receive research events when the selected
        // traces endpoint is malformed. Generic malformed values must fail initialization too.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        for selected in [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        ] {
            for invalid in ["not a url", "", "relative/path", "http://bad host"] {
                let output = s
                    .command()
                    .env(
                        "OTEL_EXPORTER_OTLP_ENDPOINT",
                        format!("http://{}", listener.local_addr().unwrap()),
                    )
                    .env(selected, invalid)
                    .args(["--trace-profile", "research", "--trace-otlp", "tutorial"])
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), baseline.status.code());
                assert_eq!(output.stdout, baseline.stdout);
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("tracing initialization failed")
                );
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
        }
    }

    #[test]
    fn unavailable_exporter_is_visible_and_shutdown_is_bounded() {
        let s = Sandbox::new();
        let baseline = s.command().arg("tutorial").output().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let start = Instant::now();
        let output = s
            .command()
            .args(["--trace-otlp", "tutorial"])
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
            .output()
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(output.stdout, baseline.stdout);
        assert_eq!(output.status.code(), baseline.status.code());
        assert!(String::from_utf8_lossy(&output.stderr).contains("tracing"));
        // Accept a request but never respond: the library flush must still return on its bound.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let start = Instant::now();
        let output = s
            .command()
            .args(["--trace-otlp", "tutorial"])
            .env(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .output()
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(output.stdout, baseline.stdout);
        assert_eq!(output.status.code(), baseline.status.code());
        assert!(String::from_utf8_lossy(&output.stderr).contains("tracing"));
    }
}

#[cfg(not(feature = "local-tracing"))]
#[test]
fn compiled_out_flags_and_environment_are_inert() {
    let s = Sandbox::new();
    s.fixture();
    let baseline = s.search().output().unwrap();
    let output = s
        .search()
        .args([
            "--trace",
            "--trace-profile",
            "research",
            "--trace-otlp",
            "--trace-file",
        ])
        .arg(s.0.join("vault/forbidden.jsonl"))
        .env("VAGUS_TRACE_PROFILE", "invalid")
        .env("VASOVAGAL_TRACE", "invalid")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
        .output()
        .unwrap();
    equal(&baseline, &output);
    assert!(!s.0.join("state").exists());
    assert!(!s.0.join("vault/forbidden.jsonl").exists());
}
