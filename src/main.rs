fn main() {
    enclave::logging::init();
    if let Err(err) = enclave::commands::run() {
        tracing::error!("{err:#}");
        std::process::exit(enclave::commands::exit_code_of(&err));
    }
}
