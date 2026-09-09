//! Provider共通のstructured output設定。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputMode {
    Off,
    On,
    Auto,
}
