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
use tokio::io::BufReader;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("linear plugin starting");
    let mut plugin = LinearPlugin::from_environment().map_err(std::io::Error::other)?;
    rpc::serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        &mut plugin,
    )
    .await?;
    eprintln!("linear plugin stopping");
    Ok(())
}
