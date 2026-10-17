//! Run modes of the binary: `serve` (the web process), `task run <id>` (the
//! host runner) and the internal namespace mode every host command of the web
//! container goes through ([`host::INTERNAL_NSENTER_ARG`]; it is not part of
//! the usage text).

use std::ffi::{OsStr, OsString};
use std::sync::Arc;

use truenas_nvidia_driver_manager::{host, runner, settings, web};

fn usage() -> std::process::ExitCode {
    eprintln!("usage: truenas-nvidia-driver-manager [serve]");
    eprintln!("       truenas-nvidia-driver-manager task run <task id>");
    std::process::ExitCode::from(2)
}

fn main() -> std::process::ExitCode {
    // The internal namespace mode of the web container: it runs before any
    // thread or runtime exists and before the UTF-8 argument vector of the
    // public modes is read, so a host command's arguments pass through byte for
    // byte. The mode is the first argument, like every public verb.
    let mut argv = std::env::args_os();
    let _ = argv.next(); // argv[0], the executable path
    if argv.next().as_deref() == Some(OsStr::new(host::INTERNAL_NSENTER_ARG)) {
        // The only accepted form is the one `host::build_command` produces:
        //   <exe> <INTERNAL_NSENTER_ARG> <program> [args…]
        // A call without a program is a usage error like any other; the
        // argument itself never appears in the usage text.
        let Some(target) = argv.next() else {
            return usage();
        };
        let args: Vec<OsString> = argv.collect();
        return host::enter_host_namespaces(&target, &args);
    }
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        None | Some("serve") => {
            let settings = settings::load(std::env::vars_os());
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("could not start the runtime: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            match runtime.block_on(web::serve(
                settings,
                Arc::new(host::RealHost::in_container()),
            )) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("the web process stopped: {e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        Some("task") => {
            if args.get(2).map(|s| s.as_str()) == Some("run") && args.len() == 4 {
                runner::run(&args[3])
            } else {
                usage()
            }
        }
        Some(_) => usage(),
    }
}
