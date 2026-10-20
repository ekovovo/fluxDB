fn main() {
    println!("cargo:rerun-if-env-changed=FLUXDB_UPDATE_PUBLIC_KEY");
}
