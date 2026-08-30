use std::io;

fn main() {
    let arguments = std::env::args_os()
        .skip(1)
        .collect::<Vec<std::ffi::OsString>>();
    let code = if wayfinder_cli::is_serve_command(&arguments) {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("wayfinder-router: cannot start async runtime: {error}");
                std::process::exit(wayfinder_cli::EXIT_CONFIG);
            }
        };
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        runtime.block_on(wayfinder_cli::run_serve_process(
            &arguments[1..],
            &mut stdout,
            &mut stderr,
        ))
    } else if wayfinder_cli::is_exec_command(&arguments) {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        wayfinder_cli::run_exec_process(&arguments[1..], &mut stdout, &mut stderr)
    } else {
        let mut stdin = io::stdin().lock();
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        wayfinder_cli::run(arguments, &mut stdin, &mut stdout, &mut stderr)
    };
    std::process::exit(code);
}
