//! A port of the app's own: haptic feedback, which only the platform can give. The core declares
//! the trait and calls it like any other port; each platform implements it its own way (Swift
//! from closures or a class, Kotlin an `object`, TypeScript an object literal) and registers it
//! with the rest of the adapters. The site's "Your own port" recipe quotes this file.

use undra::prelude::*;

/// A light tap, the confirmation of an action the user just took.
const LIGHT: u8 = 96;

// docs:begin haptics-port
/// A tap the user feels: the platform's haptics engine.
#[undra::port(sync)]
pub trait Haptics {
    /// One tap, `strength` from 0 (nothing) to 255 (the strongest the device has).
    fn tap(&self, strength: u8);
}
// docs:end

// docs:begin haptics-call
/// Confirms an action the user just took: one light tap through the [`Haptics`] port.
#[undra::api]
pub fn confirm(ctx: &Ctx) {
    haptics(ctx).tap(LIGHT);
}
// docs:end

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use undra::runtime::testing::TestRuntime;

    use super::*;

    /// A fake that keeps every tap.
    #[derive(Default)]
    struct Recorded(Mutex<Vec<u8>>);

    #[undra::port]
    impl Haptics for Recorded {
        fn tap(&self, strength: u8) {
            self.0.lock().unwrap().push(strength);
        }
    }

    #[test]
    fn a_confirmation_is_one_light_tap() {
        let t = TestRuntime::new();
        let taps = Arc::new(Recorded::default());
        t.runtime().bind_dyn_port::<dyn Haptics>(
            <dyn Haptics as undra::runtime::Port>::PORT_ID,
            taps.clone(),
        );
        confirm(&t.ctx());
        confirm(&t.ctx());
        assert_eq!(*taps.0.lock().unwrap(), vec![LIGHT, LIGHT]);
    }
}
