pub(crate) fn list() -> Result<(), String> {
    flowcloze::config::ensure_default_files()?;
    let (_, models) =
        flowcloze::config::model_file::load_catalogs(&flowcloze::config::model_path()?)?;
    let mut entries: Vec<_> = models.iter().collect();
    entries.sort_by_key(|(name, _)| *name);
    for (name, model) in entries {
        println!("{name}\t{}\t{}", model.provider, model.model);
    }
    Ok(())
}

pub(crate) fn add(name: &str, provider: &str, provider_model: &str) -> Result<(), String> {
    flowcloze::config::ensure_default_files()?;
    let path = flowcloze::config::model_path()?;
    let (providers, _) = flowcloze::config::model_file::load_catalogs(&path)?;
    if providers.get(provider).is_none() {
        return Err(format!("unknown provider: {provider}"));
    }
    flowcloze::config::model_file::upsert_model_yaml(&path, name, provider, provider_model)?;
    println!("{} を更新しました．", path.display());
    Ok(())
}
