//! 平台身份层（realm）
//!
//! 独立于账套（`.fbk`）的一层全局存储，负责两件事：
//! 1. **全局账号** `realm_user`：能登录 Web 的"平台账号"。管理员与普通用户都是这里面的账号。
//! 2. **账套目录** `realm_book`：每个账套一条记录，记录 `key`（文件名）、`path`、`owner_username`（创建者）。
//!
//! 账套内部仍用 `.fbk` 自带的 `user` 表做授权；进入账套时由 `state::CurrentUser` 按本文件的
//! 归属关系做"授权 + 身份对账"（见 [`ensure_book_admin`]），从而完整复用账套引擎的权限/设备绑定逻辑。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use findb::{users, Db, DbError, DbResult};
use fincore::user::{burn_argon2, hash_password, verify_password, Role, User};
use rusqlite::{Connection, OptionalExtension};

/// 全局账号（平台层）
#[derive(Clone, Debug)]
pub struct RealmUser {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub must_change_pwd: bool,
    /// 绑定的设备指纹（Web 端"一人一机"）；空 = 尚未绑定，下次登录自动绑定
    pub device_id: String,
    pub created_at: String,
}

/// 账套目录项（平台层）
#[derive(Clone, Debug)]
pub struct RealmBook {
    pub id: i64,
    pub key: String,
    pub path: String,
    pub owner_username: String,
    pub company: String,
    pub created_at: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS realm_user (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    username        TEXT UNIQUE NOT NULL,
    display_name    TEXT NOT NULL DEFAULT '',
    password_hash   TEXT NOT NULL,
    is_admin        INTEGER NOT NULL DEFAULT 0,
    disabled        INTEGER NOT NULL DEFAULT 0,
    must_change_pwd INTEGER NOT NULL DEFAULT 0,
    device_id       TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS realm_book (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    key             TEXT UNIQUE NOT NULL,
    path            TEXT NOT NULL,
    owner_username  TEXT NOT NULL,
    company         TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT ''
);
"#;

/// 平台身份库（单进程内以 Mutex<Connection> 持有，WAL 模式下并发安全）
pub struct RealmDb {
    inner: Mutex<Connection>,
    path: PathBuf,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

impl RealmDb {
    /// 打开（必要时创建）平台身份库并初始化表结构
    pub fn open<P: AsRef<Path>>(path: P) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self {
            inner: Mutex::new(conn),
            path,
        };
        db.init()?;
        Ok(db)
    }

    /// 库文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init(&self) -> DbResult<()> {
        self.inner.lock().unwrap().execute_batch(SCHEMA)?;
        self.migrate()?;
        Ok(())
    }

    /// 轻量迁移：为早期创建的库补上后加的列（device_id）
    fn migrate(&self) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        let has: bool = conn
            .prepare("PRAGMA table_info(realm_user)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|c| c == "device_id");
        if !has {
            conn.execute(
                "ALTER TABLE realm_user ADD COLUMN device_id TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // 账号
    // ---------------------------------------------------------------

    pub fn count_users(&self) -> DbResult<i64> {
        let conn = self.inner.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM realm_user", [], |r| r.get(0))?)
    }

    /// 首次启动引导：若没有任何账号，则创建平台管理员。
    /// - `admin_pass` 非空：用给定口令创建（运维通过环境变量注入，推荐）。
    /// - 否则：自动生成强口令，调用方负责打印一次性凭据。
    /// 返回 `(username, password)`：若管理员已存在则返回 `None`。
    pub fn ensure_bootstrap(
        &self,
        admin_user: &str,
        admin_pass: &str,
        must_change: bool,
    ) -> DbResult<Option<(String, String)>> {
        if self.count_users()? > 0 {
            return Ok(None);
        }
        let (user, pass) = if admin_pass.is_empty() {
            let p = generate_password();
            (admin_user.to_string(), p)
        } else {
            (admin_user.to_string(), admin_pass.to_string())
        };
        self.create_user(&user, "平台管理员", &pass, true, must_change)?;
        Ok(Some((user, pass)))
    }

    pub fn create_user(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
        is_admin: bool,
        must_change: bool,
    ) -> DbResult<i64> {
        let username = username.trim().to_string();
        if username.is_empty() || password.len() < 6 {
            return Err(DbError::Fin(fincore::FinError::msg(
                "用户名不能为空，口令至少 6 位",
            )));
        }
        if self.get_user(&username)?.is_some() {
            return Err(DbError::Fin(fincore::FinError::msg("该用户名已存在")));
        }
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO realm_user(username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at)
             VALUES(?1,?2,?3,?4,0,?5,'',?6)",
            rusqlite::params![
                username,
                display_name,
                hash_password(password),
                is_admin as i64,
                must_change as i64,
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn authenticate(&self, username: &str, password: &str) -> DbResult<Option<RealmUser>> {
        match self.get_user(username)? {
            Some(u) => {
                if u.disabled {
                    return Ok(None);
                }
                if verify_password(password, &u.password_hash) {
                    Ok(Some(u))
                } else {
                    // 口令错误时做等价空校验，降低用户名枚举的时序差异
                    let _ = burn_argon2(password);
                    Ok(None)
                }
            }
            None => {
                // 用户不存在也做等价空校验：真实执行一次 argon2，耗时与真实校验相当
                let _ = burn_argon2(password);
                Ok(None)
            }
        }
    }

    pub fn get_user(&self, username: &str) -> DbResult<Option<RealmUser>> {
        let conn = self.inner.lock().unwrap();
        conn.query_row(
            "SELECT id,username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at
             FROM realm_user WHERE username=?1",
            rusqlite::params![username],
            map_user,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_users(&self) -> DbResult<Vec<RealmUser>> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at
             FROM realm_user ORDER BY id",
        )?;
        let rows = stmt.query_map([], map_user)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 仅更新展示名 / 停用状态 / 是否管理员（不改口令）
    pub fn update_user(
        &self,
        username: &str,
        display_name: Option<&str>,
        disabled: Option<bool>,
        is_admin: Option<bool>,
    ) -> DbResult<()> {
        let mut u = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("平台账号不存在"))?;
        if let Some(d) = display_name {
            u.display_name = d.to_string();
        }
        if let Some(d) = disabled {
            u.disabled = d;
        }
        if let Some(a) = is_admin {
            // 不允许把最后一个管理员改成非管理员，否则无人能管理系统
            if u.is_admin && !a {
                let admins = self
                    .list_users()?
                    .into_iter()
                    .filter(|x| x.is_admin)
                    .count();
                if admins <= 1 {
                    return Err(DbError::Fin(fincore::FinError::msg(
                        "至少保留一个平台管理员账号",
                    )));
                }
            }
            u.is_admin = a;
        }
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET display_name=?2,disabled=?3,is_admin=?4 WHERE username=?1",
            rusqlite::params![username, u.display_name, u.disabled as i64, u.is_admin as i64],
        )?;
        Ok(())
    }

    /// 删除平台账号。拒绝删除最后一个管理员，避免出现无人能管理系统的状态。
    pub fn delete_user(&self, username: &str) -> DbResult<()> {
        let target = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("平台账号不存在"))?;
        if target.is_admin {
            let admins = self.list_users()?.into_iter().filter(|u| u.is_admin).count();
            if admins <= 1 {
                return Err(DbError::Fin(fincore::FinError::msg(
                    "至少保留一个平台管理员账号",
                )));
            }
        }
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "DELETE FROM realm_user WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }

    pub fn reset_password(&self, username: &str, new: &str) -> DbResult<()> {
        if new.len() < 6 {
            return Err(DbError::Fin(fincore::FinError::msg("新口令至少 6 位")));
        }
        let conn = self.inner.lock().unwrap();
        // 管理员重置的口令是管理员已知的口令，必须强制用户下次登录自行改一次，
        // 否则口令会长期停留在管理员设定值上，失去重置的意义。
        conn.execute(
            "UPDATE realm_user SET password_hash=?2,must_change_pwd=1 WHERE username=?1",
            rusqlite::params![username, hash_password(new)],
        )?;
        Ok(())
    }

    /// 设备绑定（Web 端"一人一机"）：首次登录自动绑定当前设备。
    /// 单次持锁内完成"读-判-写"，并用条件 UPDATE 兜底，
    /// 避免两个设备几乎同时首次登录时互相覆盖绑定。
    /// 返回 Result：Ok = 绑定成功或无绑定；Err(msg) = 该账号已绑定其它设备。
    pub fn bind_device(&self, username: &str, device_id: &str) -> DbResult<Result<(), String>> {
        let conn = self.inner.lock().unwrap();
        let cur: Option<String> = conn
            .query_row(
                "SELECT device_id FROM realm_user WHERE username=?1",
                rusqlite::params![username],
                |r| r.get(0),
            )
            .optional()?;
        let cur = match cur {
            Some(c) => c,
            None => {
                return Err(DbError::Fin(fincore::FinError::msg("平台账号不存在")));
            }
        };
        if !cur.is_empty() {
            return if cur == device_id {
                Ok(Ok(()))
            } else {
                Ok(Err("该账号已绑定其它设备，如需更换请联系管理员重置设备".to_string()))
            };
        }
        // 条件更新：仅当仍为空时写入，并发首登不会互相覆盖
        let n = conn.execute(
            "UPDATE realm_user SET device_id=?2 WHERE username=?1 AND device_id=''",
            rusqlite::params![username, device_id],
        )?;
        Ok(if n == 1 {
            Ok(())
        } else {
            // 竞态下另一请求抢先绑定：回读最新值判定
            let now: String = conn.query_row(
                "SELECT device_id FROM realm_user WHERE username=?1",
                rusqlite::params![username],
                |r| r.get(0),
            )?;
            if now == device_id {
                Ok(())
            } else {
                Err("该账号已绑定其它设备，如需更换请联系管理员重置设备".to_string())
            }
        })
    }

    /// 解绑设备（管理员「重置设备」）：清空后该账号下次登录自动重新绑定
    pub fn clear_device(&self, username: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET device_id='' WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }

    /// 修改自身口令（需校验旧口令）
    pub fn change_password(&self, username: &str, old: &str, new: &str) -> DbResult<Result<(), String>> {
        let u = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("平台账号不存在"))?;
        if !verify_password(old, &u.password_hash) {
            return Ok(Err("原口令不正确".to_string()));
        }
        if new.len() < 6 {
            return Ok(Err("新口令至少 6 位".to_string()));
        }
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET password_hash=?2,must_change_pwd=0 WHERE username=?1",
            rusqlite::params![username, hash_password(new)],
        )?;
        Ok(Ok(()))
    }

    // ---------------------------------------------------------------
    // 账套目录
    // ---------------------------------------------------------------

    pub fn register_book(
        &self,
        key: &str,
        path: &str,
        owner_username: &str,
        company: &str,
    ) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO realm_book(key,path,owner_username,company,created_at) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![key, path, owner_username, company, now()],
        )?;
        Ok(())
    }

    pub fn get_book(&self, key: &str) -> DbResult<Option<RealmBook>> {
        let conn = self.inner.lock().unwrap();
        conn.query_row(
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book WHERE key=?1",
            rusqlite::params![key],
            map_book,
        )
        .optional()
        .map_err(Into::into)
    }

    /// 按归属返回账套列表：管理员返回全部，普通用户只返回自己创建的
    pub fn list_books_for(&self, username: &str, is_admin: bool) -> DbResult<Vec<RealmBook>> {
        let conn = self.inner.lock().unwrap();
        let sql: &str = if is_admin {
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book ORDER BY id"
        } else {
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book WHERE owner_username=?1 ORDER BY id"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = if is_admin {
            stmt.query_map([], map_book)?
        } else {
            stmt.query_map(rusqlite::params![username], map_book)?
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn book_owner(&self, key: &str) -> DbResult<Option<String>> {
        let conn = self.inner.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT owner_username FROM realm_book WHERE key=?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_book(&self, key: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        conn.execute("DELETE FROM realm_book WHERE key=?1", rusqlite::params![key])?;
        Ok(())
    }

    pub fn count_books_of(&self, username: &str) -> DbResult<i64> {
        let conn = self.inner.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM realm_book WHERE owner_username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?)
    }

    /// 启动时把目录全量载入（返回 path，供注册进 BookRegistry）
    pub fn load_all_book_paths(&self) -> DbResult<Vec<PathBuf>> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare("SELECT path FROM realm_book ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(PathBuf::from).collect())
    }

    /// 把某用户所有账套内的同名用户行的口令哈希同步为最新（改平台口令后保持一致）
    pub fn sync_password_to_books(&self, books_dir: &Path, username: &str, new_hash: &str) -> DbResult<()> {
        let books = self.list_books_for(username, false)?;
        for b in books {
            let p = PathBuf::from(&b.path);
            if !p.exists() {
                continue;
            }
            // 仅当账套目录在指定目录下（防越权改到其他位置的库）
            if let Some(dir) = books_dir.canonicalize().ok() {
                if let Some(pc) = p.canonicalize().ok() {
                    if !pc.starts_with(&dir) {
                        continue;
                    }
                }
            }
            if let Ok(db) = Db::open(&p) {
                if let Ok(Some(mut u)) = users::get(&db, username) {
                    u.password_hash = new_hash.to_string();
                    let _ = users::update(&db, &u);
                }
            }
        }
        Ok(())
    }
}

fn map_user(r: &rusqlite::Row) -> rusqlite::Result<RealmUser> {
    Ok(RealmUser {
        id: r.get(0)?,
        username: r.get(1)?,
        display_name: r.get(2)?,
        password_hash: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        must_change_pwd: r.get::<_, i64>(6)? != 0,
        device_id: r.get(7)?,
        created_at: r.get(8)?,
    })
}

fn map_book(r: &rusqlite::Row) -> rusqlite::Result<RealmBook> {
    Ok(RealmBook {
        id: r.get(0)?,
        key: r.get(1)?,
        path: r.get(2)?,
        owner_username: r.get(3)?,
        company: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// 生成 16 位强口令（大小写字母 + 数字）
fn generate_password() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// 身份对账：确保账套内存在该平台用户对应的 `user` 行（角色 Admin）。
///
/// - 普通用户自建账套时已被 [`crate::handlers::create_book`] 种子为 Admin；
/// - 平台管理员查看他人账套时，首次进入自动以其全局口令哈希种子一行 Admin。
/// 之后账套引擎的权限/数据范围/设备绑定逻辑对这行用户照常生效。
pub fn ensure_book_admin(db: &Db, ru: &RealmUser) -> DbResult<()> {
    if users::get(db, &ru.username)?.is_none() {
        let mut u = User::new(&ru.username, &ru.display_name, Role::Admin);
        u.password_hash = ru.password_hash.clone();
        u.must_change_pwd = false;
        users::insert(db, &u)?;
    }
    Ok(())
}
