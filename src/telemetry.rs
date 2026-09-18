//! Opt-in standard tracing subscribers. No application recorder, queue, or replay format.
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_subscriber::{Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt};

const SHUTDOWN: Duration = Duration::from_secs(2);

pub(crate) struct Guard(Option<SdkTracerProvider>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(provider) = &self.0
            && provider.shutdown_with_timeout(SHUTDOWN).is_err()
        {
            eprintln!("vagus: tracing shutdown/export failed; traces may be incomplete");
        }
    }
}

/// Exporter diagnostics are deliberately reduced to a fixed message, never an endpoint/header/error
/// dump. SDK worker threads use the global dispatcher; these events do not enter either exporter.
struct ExportErrors;
impl<S: tracing::Subscriber> Layer<S> for ExportErrors {
    fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        eprintln!("vagus: tracing exporter failed; traces may be incomplete");
    }
}

pub(crate) fn init(cli: &crate::Cli) -> Result<Option<Guard>> {
    let profile = if let Some(profile) = cli.trace_profile {
        Some(profile)
    } else if cli.trace || cli.trace_file.is_some() || cli.trace_otlp {
        Some(crate::TraceProfile::Safe)
    } else {
        match std::env::var("VAGUS_TRACE_PROFILE").as_deref() {
            Ok("safe") => Some(crate::TraceProfile::Safe),
            Ok("research") => Some(crate::TraceProfile::Research),
            Ok("off") => None,
            Err(std::env::VarError::NotPresent) => {
                match std::env::var("VASOVAGAL_TRACE").as_deref() {
                    Ok("true") => Some(crate::TraceProfile::Safe),
                    Ok("false") | Err(std::env::VarError::NotPresent) => None,
                    _ => bail!("invalid VASOVAGAL_TRACE (expected true or false)"),
                }
            }
            _ => bail!("invalid VAGUS_TRACE_PROFILE (expected off, safe, or research)"),
        }
    };
    let Some(profile) = profile else {
        return Ok(None);
    };
    let research = matches!(profile, crate::TraceProfile::Research);
    // No EnvFilter/RUST_LOG: third-party logs and ambient OTEL settings cannot enable content.
    let allowed = move |meta: &tracing::Metadata<'_>| {
        meta.target() == "vagus::timing" || (research && meta.target() == "vagus::research")
    };

    let file = if cli.trace_file.is_some() || !cli.trace_otlp {
        let path = match &cli.trace_file {
            Some(path) => path.clone(),
            None => default_path()?,
        };
        Some(private_file(&path)?)
    } else {
        None
    };
    let json = file.map(|file| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(Mutex::new(file))
            .with_span_events(
                tracing_subscriber::fmt::format::FmtSpan::NEW
                    | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
            )
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(filter_fn(allowed))
    });

    let provider = if cli.trace_otlp {
        // Explicit opt-in still requires an explicit endpoint; never default to localhost or a cloud.
        if !std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
            .is_ok_and(|endpoint| !endpoint.is_empty())
        {
            bail!(
                "--trace-otlp requires OTEL_EXPORTER_OTLP_ENDPOINT or OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"
            );
        }
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_timeout(SHUTDOWN)
            .build()
            .map_err(|_| anyhow::anyhow!("cannot initialize OTLP HTTP/protobuf exporter"))?;
        Some(
            SdkTracerProvider::builder()
                // Empty builder deliberately excludes host/process/environment resource detectors.
                .with_resource(Resource::builder_empty().with_service_name("vagus").build())
                .with_batch_exporter(exporter)
                .build(),
        )
    } else {
        None
    };
    let otlp = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("vagus"))
            .with_location(false)
            .with_threads(false)
            .with_tracked_inactivity(false)
            .with_filter(filter_fn(allowed))
    });
    let guard = Guard(provider);
    tracing_subscriber::registry()
        .with(json)
        .with(otlp)
        .with(ExportErrors.with_filter(filter_fn(|meta| {
            meta.target() == "opentelemetry_sdk" && *meta.level() == tracing::Level::ERROR
        })))
        .try_init()
        .map_err(|_| anyhow::anyhow!("cannot install tracing subscriber"))?;
    Ok(Some(guard))
}

fn default_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .ok_or_else(|| anyhow::anyhow!("cannot resolve tracing state directory"))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(base
        .join("vasovagal/traces/vagus")
        .join(format!("{}-{nonce}.jsonl", std::process::id())))
}

fn private_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let vault = std::env::var_os("VAGUS_VAULT")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join("brain")))
        .ok_or_else(|| anyhow::anyhow!("cannot resolve vault for tracing safety check"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Check BEFORE any derived write, including missing ancestors and symlink aliases.
    if crate::path_safety::overlap(parent, &vault)? {
        bail!("tracing output directory overlaps vault");
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    if crate::path_safety::overlap(parent, &vault)?
        || fs::metadata(parent)?.permissions().mode() & 0o077 != 0
    {
        bail!("tracing output requires a private directory outside the vault");
    }
    // create_new also refuses existing symlinks. No overwrite, rotation, retention, or durability claim.
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}
