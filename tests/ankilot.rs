use flowcloze::{to_ankilot_csv, GeneratedDocument};

#[test]
fn 生成結果をankilotの2列csvに変換できる() {
    let document = serde_json::from_str::<GeneratedDocument>(
        r#"{
  "questions": [
    {
      "id": "qblock-001",
      "section": "要求定義",
      "type": "context-cloze",
      "question": "＿＿＿は，顧客が欲しいモノから＿＿＿をまとめる工程である。",
      "answers": ["要求定義", "要求仕様書"],
      "source_text": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である。",
      "explanation": "「欲しいモノ」を仕様へ落とし込む。",
      "tags": ["se", "requirements"],
      "warnings": []
    }
  ]
}"#,
    )
    .unwrap();

    assert_eq!(
        to_ankilot_csv(&document),
        "＿＿＿は，顧客が欲しいモノから＿＿＿をまとめる工程である。,\"要求定義\n要求仕様書\"\n"
    );
}

#[test]
fn csvの特殊文字をエスケープする() {
    let document = serde_json::from_str::<GeneratedDocument>(
        r#"{
  "questions": [
    {
      "id": "qblock-001",
      "type": "context-cloze",
      "question": "\"＿＿＿\", test",
      "answers": ["A\"B"]
    }
  ]
}"#,
    )
    .unwrap();

    assert_eq!(
        to_ankilot_csv(&document),
        "\"\"\"＿＿＿\"\", test\",\"A\"\"B\"\n"
    );
}
