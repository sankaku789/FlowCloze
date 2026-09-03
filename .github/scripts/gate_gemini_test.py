from pathlib import Path

p = Path('tests/gemini.rs')
text = p.read_text()
if not text.startswith('#![cfg(feature = "gemini-native")]'):
    p.write_text('#![cfg(feature = "gemini-native")]\n\n' + text)
