use flowcloze::config::auth_store::AuthStore;

pub(crate) fn run(provider: &str) -> Result<(), String> {
    flowcloze::config::ensure_default_files()?;
    let (providers, _) =
        flowcloze::config::model_file::load_catalogs(&flowcloze::config::model_path()?)?;
    if providers.get(provider).is_none() {
        return Err(format!("unknown provider: {provider}"));
    }
    let api_key = rpassword::prompt_password("API key: ")
        .map_err(|e| format!("APIキーを読めませんでした: {e}"))?;
    let mut auth = AuthStore::load().map_err(|e| e.to_string())?;
    auth.set_api_key(provider, &api_key)
        .map_err(|e| e.to_string())?;
    auth.save().map_err(|e| e.to_string())?;
    println!(
        "{} を更新しました．",
        flowcloze::config::config_dir()?.join("auth.yaml").display()
    );
    Ok(())
}
