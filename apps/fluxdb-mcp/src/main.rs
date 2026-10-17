fn main() {
    std::process::exit(fluxdb_mcp::run_cli(std::env::args().skip(1)));
}
