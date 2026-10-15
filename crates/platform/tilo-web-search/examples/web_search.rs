//! Try the engines from a terminal.
//!
//! `web_search <query>` searches with every engine and prints how each one
//! fared and the merged results; `web_search --fetch <url>` prints a page as
//! readable text.

use tilo_web_search::{Engine, FetchLimits, SearchLimits, client, fetch, search};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = client()?;
    if let [flag, url] = args.as_slice()
        && flag == "--fetch"
    {
        let page = runtime.block_on(fetch(&client, url, &FetchLimits::default()))?;
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    let query = args.join(" ");
    match runtime.block_on(search(
        &client,
        &query,
        &Engine::ALL,
        &SearchLimits::default(),
    )) {
        Ok(response) => println!("{}", serde_json::to_string_pretty(&response)?),
        Err(error) => println!("{error}"),
    }
    Ok(())
}
