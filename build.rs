use std::fs::File;
use std::io::Write;

fn main() {
    println!("cargo::rerun-if-env-changed=LINEAR_CLIENT_ID");
    let client_id = std::env::var("LINEAR_CLIENT_ID")
        .ok()
        .filter(|id| !id.trim().is_empty());
    if client_id.is_none() {
        println!(
            "cargo::warning=LINEAR_CLIENT_ID not set; the plugin offers only API key connections."
        );
    }
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set");
    let mut file = File::create(std::path::Path::new(&out_dir).join("oauth_client.rs"))
        .expect("failed to create generated OAuth client id");
    writeln!(file, "const BUILT_CLIENT_ID: Option<&str> = {client_id:?};").unwrap();
}
