use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
}

impl User {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, crate::domain::errors::DomainError> {
        let id = id.into();
        let name = name.into();
        if id.trim().is_empty() {
            return Err(crate::domain::errors::DomainError::Validation(
                "id is empty".into(),
            ));
        }
        if id.len() > 64 {
            return Err(crate::domain::errors::DomainError::Validation(
                "id too long".into(),
            ));
        }
        if name.trim().is_empty() {
            return Err(crate::domain::errors::DomainError::Validation(
                "name is empty".into(),
            ));
        }
        if name.len() > 256 {
            return Err(crate::domain::errors::DomainError::Validation(
                "name too long".into(),
            ));
        }
        Ok(Self { id, name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_user_rejects_empty_name() {
        let res = User::new("u1", "");
        assert!(res.is_err());
    }

    #[test]
    fn new_user_rejects_empty_id() {
        let res = User::new("  ", "Alice");
        assert!(res.is_err());
    }

    #[test]
    fn new_user_rejects_id_too_long() {
        let id = "a".repeat(65);
        let res = User::new(id, "Alice");
        assert!(res.is_err());
    }

    #[test]
    fn new_user_rejects_name_too_long() {
        let name = "a".repeat(257);
        let res = User::new("u1", name);
        assert!(res.is_err());
    }
}
