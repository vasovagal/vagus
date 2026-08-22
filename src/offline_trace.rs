//! Compile-time-optional, privacy-projected tracing helpers (ADR 0029).
//!
//! Every constructor below names one immutable schema-v1 catalogue operation and exposes only its
//! reviewed low-cardinality fields. In particular, this module has no API that accepts queries,
//! paths, note/model content, prompts, hashes, raw errors, or arbitrary debug/display values.

use std::marker::PhantomData;
#[cfg(feature = "generate")]
use std::thread::JoinHandle;

/// A catalogue span. Without `local-tracing` this is a zero-sized no-op, so instrumentation call
/// sites stay type-checked while the tracing crates, activation reads, and output paths are omitted.
#[derive(Clone)]
pub(crate) struct Span {
    #[cfg(feature = "local-tracing")]
    inner: tracing::Span,
}

/// RAII entry guard returned by [`Span::enter`].
pub(crate) struct Entered<'a> {
    #[cfg(feature = "local-tracing")]
    inner: tracing::span::Entered<'a>,
    marker: PhantomData<&'a Span>,
}

/// Marks an unfinished long-lived phase as a reviewed error when `?`/unwind exits its scope.
pub(crate) struct OutcomeGuard<'a> {
    span: &'a Span,
    error_code: ErrorCode,
    finished: bool,
}

impl Span {
    #[cfg(feature = "local-tracing")]
    fn new(inner: tracing::Span) -> Self {
        Self { inner }
    }

    #[cfg(not(feature = "local-tracing"))]
    const fn noop() -> Self {
        Self {}
    }

    /// Enter this span only around active work. Callers must not retain the guard while idle.
    pub(crate) fn enter(&self) -> Entered<'_> {
        Entered {
            #[cfg(feature = "local-tracing")]
            inner: self.inner.enter(),
            marker: PhantomData,
        }
    }

    #[cfg(feature = "local-tracing")]
    fn tracing_span(&self) -> &tracing::Span {
        &self.inner
    }

    /// Fail closed if a long-lived phase leaves scope before the caller marks it complete.
    pub(crate) fn error_on_drop(&self, error_code: ErrorCode) -> OutcomeGuard<'_> {
        OutcomeGuard {
            span: self,
            error_code,
            finished: false,
        }
    }

    /// Execute one active portion and project only a reviewed error class on failure.
    pub(crate) fn in_scope<T, E>(
        &self,
        error_code: ErrorCode,
        work: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let result = {
            let _entered = self.enter();
            work()
        };
        self.finish_result(&result, error_code);
        result
    }

    /// Mark a fallible operation complete without observing or formatting its error value.
    pub(crate) fn finish_result<T, E>(
        &self,
        result: &std::result::Result<T, E>,
        error_code: ErrorCode,
    ) {
        match result {
            Ok(_) => self.outcome("ok"),
            Err(_) => {
                self.outcome("error");
                self.error_code(error_code.as_str());
            }
        }
    }

    pub(crate) fn ok(&self) {
        self.outcome("ok");
    }

    pub(crate) fn skipped(&self) {
        self.outcome("skipped");
    }

    pub(crate) fn fallback(&self) {
        self.outcome("fallback");
    }

    fn outcome(&self, value: &'static str) {
        #[cfg(feature = "local-tracing")]
        self.inner.record("outcome", value);
        #[cfg(not(feature = "local-tracing"))]
        let _ = value;
    }

    fn error_code(&self, value: &'static str) {
        #[cfg(feature = "local-tracing")]
        self.inner.record("error_code", value);
        #[cfg(not(feature = "local-tracing"))]
        let _ = value;
    }

    fn count(&self, field: &'static str, value: usize, maximum: u64) {
        let value = u64::try_from(value).unwrap_or(u64::MAX).min(maximum);
        #[cfg(feature = "local-tracing")]
        self.inner.record(field, value);
        #[cfg(not(feature = "local-tracing"))]
        let _ = (field, value);
    }

    pub(crate) fn item_count(&self, value: usize) {
        self.count("item_count", value, 1_000_000_000);
    }

    pub(crate) fn document_count(&self, value: usize) {
        self.count("document_count", value, 1_000_000_000);
    }

    pub(crate) fn chunk_count(&self, value: usize) {
        self.count("chunk_count", value, 4_000_000_000);
    }

    pub(crate) fn candidate_count(&self, value: usize) {
        self.count("candidate_count", value, 1_000_000_000);
    }

    pub(crate) fn result_count(&self, value: usize) {
        self.count("result_count", value, 1_000_000_000);
    }

    pub(crate) fn index_kind(&self, value: &'static str) {
        #[cfg(feature = "local-tracing")]
        self.inner.record("index_kind", value);
        #[cfg(not(feature = "local-tracing"))]
        let _ = value;
    }

    #[cfg_attr(not(feature = "local-tracing"), allow(dead_code))]
    fn dimensions(&self, value: usize) {
        self.count("dimensions", value, 1_048_576);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // The contained tracing span closes after this hook; the no-feature representation keeps the
        // same explicit lifecycle semantics for strict feature-matrix linting.
    }
}

impl OutcomeGuard<'_> {
    pub(crate) fn ok(mut self) {
        self.span.ok();
        self.finished = true;
    }

    pub(crate) fn skipped(mut self) {
        self.span.skipped();
        self.finished = true;
    }
}

impl Drop for OutcomeGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.span.outcome("error");
            self.span.error_code(self.error_code.as_str());
        }
    }
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "local-tracing")]
        {
            // Read the field so strict dead-code checks know it exists solely for RAII drop order.
            let _ = &self.inner;
        }
    }
}

/// Reviewed schema-v1 error classes. Raw `anyhow`/I/O/model errors never cross this boundary.
#[derive(Clone, Copy)]
pub(crate) enum ErrorCode {
    Configuration,
    Storage,
    ModelUnavailable,
    #[cfg(feature = "generate")]
    DecodeFailed,
    BackendUnavailable,
    Other,
}

impl ErrorCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Storage => "storage",
            Self::ModelUnavailable => "model_unavailable",
            #[cfg(feature = "generate")]
            Self::DecodeFailed => "decode_failed",
            Self::BackendUnavailable => "backend_unavailable",
            Self::Other => "other",
        }
    }
}

pub(crate) fn command(command: &'static str) -> Span {
    #[cfg(feature = "local-tracing")]
    return Span::new(tracing::span!(
        target: vasovagal_tracing::TARGET,
        tracing::Level::INFO,
        "vagus.command",
        command,
        outcome = tracing::field::Empty,
        error_code = tracing::field::Empty,
    ));
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = command;
        Span::noop()
    }
}

pub(crate) fn config_load(parent: &Span) -> Span {
    simple_span(parent, SimpleOperation::ConfigLoad)
}

pub(crate) fn storage_validate(parent: &Span) -> Span {
    simple_span(parent, SimpleOperation::StorageValidate)
}

pub(crate) fn index(index_kind: &'static str) -> Span {
    #[cfg(feature = "local-tracing")]
    return Span::new(tracing::span!(
        target: vasovagal_tracing::TARGET,
        tracing::Level::INFO,
        "vagus.index",
        index_kind,
        document_count = tracing::field::Empty,
        chunk_count = tracing::field::Empty,
        item_count = tracing::field::Empty,
        outcome = tracing::field::Empty,
        error_code = tracing::field::Empty,
    ));
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = index_kind;
        Span::noop()
    }
}

pub(crate) fn index_snapshot(parent: &Span, index_kind: &'static str) -> Span {
    index_phase(parent, IndexOperation::Snapshot, index_kind)
}

pub(crate) fn index_reconcile(parent: &Span, index_kind: &'static str) -> Span {
    index_phase(parent, IndexOperation::Reconcile, index_kind)
}

pub(crate) fn index_embed(parent: &Span) -> Span {
    #[cfg(feature = "local-tracing")]
    {
        let span = Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(),
            tracing::Level::INFO,
            "vagus.index.embed",
            engine = "fastembed",
            model_family = "embedding",
            dimensions = tracing::field::Empty,
            item_count = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
            outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        ));
        span.dimensions(crate::config::EMBED_DIMS);
        span
    }
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = parent;
        Span::noop()
    }
}

pub(crate) fn index_lexical_commit(parent: &Span, index_kind: &'static str) -> Span {
    index_phase(parent, IndexOperation::LexicalCommit, index_kind)
}

pub(crate) fn index_vector_persist(parent: &Span, index_kind: &'static str) -> Span {
    index_phase(parent, IndexOperation::VectorPersist, index_kind)
}

#[derive(Clone, Copy)]
enum IndexOperation {
    Snapshot,
    Reconcile,
    LexicalCommit,
    VectorPersist,
}

fn index_phase(parent: &Span, operation: IndexOperation, index_kind: &'static str) -> Span {
    #[cfg(feature = "local-tracing")]
    return match operation {
        IndexOperation::Snapshot => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(),
            tracing::Level::INFO,
            "vagus.index.snapshot",
            index_kind,
            document_count = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
            item_count = tracing::field::Empty,
            outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        IndexOperation::Reconcile => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(),
            tracing::Level::INFO,
            "vagus.index.reconcile",
            index_kind,
            document_count = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
            item_count = tracing::field::Empty,
            outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        IndexOperation::LexicalCommit => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(),
            tracing::Level::INFO,
            "vagus.index.lexical_commit",
            index_kind,
            document_count = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
            item_count = tracing::field::Empty,
            outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        IndexOperation::VectorPersist => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(),
            tracing::Level::INFO,
            "vagus.index.vector_persist",
            index_kind,
            document_count = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
            item_count = tracing::field::Empty,
            outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
    };
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (parent, operation, index_kind);
        Span::noop()
    }
}

pub(crate) fn search(
    search_mode: &'static str,
    exact: bool,
    smart: bool,
    rerank: bool,
    source_filter: &'static str,
) -> Span {
    #[cfg(feature = "local-tracing")]
    return Span::new(tracing::span!(
        target: vasovagal_tracing::TARGET,
        tracing::Level::INFO,
        "vagus.search",
        search_mode,
        exact,
        smart,
        rerank,
        source_filter,
        candidate_count = tracing::field::Empty,
        result_count = tracing::field::Empty,
        outcome = tracing::field::Empty,
        error_code = tracing::field::Empty,
    ));
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (search_mode, exact, smart, rerank, source_filter);
        Span::noop()
    }
}

pub(crate) fn search_refresh(parent: &Span, search_mode: &'static str) -> Span {
    search_phase(
        parent,
        SearchOperation::Refresh,
        search_mode,
        false,
        false,
        false,
        "all",
    )
}

pub(crate) fn search_scope(parent: &Span, search_mode: &'static str) -> Span {
    search_phase(
        parent,
        SearchOperation::Scope,
        search_mode,
        false,
        false,
        false,
        "all",
    )
}

pub(crate) fn search_retrieve(parent: &Span, search_mode: &'static str) -> Span {
    retrieve_phase(parent, RetrieveOperation::Retrieve, search_mode)
}

pub(crate) fn search_bm25(parent: &Span, search_mode: &'static str) -> Span {
    retrieve_phase(parent, RetrieveOperation::Bm25, search_mode)
}

pub(crate) fn search_vector(parent: &Span, search_mode: &'static str) -> Span {
    retrieve_phase(parent, RetrieveOperation::Vector, search_mode)
}

pub(crate) fn search_rrf(parent: &Span, search_mode: &'static str) -> Span {
    retrieve_phase(parent, RetrieveOperation::Rrf, search_mode)
}

pub(crate) fn search_hydrate(parent: &Span, search_mode: &'static str) -> Span {
    search_phase(
        parent,
        SearchOperation::Hydrate,
        search_mode,
        false,
        false,
        false,
        "all",
    )
}

#[cfg(feature = "generate")]
pub(crate) fn search_rewrite(parent: &Span, rerank: bool) -> Span {
    search_phase(
        parent,
        SearchOperation::Rewrite,
        "smart",
        false,
        true,
        rerank,
        "all",
    )
}

pub(crate) fn search_rerank(
    parent: &Span,
    search_mode: &'static str,
    exact: bool,
    smart: bool,
    rerank: bool,
) -> Span {
    search_phase(
        parent,
        SearchOperation::Rerank,
        search_mode,
        exact,
        smart,
        rerank,
        "all",
    )
}

pub(crate) fn search_postprocess(parent: &Span, search_mode: &'static str) -> Span {
    search_phase(
        parent,
        SearchOperation::Postprocess,
        search_mode,
        false,
        false,
        false,
        "all",
    )
}

#[derive(Clone, Copy)]
enum SearchOperation {
    Refresh,
    Scope,
    Hydrate,
    #[cfg(feature = "generate")]
    Rewrite,
    Rerank,
    Postprocess,
}

fn search_phase(
    parent: &Span,
    operation: SearchOperation,
    search_mode: &'static str,
    exact: bool,
    smart: bool,
    rerank: bool,
    source_filter: &'static str,
) -> Span {
    #[cfg(feature = "local-tracing")]
    return match operation {
        SearchOperation::Refresh => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.refresh",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        SearchOperation::Scope => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.scope",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        SearchOperation::Hydrate => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.hydrate",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        #[cfg(feature = "generate")]
        SearchOperation::Rewrite => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.rewrite",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        SearchOperation::Rerank => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.rerank",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        SearchOperation::Postprocess => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.postprocess",
            search_mode, exact, smart, rerank, source_filter,
            candidate_count = tracing::field::Empty, result_count = tracing::field::Empty,
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
    };
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (
            parent,
            operation,
            search_mode,
            exact,
            smart,
            rerank,
            source_filter,
        );
        Span::noop()
    }
}

#[derive(Clone, Copy)]
enum RetrieveOperation {
    Retrieve,
    Bm25,
    Vector,
    Rrf,
}

fn retrieve_phase(parent: &Span, operation: RetrieveOperation, search_mode: &'static str) -> Span {
    #[cfg(feature = "local-tracing")]
    return match operation {
        RetrieveOperation::Retrieve => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.retrieve",
            search_mode, candidate_count = tracing::field::Empty,
            result_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        RetrieveOperation::Bm25 => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.retrieve.bm25",
            search_mode, candidate_count = tracing::field::Empty,
            result_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        RetrieveOperation::Vector => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.retrieve.vector",
            search_mode, candidate_count = tracing::field::Empty,
            result_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        RetrieveOperation::Rrf => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.search.retrieve.rrf",
            search_mode, candidate_count = tracing::field::Empty,
            result_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
    };
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (parent, operation, search_mode);
        Span::noop()
    }
}

pub(crate) fn model_load(
    parent: &Span,
    engine: &'static str,
    model_family: &'static str,
    dimensions: Option<usize>,
) -> Span {
    model_span(
        parent,
        ModelOperation::Load,
        engine,
        model_family,
        dimensions,
    )
}

#[cfg(feature = "generate")]
pub(crate) fn model_decode(
    parent: &Span,
    engine: &'static str,
    model_family: &'static str,
) -> Span {
    model_span(parent, ModelOperation::Decode, engine, model_family, None)
}

pub(crate) fn model_infer(
    parent: &Span,
    engine: &'static str,
    model_family: &'static str,
    dimensions: Option<usize>,
) -> Span {
    model_span(
        parent,
        ModelOperation::Infer,
        engine,
        model_family,
        dimensions,
    )
}

#[derive(Clone, Copy)]
enum ModelOperation {
    Load,
    #[cfg(feature = "generate")]
    Decode,
    Infer,
}

fn model_span(
    parent: &Span,
    operation: ModelOperation,
    engine: &'static str,
    model_family: &'static str,
    dimensions: Option<usize>,
) -> Span {
    #[cfg(feature = "local-tracing")]
    let span = match operation {
        ModelOperation::Load => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.model.load",
            engine, model_family, dimensions = tracing::field::Empty,
            item_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        #[cfg(feature = "generate")]
        ModelOperation::Decode => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.model.decode",
            engine, model_family, dimensions = tracing::field::Empty,
            item_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
        ModelOperation::Infer => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.model.infer",
            engine, model_family, dimensions = tracing::field::Empty,
            item_count = tracing::field::Empty, outcome = tracing::field::Empty,
            error_code = tracing::field::Empty,
        )),
    };
    #[cfg(feature = "local-tracing")]
    {
        if let Some(dimensions) = dimensions {
            span.dimensions(dimensions);
        }
        span
    }
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (parent, operation, engine, model_family, dimensions);
        Span::noop()
    }
}

#[derive(Clone, Copy)]
enum SimpleOperation {
    ConfigLoad,
    StorageValidate,
}

fn simple_span(parent: &Span, operation: SimpleOperation) -> Span {
    #[cfg(feature = "local-tracing")]
    return match operation {
        SimpleOperation::ConfigLoad => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.config.load",
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
        SimpleOperation::StorageValidate => Span::new(tracing::span!(
            target: vasovagal_tracing::TARGET,
            parent: parent.tracing_span(), tracing::Level::INFO, "vagus.storage.validate",
            outcome = tracing::field::Empty, error_code = tracing::field::Empty,
        )),
    };
    #[cfg(not(feature = "local-tracing"))]
    {
        let _ = (parent, operation);
        Span::noop()
    }
}

/// Spawn bounded work with both the current dispatcher and an explicit cloned parent. The parent is
/// entered only inside the worker closure, never around a join or idle wait.
#[cfg(feature = "generate")]
pub(crate) fn spawn_with_parent<T, F>(parent: &Span, work: F) -> JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce(Span) -> T + Send + 'static,
{
    let parent = parent.clone();
    #[cfg(feature = "local-tracing")]
    {
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        std::thread::spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                let _entered = parent.enter();
                work(parent.clone())
            })
        })
    }
    #[cfg(not(feature = "local-tracing"))]
    {
        std::thread::spawn(move || {
            let _entered = parent.enter();
            work(parent.clone())
        })
    }
}

#[cfg(all(test, feature = "local-tracing", feature = "generate"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::Subscriber;
    use tracing::span::{Attributes, Id};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::{Layer, Registry};

    use super::*;

    type ParentRecords = Arc<Mutex<Vec<(&'static str, Option<&'static str>)>>>;
    type EnterRecords = Arc<Mutex<Vec<(&'static str, std::thread::ThreadId)>>>;

    #[derive(Clone)]
    struct Capture {
        parents: ParentRecords,
        enters: EnterRecords,
    }

    impl<S> Layer<S> for Capture
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
            let parent = context
                .span(id)
                .and_then(|span| span.parent())
                .map(|span| span.metadata().name());
            self.parents
                .lock()
                .unwrap()
                .push((attributes.metadata().name(), parent));
        }

        fn on_enter(&self, id: &Id, context: Context<'_, S>) {
            if let Some(span) = context.span(id) {
                self.enters
                    .lock()
                    .unwrap()
                    .push((span.metadata().name(), std::thread::current().id()));
            }
        }
    }

    #[test]
    fn smart_worker_propagates_dispatcher_and_enters_only_inside_explicit_parent() {
        let parents = Arc::new(Mutex::new(Vec::new()));
        let enters = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default().with(Capture {
            parents: Arc::clone(&parents),
            enters: Arc::clone(&enters),
        });
        let main_thread = std::thread::current().id();

        tracing::subscriber::with_default(subscriber, || {
            let search = search("smart", false, true, true, "all");
            let retrieve = search_retrieve(&search, "smart");
            let vector = search_vector(&retrieve, "smart");
            spawn_with_parent(&vector, |parent| {
                let load = model_load(&parent, "fastembed", "embedding", Some(768));
                load.in_scope(ErrorCode::ModelUnavailable, || Ok::<(), ()>(()))
                    .unwrap();
            })
            .join()
            .unwrap();
        });

        assert!(parents.lock().unwrap().iter().any(|(name, parent)| {
            *name == "vagus.model.load" && *parent == Some("vagus.search.retrieve.vector")
        }));
        let vector_enters: Vec<_> = enters
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| *name == "vagus.search.retrieve.vector")
            .map(|(_, thread)| *thread)
            .collect();
        assert_eq!(vector_enters.len(), 1);
        assert_ne!(vector_enters[0], main_thread);
    }
}
