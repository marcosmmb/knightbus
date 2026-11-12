//! Thin in-process Rust host adapter. The stable foreign C ABI belongs to M12.
use knightbus_core::{
    CONTROL_VERSION, Caller, Command, ControlTarget, Error, KnightBusControl, Reply, Result,
};

/// Borrowed engine access with authority bound by the trusted embedding host.
/// Dropping a UI adapter does not implicitly tear down engine-owned leases.
pub struct InProcessControl<'a, T: ControlTarget> {
    target: &'a mut T,
    caller: Caller,
    subscriptions: std::collections::BTreeSet<u64>,
}
impl<'a, T: ControlTarget> InProcessControl<'a, T> {
    /// Negotiate the initial major version before exposing any engine operation.
    pub fn new(target: &'a mut T, caller: Caller, version: u16) -> Result<Self> {
        if version != CONTROL_VERSION {
            return Err(Error::Version);
        }
        Ok(Self {
            target,
            caller,
            subscriptions: Default::default(),
        })
    }
}
impl<T: ControlTarget> KnightBusControl for InProcessControl<'_, T> {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        if let Command::Poll(id) | Command::Unsubscribe(id) = command {
            if !self.subscriptions.contains(&id) {
                return Err(Error::Denied);
            }
        }
        let result = self.target.execute(self.caller, command.clone(), deadline);
        if let Ok(Reply::Subscription(id)) = &result {
            self.subscriptions.insert(*id);
        }
        if let (Command::Unsubscribe(id), Ok(_)) = (command, &result) {
            self.subscriptions.remove(&id);
        }
        result
    }
}

impl<T: ControlTarget> Drop for InProcessControl<'_, T> {
    fn drop(&mut self) {
        for id in &self.subscriptions {
            let _ = self
                .target
                .execute(self.caller, Command::Unsubscribe(*id), u64::MAX);
        }
    }
}
