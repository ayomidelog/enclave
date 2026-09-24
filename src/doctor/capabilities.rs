use crate::sandbox::cgroup;

use super::DoctorCheck;

pub(crate) fn check_cgroup_v2_availability() -> DoctorCheck {
    let name = "cgroup_v2";
    if cgroup::is_cgroup_v2_available() {
        let controllers = cgroup::available_controllers();
        DoctorCheck::ok(
            name,
            &format!(
                "cgroup v2 available; controllers: {}",
                if controllers.is_empty() {
                    "none".to_string()
                } else {
                    controllers.join(", ")
                }
            ),
        )
    } else {
        DoctorCheck::warn(
            name,
            "cgroup v2 not available; resource limits will use rlimit only",
        )
    }
}
