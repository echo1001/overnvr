
use tokio;
use anyhow::Result;

use warp::Filter;

mod track;
mod packetizer;

mod ffi;
mod surface;
mod detector;
mod source;
mod dvr;
mod client;
mod model;

use tokio::io::AsyncReadExt;

#[global_allocator] 
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

pub use source::{Source, SourceLink};


#[tokio::main]
async fn main() -> Result<()> {
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

    dvr::DVR::new(config).await?.start().await?;

    Ok(())
}
