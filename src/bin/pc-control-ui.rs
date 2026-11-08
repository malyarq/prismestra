#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    let result = {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        pc_control::ui::windows::run(args)
    };
    #[cfg(windows)]
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("Native UI currently requires Windows");
        std::process::ExitCode::FAILURE
    }
}
