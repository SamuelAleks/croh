//! Config command - view or modify configuration.

use anyhow::Result;
use croh_core::Config;

pub async fn execute(key: Option<String>, value: Option<String>) -> Result<()> {
    let mut config = Config::load_with_env()?;

    match (key.as_deref(), value) {
        (None, None) => {
            // Show all config
            println!("Current Configuration");
            println!("=====================");
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        (Some(key), None) => {
            // Get specific key
            match key {
                "download_dir" => println!("{:?}", config.download_dir),
                "default_relay" => println!("{:?}", config.default_relay),
                "theme" => println!("{:?}", config.theme),
                "croc_path" => println!("{:?}", config.croc_path),
                _ => println!("Unknown config key: {}", key),
            }
        }
        (Some(key), Some(value)) => {
            // Set specific key
            match key {
                "download_dir" => {
                    config.download_dir = value.into();
                    config.save()?;
                    println!("Set download_dir = {:?}", config.download_dir);
                }
                "default_relay" => {
                    config.default_relay = if value.is_empty() {
                        None
                    } else {
                        Some(value)
                    };
                    config.save()?;
                    println!("Set default_relay = {:?}", config.default_relay);
                }
                _ => println!("Cannot set config key: {}", key),
            }
        }
        (None, Some(_)) => {
            println!("Must specify a key to set a value");
        }
    }

    Ok(())
}

