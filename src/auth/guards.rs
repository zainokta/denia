use super::credentials::AuthError;
use super::principal::Principal;
use crate::api::ApiError;
use crate::app::AppState;
use crate::domain::Role;

pub(crate) fn ensure_role(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    min: Role,
) -> Result<(), ApiError> {
    if principal.is_super_admin {
        return Ok(());
    }
    let user_id = principal
        .user_id
        .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
    let role = state.users.role_for(user_id, project_id)?;
    require_project_role(principal, role, min).map_err(Into::into)
}

/// Like [`ensure_role`] but rewrites a `Forbidden` into `NotFound(not_found_msg)`
/// so a non-member cannot distinguish "exists but I can't see it" from "doesn't
/// exist" (existence-leak avoidance). Replaces the per-handler
/// `ensure_service_role` / `ensure_job_role` / `ensure_deployment_role`
/// wrappers that were byte-identical except for the message.
pub(crate) fn ensure_role_or_not_found(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    min: Role,
    not_found_msg: &str,
) -> Result<(), ApiError> {
    ensure_role(state, principal, project_id, min).map_err(|error| match error {
        ApiError::Forbidden(_) => ApiError::NotFound(not_found_msg.to_string()),
        other => other,
    })
}

pub(crate) fn ensure_super_admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.is_super_admin {
        Ok(())
    } else {
        Err(ApiError::Forbidden("super admin required".to_string()))
    }
}

pub fn require_project_role(
    principal: &Principal,
    project_role: Option<crate::domain::Role>,
    required_min: crate::domain::Role,
) -> Result<(), AuthError> {
    if principal.is_super_admin {
        return Ok(());
    }
    let role = project_role.ok_or(AuthError::Forbidden)?;
    if role >= required_min {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_project_role_super_admin_bypasses_role_check() {
        let principal = Principal::super_admin();
        assert!(require_project_role(&principal, None, crate::domain::Role::Admin).is_ok());
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Viewer),
                crate::domain::Role::Admin
            )
            .is_ok()
        );
    }

    #[test]
    fn require_project_role_forbids_lower_role() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(matches!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Operator),
                crate::domain::Role::Admin
            ),
            Err(AuthError::Forbidden)
        ));
        assert!(matches!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Viewer),
                crate::domain::Role::Operator
            ),
            Err(AuthError::Forbidden)
        ));
    }

    #[test]
    fn require_project_role_allows_equal_or_higher_role() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Admin),
                crate::domain::Role::Admin
            )
            .is_ok()
        );
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Admin),
                crate::domain::Role::Operator
            )
            .is_ok()
        );
    }

    #[test]
    fn require_project_role_forbids_non_member() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(matches!(
            require_project_role(&principal, None, crate::domain::Role::Viewer),
            Err(AuthError::Forbidden)
        ));
    }
}
