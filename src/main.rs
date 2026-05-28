use clap::Parser;

fn main() -> anyhow::Result<()> {
    // Multi-call (existing): socket-proxy / workload-launcher run before
    // subcommand parsing so a symlink named `socket-proxy` never trips clap.
    let mut args = std::env::args_os();
    let argv0 = args.next();
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "socket-proxy")
    {
        // socket_proxy::run_from_args is async; build a small runtime just
        // for it (mirrors what #[tokio::main] used to do).
        let rt = tokio::runtime::Runtime::new()?;
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

    // Subcommand parsing + dispatch.
    let cli = denia::cli::Cli::parse();
    denia::cli::dispatch(cli)
}
