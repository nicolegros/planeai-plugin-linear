mod auth;
mod connect;
mod host;
mod linear_api;
mod model;
mod oauth;
mod plugin;
mod rpc;
mod session;
mod settings;
mod store;
mod sync;
#[cfg(test)]
mod testing;
mod writeback;

use plugin::LinearPlugin;
use std::time::Duration;
use tokio::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("linear plugin starting");
    let mut plugin = LinearPlugin::from_environment().map_err(std::io::Error::other)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let served = runtime.block_on(rpc::serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        &mut plugin,
    ));
    // Tokio reads stdin on a blocking thread that PlaneAI's open pipe never releases.
    runtime.shutdown_timeout(Duration::from_millis(200));
    eprintln!("linear plugin stopping");
    served?;
    Ok(())
}
