use super::credentials::AuthError;
use super::principal::Principal;

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
