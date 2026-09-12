//! Graceful Ctrl-C for index runs (ADR 0029).
//!
//! While an index run is active, the first SIGINT only raises a flag the run checks between files: it
//! finishes the current file, commits a checkpoint, and returns `index::Interrupted` with a resume
//! hint. A second SIGINT exits at once; the uncommitted batch is redone by the next run. Outside an
//! index run the handler restores the default disposition and re-raises, so Ctrl-C during the rest of
//! `vagus search` (or any other command) still terminates the process exactly as before.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Held for the duration of one index run.
pub struct Guard(());

impl Guard {
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            static INSTALL: std::sync::Once = std::sync::Once::new();
            INSTALL.call_once(install_handler);
        }
        if ACTIVE.fetch_add(1, Ordering::SeqCst) == 0 {
            REQUESTED.store(false, Ordering::SeqCst);
        }
        Guard(())
    }

    /// True once Ctrl-C has been pressed during this run.
    pub fn requested(&self) -> bool {
        REQUESTED.load(Ordering::SeqCst)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(unix)]
fn install_handler() {
    // SAFETY: a zeroed `sigaction` is a valid starting value, and `on_sigint` only touches atomics
    // and async-signal-safe libc calls. SA_RESTART keeps SQLite/tantivy I/O from seeing EINTR.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        let mut previous: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGINT, &action, &mut previous);
        // A parent that ignores SIGINT (a background job without job control, nohup) meant it:
        // put the ignore back rather than making the process killable by Ctrl-C.
        if previous.sa_sigaction == libc::SIG_IGN {
            libc::sigaction(libc::SIGINT, &previous, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
extern "C" fn on_sigint(_signal: libc::c_int) {
    fn say(message: &[u8]) {
        // SAFETY: write(2) is async-signal-safe; the buffer is a static byte string.
        unsafe {
            libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
        }
    }
    if ACTIVE.load(Ordering::SeqCst) == 0 {
        // Not indexing: behave exactly like the default disposition. The re-raised signal stays
        // pending until this handler returns, then terminates the process.
        // SAFETY: signal(2) and raise(3) are async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::raise(libc::SIGINT);
        }
        return;
    }
    if REQUESTED.swap(true, Ordering::SeqCst) {
        say(b"\nvagus: aborted; the next `vagus index` redoes the uncommitted batch\n");
        // SAFETY: _exit(2) is async-signal-safe and skips destructors, like the default action.
        unsafe { libc::_exit(130) };
    }
    say(b"\nvagus: stopping after the current file and committing progress (Ctrl-C again to abort)\n");
}
