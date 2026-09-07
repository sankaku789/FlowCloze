use crate::http::HttpError;

/// 明示URLは1件、未指定時はOllamaからLM Studioの順で試す。
pub fn local_openai_url_candidates(explicit: Option<&str>) -> Vec<String> {
    match explicit.filter(|x| !x.trim().is_empty()) {
        Some(x) => vec![x.trim_end_matches('/').into()],
        None => vec![
            "http://127.0.0.1:11434/v1".into(),
            "http://127.0.0.1:1234/v1".into(),
        ],
    }
}

/// URL候補を順に試す。接続系以外の失敗は別endpointへ隠蔽しない。
pub fn try_local_openai_candidates<T>(
    explicit: Option<&str>,
    mut attempt: impl FnMut(&str) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    let candidates = local_openai_url_candidates(explicit);
    for (index, url) in candidates.iter().enumerate() {
        match attempt(url) {
            Ok(value) => return Ok(value),
            Err(HttpError::Transport | HttpError::Timeout) if index + 1 < candidates.len() => {}
            Err(error) => return Err(error),
        }
    }
    Err(HttpError::Transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_selection_only_falls_through_connection_errors() {
        let mut tried = Vec::new();
        let selected = try_local_openai_candidates(None, |url| {
            tried.push(url.to_string());
            if tried.len() == 1 {
                Err(HttpError::Transport)
            } else {
                Ok(url.to_string())
            }
        })
        .unwrap();
        assert_eq!(tried.len(), 2);
        assert_eq!(selected, "http://127.0.0.1:1234/v1");
    }

    #[test]
    fn custom_url_is_the_only_candidate() {
        assert_eq!(
            local_openai_url_candidates(Some("http://localhost:9999/v1/")),
            vec!["http://localhost:9999/v1".to_string()]
        );
    }
}
