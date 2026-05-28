#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Multi-call: socket-proxy / workload-launcher.
    let mut args = std::env::args_os();
    let argv0 = args.next();
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "socket-proxy")
    {
        denia::socket_proxy::run_from_args(args).await?;
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

    // Default (no subcommand for now; clap dispatch added in Task 6).
    denia::daemon::run().await
}
