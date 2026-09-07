//! Provider共通のstructured output設定と、Gemini Native専用probe。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputMode {
    Off,
    On,
    Auto,
}

#[cfg(feature = "gemini-native")]
mod native_probe {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::{LockResult, Mutex, MutexGuard};

    const UNKNOWN: u8 = 0;
    const SUPPORTED: u8 = 1;
    const UNSUPPORTED: u8 = 2;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum CapabilityState {
        Unknown,
        Supported,
        Unsupported,
    }

    #[derive(Debug)]
    pub(crate) struct CapabilityProbe {
        state: AtomicU8,
        probe: Mutex<()>,
    }

    impl Default for CapabilityProbe {
        fn default() -> Self {
            Self {
                state: AtomicU8::new(UNKNOWN),
                probe: Mutex::new(()),
            }
        }
    }

    impl Clone for CapabilityProbe {
        fn clone(&self) -> Self {
            Self {
                state: AtomicU8::new(self.state.load(Ordering::Acquire)),
                probe: Mutex::new(()),
            }
        }
    }

    impl CapabilityProbe {
        pub(crate) fn state(&self) -> CapabilityState {
            match self.state.load(Ordering::Acquire) {
                SUPPORTED => CapabilityState::Supported,
                UNSUPPORTED => CapabilityState::Unsupported,
                _ => CapabilityState::Unknown,
            }
        }

        pub(crate) fn mark_supported(&self) {
            self.state.store(SUPPORTED, Ordering::Release);
        }

        pub(crate) fn mark_unsupported(&self) {
            self.state.store(UNSUPPORTED, Ordering::Release);
        }

        pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, ()>> {
            self.probe.lock()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cloned_probe_keeps_capability_state_but_not_lock_state() {
            let probe = CapabilityProbe::default();
            probe.mark_unsupported();
            let clone = probe.clone();
            assert_eq!(clone.state(), CapabilityState::Unsupported);
            assert!(clone.lock().is_ok());
        }
    }
}

#[cfg(feature = "gemini-native")]
pub(crate) use native_probe::{CapabilityProbe, CapabilityState};
