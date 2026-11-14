//! `upstream`: start the private, local source control server.
//!
//! ```text
//! upstream [--port 7900] [--home <folder>]
//! ```

fn main() {
    let mut port: u16 = std::env::var("UPSTREAM_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7900);
    let mut home = rigspark_upstream::default_home();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or(port),
            "--home" => home = args.next().map(Into::into).unwrap_or(home),
            "-h" | "--help" => {
                println!("upstream [--port 7900] [--home <folder>]\nPrivate, local source control. Listens on 127.0.0.1 only.");
                return;
            }
            "-V" | "--version" => {
                println!("upstream {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(rigspark_upstream::serve(home, port)) {
        eprintln!("upstream: {e}");
        std::process::exit(1);
    }
}
