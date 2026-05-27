mod credentials;
mod guards;
mod middleware;
mod principal;

pub use credentials::{
    AuthError, generate_token, hash_password, hash_token, verify_dummy_password, verify_password,
};
pub use guards::require_project_role;
pub(crate) use guards::{ensure_role, ensure_super_admin};
pub(crate) use middleware::require_auth;
pub use middleware::resolve_auth;
pub use principal::Principal;
