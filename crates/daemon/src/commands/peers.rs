//! Peers command - lists trusted peers.

use anyhow::Result;
use croc_gui_core::platform;

pub async fn execute() -> Result<()> {
    let peers_path = platform::peers_file_path();

    println!("Trusted Peers");
    println!("=============");

    if peers_path.exists() {
        let contents = std::fs::read_to_string(&peers_path)?;
        // TODO: Parse and display peers nicely
        println!("Peers file: {:?}", peers_path);
        println!("{}", contents);
    } else {
        println!("No trusted peers configured.");
        println!("Peers will be stored in: {:?}", peers_path);
    }

    Ok(())
}

