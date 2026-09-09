pub(crate) fn check(provider: &str) -> Result<(), String> {
    flowcloze::config::ensure_default_files()?;
    let (providers, _) =
        flowcloze::config::model_file::load_catalogs(&flowcloze::config::model_path()?)?;
    let definition = providers
        .get(provider)
        .ok_or_else(|| format!("unknown provider: {provider}"))?;
    let response = reqwest::blocking::Client::new()
        .get(&definition.base_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("{}: {e}", definition.base_url))?;
    if response.status().is_server_error() {
        return Err(format!(
            "{}: HTTP {}",
            definition.base_url,
            response.status()
        ));
    }
    println!("provider ok: {}", definition.base_url);
    Ok(())
}
