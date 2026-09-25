#[derive(Debug, Clone, Copy)]
pub(super) enum Action {
    Ping,
    DaemonHealth,
    DaemonDoctor,
    DaemonDoctorRepair,
    Init,
    SandboxCreate,
    SandboxUpdate,
    SandboxStart,
    SandboxStop,
    SandboxPause,
    SandboxResume,
    SandboxStatus,
    SandboxDestroy,
    SandboxWipe,
    SandboxList,
    SandboxRemove,
    SandboxExecSetup,
    ProcessList,
    WorkspaceCreate,
    WorkspaceStart,
    WorkspaceStartMany,
    WorkspaceStop,
    WorkspaceDestroy,
    WorkspaceWipe,
    WorkspaceStatus,
    WorkspaceStats,
    WorkspaceStatsList,
    WorkspaceList,
    WorkspaceRemove,
    WorkspaceUpdate,
    WorkspaceResize,
    WorkspaceExec,
    WorkspaceCp,
    WorkspacePortPublish,
    WorkspacePortUnpublish,
    WorkspacePortList,
    WorkspaceRuntime,
    WorkspaceLogs,
    WorkspaceSnapshot,
    WorkspaceSnapshotList,
    WorkspaceRestore,
    WorkspaceSnapshotGc,
    WorkspaceSnapshotExport,
    WorkspaceSnapshotImport,
    RegistryRepair,
    PolicyGet,
    PolicySetDefault,
    PolicyAllow,
    PolicyDeny,
    PolicyClear,
    Shutdown,
}

impl Action {
    /// Whether this action changes lifecycle state and so has to be serialized
    /// against other actions on the same resources.
    pub(super) fn is_lifecycle(self) -> bool {
        self.is_sandbox_lifecycle() || self.is_workspace_lifecycle() || self.is_global_lifecycle()
    }

    /// Actions scoped to one sandbox, or to every workspace it holds.
    pub(super) fn is_sandbox_lifecycle(self) -> bool {
        matches!(
            self,
            Self::SandboxCreate
                | Self::SandboxUpdate
                | Self::SandboxStart
                | Self::SandboxStop
                | Self::SandboxPause
                | Self::SandboxResume
                | Self::SandboxDestroy
                | Self::SandboxRemove
                | Self::SandboxExecSetup
                // The bulk start touches every workspace it is given, so it is
                // treated as a sandbox-wide operation rather than one workspace.
                | Self::WorkspaceStartMany
        )
    }

    /// Actions scoped to one workspace.
    pub(super) fn is_workspace_lifecycle(self) -> bool {
        matches!(
            self,
            Self::WorkspaceCreate
                | Self::WorkspaceStart
                | Self::WorkspaceStop
                | Self::WorkspaceDestroy
                | Self::WorkspaceRemove
                | Self::WorkspaceUpdate
                | Self::WorkspaceResize
                | Self::WorkspaceRestore
                | Self::WorkspaceSnapshot
                | Self::WorkspaceSnapshotGc
                | Self::WorkspaceSnapshotExport
                | Self::WorkspaceSnapshotImport
                | Self::WorkspacePortPublish
                | Self::WorkspacePortUnpublish
        )
    }

    /// Actions that touch every sandbox, or the registry itself.
    pub(super) fn is_global_lifecycle(self) -> bool {
        matches!(
            self,
            Self::SandboxWipe
                | Self::WorkspaceWipe
                | Self::RegistryRepair
                | Self::DaemonDoctorRepair
        )
    }

    pub(super) fn parse(raw: &str) -> Result<Self> {
        let action = match raw {
            "ping" => Self::Ping,
            "daemon.health" => Self::DaemonHealth,
            "daemon.doctor" => Self::DaemonDoctor,
            "daemon.doctor.repair" => Self::DaemonDoctorRepair,
            "init" => Self::Init,
            "sandbox.create" => Self::SandboxCreate,
            "sandbox.update" => Self::SandboxUpdate,
            "sandbox.start" => Self::SandboxStart,
            "sandbox.stop" => Self::SandboxStop,
            "sandbox.pause" => Self::SandboxPause,
            "sandbox.resume" => Self::SandboxResume,
            "sandbox.status" => Self::SandboxStatus,
            "sandbox.destroy" => Self::SandboxDestroy,
            "sandbox.wipe" => Self::SandboxWipe,
            "sandbox.list" => Self::SandboxList,
            "sandbox.remove" => Self::SandboxRemove,
            "sandbox.exec_setup" => Self::SandboxExecSetup,
            "process.list" => Self::ProcessList,
            "workspace.create" => Self::WorkspaceCreate,
            "workspace.start" => Self::WorkspaceStart,
            "workspace.start_many" => Self::WorkspaceStartMany,
            "workspace.stop" => Self::WorkspaceStop,
            "workspace.destroy" => Self::WorkspaceDestroy,
            "workspace.wipe" => Self::WorkspaceWipe,
            "workspace.status" => Self::WorkspaceStatus,
            "workspace.stats" => Self::WorkspaceStats,
            "workspace.stats.list" => Self::WorkspaceStatsList,
            "workspace.list" => Self::WorkspaceList,
            "workspace.remove" => Self::WorkspaceRemove,
            "workspace.update" | "workspace.update_auth" => Self::WorkspaceUpdate,
            "workspace.resize" => Self::WorkspaceResize,
            "workspace.exec" => Self::WorkspaceExec,
            "workspace.cp" => Self::WorkspaceCp,
            "workspace.port.publish" => Self::WorkspacePortPublish,
            "workspace.port.unpublish" => Self::WorkspacePortUnpublish,
            "workspace.port.list" => Self::WorkspacePortList,
            "workspace.runtime" => Self::WorkspaceRuntime,
            "workspace.logs" => Self::WorkspaceLogs,
            "workspace.snapshot" => Self::WorkspaceSnapshot,
            "workspace.snapshot.list" => Self::WorkspaceSnapshotList,
            "workspace.restore" => Self::WorkspaceRestore,
            "workspace.snapshot.gc" => Self::WorkspaceSnapshotGc,
            "workspace.snapshot.export" => Self::WorkspaceSnapshotExport,
            "workspace.snapshot.import" => Self::WorkspaceSnapshotImport,
            "registry.repair" => Self::RegistryRepair,
            "policy.get" => Self::PolicyGet,
            "policy.set_default" => Self::PolicySetDefault,
            "policy.allow" => Self::PolicyAllow,
            "policy.deny" => Self::PolicyDeny,
            "policy.clear" => Self::PolicyClear,
            "shutdown" => Self::Shutdown,
            _ => {
                return Err(crate::error::coded(
                    crate::error::ErrorCode::InvalidRequest,
                    format!("unknown action '{}'", raw),
                ))
            }
        };
        Ok(action)
    }
}
use anyhow::Result;
