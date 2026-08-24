fn main() -> std::process::ExitCode {
    // Rust ignores SIGPIPE, so `trekr --refs each | head` panics on the first
    // write past the closed pipe instead of exiting quietly. Piping into `head`
    // is the most ordinary thing a caller does with a long result.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    trekr::cli::run()
}
