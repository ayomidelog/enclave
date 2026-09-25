//! The operations that publish ports.
//!
//! A workspace publishes a set, so applying a set means withdrawing the old one and
//! binding the new one. The withdrawal happens first because a port the workspace is
//! changing from has to be free before the one it is changing to can be bound.
//!
//! Strict publication fails the caller when any port cannot be bound, and best effort
//! keeps the ports that worked and reports the rest, which is the difference between a
//! start and a reconcile.

use super::*;

impl PortPublisher {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PublisherState::default()),
            connections: Arc::new(ConnectionLimiter::new(MAX_PUBLISHED_CONNECTIONS)),
        }
    }

    /// The publisher state, recovered if a panic poisoned the mutex.
    ///
    /// A panic while the lock is held would otherwise poison it for the life of
    /// the daemon: every later publish, unpublish, and status call would panic in
    /// turn, and each panic takes down another request worker. The state behind
    /// the lock is two maps of owned listeners, which a panic cannot leave
    /// half-updated in a way that matters, so recovering is both safe and the
    /// difference between one failed request and a daemon that cannot serve ports
    /// again.
    pub(super) fn state(&self) -> std::sync::MutexGuard<'_, PublisherState> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn apply_workspace_ports_strict(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
    ) -> Result<Vec<PublishedPortStatus>> {
        self.apply_workspace_ports(
            sandbox_id,
            workspace_id,
            runtime_pid,
            workspace_ip,
            specs,
            PublishMode::Strict,
        )
    }

    pub fn reconcile_workspace_ports(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
    ) -> Result<Vec<PublishedPortStatus>> {
        self.apply_workspace_ports(
            sandbox_id,
            workspace_id,
            runtime_pid,
            workspace_ip,
            specs,
            PublishMode::BestEffort,
        )
    }

    pub fn clear_workspace_ports(&self, sandbox_id: &str, workspace_id: &str) {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let active = self.take_active_publications(&key);
        shutdown_publications(active);
    }

    pub(super) fn apply_workspace_ports(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
        mode: PublishMode,
    ) -> Result<Vec<PublishedPortStatus>> {
        validate_published_ports(specs)?;

        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let old_active = self.take_active_publications(&key);
        shutdown_publications(old_active);

        let mut active = Vec::new();
        let mut failures = Vec::new();

        for spec in specs {
            match ActivePublication::bind(
                spec.clone(),
                runtime_pid,
                workspace_ip,
                Arc::clone(&self.connections),
            ) {
                Ok(publication) => active.push(publication),
                Err(err) => match mode {
                    PublishMode::Strict => {
                        shutdown_publications(active);
                        self.clear_failed_statuses(&key);
                        return Err(err);
                    }
                    PublishMode::BestEffort => {
                        let message = err.to_string();
                        tracing::warn!(
                            "failed to republish {} for workspace {} in sandbox {}: {message}",
                            spec,
                            workspace_id,
                            sandbox_id
                        );
                        failures.push(PublishedPortStatus::failed(spec, message));
                    }
                },
            }
        }

        let statuses = active
            .iter()
            .map(ActivePublication::status)
            .chain(failures.iter().cloned())
            .collect::<Vec<_>>();
        self.store_workspace_state(key, active, failures);
        Ok(statuses)
    }

    pub(super) fn take_active_publications(
        &self,
        key: &WorkspacePublishKey,
    ) -> Vec<ActivePublication> {
        let mut state = self.state();
        let active = state.active.remove(key).unwrap_or_default();
        state.failed.remove(key);
        active
    }

    pub(super) fn clear_failed_statuses(&self, key: &WorkspacePublishKey) {
        let mut state = self.state();
        state.failed.remove(key);
    }

    pub(super) fn store_workspace_state(
        &self,
        key: WorkspacePublishKey,
        active: Vec<ActivePublication>,
        failures: Vec<PublishedPortStatus>,
    ) {
        let mut state = self.state();
        if active.is_empty() {
            state.active.remove(&key);
        } else {
            state.active.insert(key.clone(), active);
        }
        if failures.is_empty() {
            state.failed.remove(&key);
        } else {
            state.failed.insert(key, failures);
        }
    }
}
