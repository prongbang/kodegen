use crate::domain::{DomainError, User, UserRepository};
use crate::infrastructure::diesel_db::{infra, Db};
use async_trait::async_trait;
use diesel::prelude::*;

diesel::table! {
    users (id) {
        id -> Text,
        name -> Text,
    }
}

#[derive(Insertable, Queryable)]
#[diesel(table_name = users)]
struct UserRow {
    id: String,
    name: String,
}

pub struct DieselUserRepository {
    db: Db,
}

impl DieselUserRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UserRepository for DieselUserRepository {
    async fn save(&self, user: User) -> Result<(), DomainError> {
        self.db
            .run(move |conn| {
                diesel::replace_into(users::table)
                    .values(UserRow {
                        id: user.id,
                        name: user.name,
                    })
                    .execute(conn)
                    .map_err(infra)?;
                Ok(())
            })
            .await
    }

    async fn get(&self, id: &str) -> Result<User, DomainError> {
        let id = id.to_string();
        self.db
            .run(move |conn| {
                users::table
                    .find(id)
                    .first::<UserRow>(conn)
                    .optional()
                    .map_err(infra)?
                    .map(|r| User {
                        id: r.id,
                        name: r.name,
                    })
                    .ok_or(DomainError::NotFound)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::diesel_db::build_pool;

    fn temp_repo(tag: &str) -> (DieselUserRepository, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "hexagonal-rs-diesel-test-{tag}-{}.db",
            std::process::id()
        ));
        let pool = build_pool(path.to_str().unwrap()).expect("pool init");
        (DieselUserRepository::new(Db::new(pool)), path)
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let (repo, path) = temp_repo("roundtrip");

        let user = User::new("u1", "Alice").expect("valid user");
        repo.save(user).await.expect("save");

        let got = repo.get("u1").await.expect("get");
        assert_eq!(got.name, "Alice");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn get_missing_is_not_found() {
        let (repo, path) = temp_repo("missing");

        let err = repo.get("nope").await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));

        let _ = std::fs::remove_file(&path);
    }
}
