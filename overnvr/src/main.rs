
use tokio;
use anyhow::Result;

mod track;
mod packetizer;

pub(crate) use overnvr_deepstream::{detector, surface};
mod source;
mod dvr;
mod client;
mod model;
mod database;
mod api;

use tokio::io::AsyncReadExt;

#[global_allocator] 
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

pub use source::{Source, SourceLink};
use std::env;


#[tokio::main]
async fn main() -> Result<()> {
    env::set_var("RUST_LOG", "actix_web=debug");
    gst::init()?;

    let file = if let Ok(f) = std::env::var("OVERNVR_CONFIG") {
        f
    } else {
        "config.toml".to_owned()
    };

    let mut config_file = tokio::fs::File::open(file).await?;
    let mut buf = Vec::new();
    config_file.read_to_end(&mut buf).await?;

    let config: model::Config = toml::from_slice(buf.as_slice())?;

    let dvr = dvr::DVR::new(config).await?;
    if let Err(e) = dvr.start().await {
        println!("{:?}", e);
    }

    api::start_server(dvr.database.clone(), dvr.clone()).await?;

    Ok(())
}
