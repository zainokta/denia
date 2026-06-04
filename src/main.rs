use clap::Parser;

fn main() -> anyhow::Result<()> {
    // Multi-call (Linux-only): socket-proxy / workload-launcher run before
    // subcommand parsing so a symlink named `socket-proxy` never trips clap.
    // These are runtime-isolation helpers that exist only in the Linux server
    // build; on macOS/Windows `denia` is a client-only binary (ADR-036).
    #[cfg(target_os = "linux")]
    {
        let mut args = std::env::args_os();
        let argv0 = args.next();
        if argv0
            .as_ref()
            .and_then(|path| std::path::Path::new(path).file_name())
            .is_some_and(|name| name == "socket-proxy")
        {
            // socket_proxy::run_from_args is async; build a CURRENT-THREAD runtime so
            // the whole process is single-threaded. The proxy drops its capability
            // bounding set and (best-effort) installs seccomp inside `run`; capability
            // bounding-set drop is inherently per-thread, so a single-threaded runtime
            // is what makes that drop cover the entire process. A multi-thread runtime
            // would leave the proxy's other worker threads with their bounding set
            // intact and unfiltered. The proxy is I/O-bound (bidirectional copy), so a
            // current-thread runtime is sufficient. See M1 / ADR-005.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(denia::socket_proxy::run_from_args(args))?;
            return Ok(());
        }
        if argv0
            .as_ref()
            .and_then(|path| std::path::Path::new(path).file_name())
            .is_some_and(|name| name == "workload-launcher")
        {
            let code = denia::workload_launcher::run_from_args(args)?;
            std::process::exit(code);
        }
    }

    // Subcommand parsing + dispatch.
    let cli = denia::cli::Cli::parse();
    denia::cli::dispatch(cli)
}
