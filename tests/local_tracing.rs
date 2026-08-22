use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let sequence = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let raw = std::env::temp_dir().join(format!(
            "vagus-local-tracing-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&raw).unwrap();
        // macOS exposes /tmp through a symlink. The shared exporter intentionally rejects symlink
        // path components, so use the canonical spelling for every isolated test path.
        let root = raw.canonicalize().unwrap();
        for child in ["home", "vault", "config"] {
            fs::create_dir_all(root.join(child)).unwrap();
        }
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vagus"));
        command
            .env("HOME", self.root.join("home"))
            .env("VAGUS_VAULT", self.root.join("vault"))
            .env("VAGUS_DATA_DIR", self.root.join("data"))
            .env("VAGUS_CACHE_DIR", self.root.join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("NO_COLOR", "1")
            .env_remove("VASOVAGAL_TRACE");
        command
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config/vasovagal/vagus.yaml")
    }

    fn trace_files(&self) -> Vec<PathBuf> {
        fn visit(path: &Path, output: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, output);
                } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                    output.push(path);
                }
            }
        }
        let mut files = Vec::new();
        visit(&self.root.join("state"), &mut files);
        files.sort();
        files
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn search(command: &mut Command, cli_trace: bool) -> Output {
    if cli_trace {
        command.arg("--trace");
    }
    command.args([
        "search",
        "PRIVATE QUERY MUST NOT LEAK",
        "--mode",
        "bm25",
        "--no-index",
        "--json",
    ]);
    command.output().unwrap()
}

#[cfg(all(feature = "local-tracing", unix))]
mod enabled {
    use std::collections::HashMap;

    use super::*;

    fn records(sandbox: &Sandbox) -> Vec<serde_json::Value> {
        use std::os::unix::fs::PermissionsExt;

        let files = sandbox.trace_files();
        assert_eq!(files.len(), 1, "trace files: {files:?}");
        assert_eq!(
            fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(files[0].parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let bytes = fs::read(&files[0]).unwrap();
        assert!(bytes.len() < 5 * 1024 * 1024);
        assert!(
            !bytes
                .windows(b"PRIVATE QUERY".len())
                .any(|window| window == b"PRIVATE QUERY")
        );
        let records: Vec<serde_json::Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                vasovagal_tracing::validate_line_v1(line).unwrap();
                serde_json::from_slice(line).unwrap()
            })
            .collect();
        let summary = records
            .iter()
            .find(|record| record["record_type"] == "session_summary")
            .unwrap();
        for counter in [
            "rejected_operations",
            "rejected_fields",
            "rejected_types",
            "privacy_violations",
            "oversized_records",
            "writer_errors",
            "queue_drops",
        ] {
            assert_eq!(
                summary["counters"][counter], 0,
                "nonzero {counter}: {summary}"
            );
        }
        records
    }

    #[test]
    fn cli_activation_preserves_output_validates_schema_and_parents_stages() {
        let baseline = Sandbox::new("baseline");
        let baseline_output = search(&mut baseline.command(), false);
        assert!(baseline_output.status.success());
        assert!(baseline.trace_files().is_empty());

        let traced = Sandbox::new("cli");
        // CLI activation must short-circuit even an invalid present environment value.
        let mut command = traced.command();
        command.env("VASOVAGAL_TRACE", " invalid ");
        let traced_output = search(&mut command, true);
        assert_eq!(traced_output.status.code(), baseline_output.status.code());
        assert_eq!(traced_output.stdout, baseline_output.stdout);
        assert_eq!(traced_output.stderr, baseline_output.stderr);

        let records = records(&traced);
        let starts: HashMap<&str, &serde_json::Value> = records
            .iter()
            .filter(|record| record["record_type"] == "span_start")
            .filter_map(|record| {
                record["operation"]
                    .as_str()
                    .map(|operation| (operation, record))
            })
            .collect();
        let command = starts["vagus.command"];
        let search = starts["vagus.search"];
        let retrieve = starts["vagus.search.retrieve"];
        let bm25 = starts["vagus.search.retrieve.bm25"];
        assert_eq!(search["parent_span_id"], command["span_id"]);
        assert_eq!(retrieve["parent_span_id"], search["span_id"]);
        assert_eq!(bm25["parent_span_id"], retrieve["span_id"]);
        assert_eq!(
            command["attributes"],
            serde_json::json!({"command": "search"})
        );
        assert!(records.iter().any(|record| {
            record["record_type"] == "span_end"
                && record["operation"] == "vagus.command"
                && record["outcome"] == "ok"
        }));
    }

    #[test]
    fn index_phases_are_schema_valid_siblings_without_per_item_spans() {
        let sandbox = Sandbox::new("index-phases");
        let output = sandbox
            .command()
            .args(["--trace", "index"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let records = records(&sandbox);
        let starts: HashMap<&str, &serde_json::Value> = records
            .iter()
            .filter(|record| record["record_type"] == "span_start")
            .filter_map(|record| {
                record["operation"]
                    .as_str()
                    .map(|operation| (operation, record))
            })
            .collect();
        let index = starts["vagus.index"];
        for operation in [
            "vagus.index.snapshot",
            "vagus.index.reconcile",
            "vagus.index.embed",
            "vagus.index.lexical_commit",
            "vagus.index.vector_persist",
        ] {
            assert_eq!(starts[operation]["parent_span_id"], index["span_id"]);
        }
        assert_eq!(
            records
                .iter()
                .filter(|record| record["operation"] == "vagus.index.embed")
                .count(),
            2,
            "one aggregate start/end span, never one per note or chunk"
        );
    }

    #[test]
    fn human_output_and_result_error_exit_are_unchanged() {
        let tutorial = Sandbox::new("tutorial-compat");
        let baseline = tutorial.command().arg("tutorial").output().unwrap();
        let traced = tutorial
            .command()
            .args(["--trace", "tutorial"])
            .output()
            .unwrap();
        assert_eq!(traced.status.code(), baseline.status.code());
        assert_eq!(traced.stdout, baseline.stdout);
        assert_eq!(traced.stderr, baseline.stderr);

        let error = Sandbox::new("error-compat");
        let args = [
            "search",
            "PRIVATE QUERY MUST NOT LEAK",
            "--mode",
            "bm25",
            "--no-index",
            "--json",
            "--relevance",
        ];
        let baseline = error.command().args(args).output().unwrap();
        let traced = error.command().arg("--trace").args(args).output().unwrap();
        assert!(!baseline.status.success());
        assert_eq!(traced.status.code(), baseline.status.code());
        assert_eq!(traced.stdout, baseline.stdout);
        assert_eq!(traced.stderr, baseline.stderr);
        let records = records(&error);
        assert!(records.iter().any(|record| {
            record["record_type"] == "span_end"
                && record["operation"] == "vagus.command"
                && record["outcome"] == "error"
                && record["error_code"] == "other"
        }));
    }

    #[test]
    fn environment_and_strict_yaml_each_activate_the_shared_layer() {
        let environment = Sandbox::new("environment");
        let mut command = environment.command();
        command.env("VASOVAGAL_TRACE", "true");
        let output = search(&mut command, false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!records(&environment).is_empty());

        let yaml = Sandbox::new("yaml");
        fs::create_dir_all(yaml.config_path().parent().unwrap()).unwrap();
        fs::write(
            yaml.config_path(),
            "version: 1\ntracing:\n  enabled: true\n",
        )
        .unwrap();
        let output = search(&mut yaml.command(), false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!records(&yaml).is_empty());
    }

    #[test]
    fn invalid_yaml_fails_closed_without_changing_the_command() {
        let sandbox = Sandbox::new("invalid-config");
        fs::create_dir_all(sandbox.config_path().parent().unwrap()).unwrap();
        fs::write(
            sandbox.config_path(),
            "version: 1\ntracing:\n  enabled: \"true\"\n",
        )
        .unwrap();
        let output = search(&mut sandbox.command(), false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"[]\n");
        assert!(output.stderr.is_empty());
        assert!(sandbox.trace_files().is_empty());
    }
}

#[cfg(not(feature = "local-tracing"))]
#[test]
fn compiled_out_flag_env_and_yaml_are_inert() {
    let sandbox = Sandbox::new("compiled-out");
    fs::create_dir_all(sandbox.config_path().parent().unwrap()).unwrap();
    fs::write(
        sandbox.config_path(),
        "version: 1\ntracing:\n  enabled: true\n",
    )
    .unwrap();
    let mut command = sandbox.command();
    command.env("VASOVAGAL_TRACE", "true");
    let output = search(&mut command, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"[]\n");
    assert!(output.stderr.is_empty());
    assert!(sandbox.trace_files().is_empty());
}
