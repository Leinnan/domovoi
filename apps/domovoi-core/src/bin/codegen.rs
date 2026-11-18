//! Generates the types shells use to talk to the core: Swift (`App` package)
//! for the Apple apps, Kotlin (`com.crayen.domovoi.app`) for Android.

use anyhow::{Result, bail};
use clap::Parser;
use crux_core::type_generation::facet::{Config, TypeRegistry};
use std::path::PathBuf;
use domovoi_core::delta::FfiApp;

#[derive(Parser)]
struct Args {
    /// Where the Swift `App` package goes.
    #[arg(short, long)]
    output_dir: Option<PathBuf>,
    /// Where the Kotlin sources (and their Gradle module) go.
    #[arg(long)]
    kotlin: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.output_dir.is_none() && args.kotlin.is_none() {
        bail!("pass --output-dir (Swift) and/or --kotlin");
    }
    let registry = TypeRegistry::new().register_app::<FfiApp>()?.build()?;
    if let Some(dir) = &args.output_dir {
        registry.swift(&Config::builder("App", dir).build())?;
    }
    if let Some(dir) = &args.kotlin {
        registry.kotlin(&Config::builder("com.crayen.domovoi.app", dir).build())?;
    }
    Ok(())
}
