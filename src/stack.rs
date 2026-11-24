//! The stack every trekr thread runs on.
//!
//! Prism's parser, trekr's walkers and prism's free all recurse on a tree's
//! depth, and a stack overflow aborts the process: one deep file in a checkout
//! killed the whole index. Prism caps its parser at 10,000 levels, but a
//! left-associative chain (`1 + 1 + …`) is built in a loop, so a tree is as
//! deep as its source makes it. Any thread may meet such a tree — a query
//! re-reads files on the main thread and on rayon's — so every thread gets the
//! same stack, and `clippy.toml` refuses the constructors that would not.

/// Reserved, not committed: a thread touches only the pages its deepest tree
/// needs, so this costs address space, not memory.
const STACK: usize = 256 << 20;

/// A thread builder with the stack.
#[allow(clippy::disallowed_methods, reason = "the one owner of thread stacks")]
fn builder() -> std::thread::Builder {
    std::thread::Builder::new().stack_size(STACK)
}

/// `std::thread::spawn`, on the stack.
pub(crate) fn spawn<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    builder().spawn(f).expect("spawn a thread")
}

/// `Scope::spawn`, on the stack.
pub(crate) fn spawn_scoped<'scope, 'env, F, T>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    f: F,
) -> std::thread::ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    builder().spawn_scoped(scope, f).expect("spawn a thread")
}

/// A rayon pool builder whose workers have the stack.
#[allow(clippy::disallowed_methods, reason = "the one owner of thread stacks")]
pub(crate) fn pool() -> rayon::ThreadPoolBuilder {
    rayon::ThreadPoolBuilder::new().stack_size(STACK)
}

/// Run `main` on a thread with the stack, rayon's global pool given it too.
/// The process's own main thread keeps the size the OS gave it.
pub(crate) fn run<T: Send>(main: impl FnOnce() -> T + Send) -> T {
    pool()
        .build_global()
        .expect("rayon's global pool is configured before anything uses it");
    std::thread::scope(|scope| {
        spawn_scoped(scope, main)
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
