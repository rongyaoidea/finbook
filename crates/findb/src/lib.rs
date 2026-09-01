//! # findb —— FinBook 持久化层
//!
//! 一个账套 = 一个 `.fbk` 文件（SQLite）。备份就是复制文件，恢复就是覆盖文件。
//!
//! 提供能力：
//! - [`Db::create`] 新建账套并灌入内置科目表、现金流量项目、常用摘要
//! - 科目、辅助档案、凭证、用户的增删改查
//! - 余额与账簿的实时聚合（[`balances::BalanceSnapshot`]）
//! - 期末结账 / 反结账
//! - 备份恢复、操作日志

pub mod accounts;
pub mod advanced;
pub mod attach;
pub mod automation;
pub mod assets;
pub mod auxs;
pub mod balances;
pub mod bank;
pub mod business;
pub mod imports;
pub mod invoices;
pub mod mgmt;
pub mod periods;
pub mod procurement;
pub mod security;
pub mod reports;
pub mod sales;
pub mod schema;
pub mod summaries;
pub mod settle;
pub mod template;
pub mod scm;
pub mod manufacturing;
pub mod stock;
pub mod users;
pub mod vouchers;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use fincore::chart::{
    default_accounts, default_settle_types, default_summaries, default_voucher_words,
};
use fincore::report::cashflow::{default_cash_flow_items, CashFlowDirection, CashFlowGroup};
use fincore::user::{AuditLog, User};
use fincore::{BookOptions, FinError};
use rusqlite::Connection;

pub use balances::BalanceSnapshot;

/// 账套文件默认扩展名
pub const BOOK_EXT: &str = "fbk";

/// 数据库错误
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite 错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("序列化错误：{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Fin(#[from] FinError),
}

impl From<DbError> for FinError {
    fn from(e: DbError) -> Self {
        FinError::db(e.to_string())
    }
}

/// 结果别名
pub type DbResult<T> = Result<T, DbError>;

/// 账套数据库连接
pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    /// 打开已有账套
    pub fn open<P: AsRef<Path>>(path: P) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(FinError::msg(format!("账套文件不存在：{}", path.display())).into());
        }
        let conn = Connection::open(&path)?;
        schema::init(&conn)?;
        Ok(Self { conn, path })
    }

    /// 新建账套。若文件已存在则报错，避免误覆盖。
    pub fn create<P: AsRef<Path>>(path: P, opts: &BookOptions) -> DbResult<Self> {
        Self::create_with(path, opts, true)
    }

    /// 新建账套但不内置管理员账号。
    ///
    /// 用于 Web 服务端「首次登录即管理员」的初始化流程：账套建好后用户表为空，
    /// 第一个成功登录的账号会被自动创建为系统管理员。
    pub fn create_no_admin<P: AsRef<Path>>(path: P, opts: &BookOptions) -> DbResult<Self> {
        Self::create_with(path, opts, false)
    }

    fn create_with<P: AsRef<Path>>(path: P, opts: &BookOptions, seed_admin: bool) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(FinError::msg(format!("文件已存在，未覆盖：{}", path.display())).into());
        }
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| FinError::io(e.to_string()))?;
            }
        }
        let conn = Connection::open(&path)?;
        schema::init(&conn)?;
        let db = Self { conn, path };
        db.seed_builtin(opts, seed_admin)?;
        Ok(db)
    }

    /// 打开内存数据库（测试 / 演示用）
    pub fn in_memory(opts: &BookOptions) -> DbResult<Self> {
        Self::in_memory_with(opts, true)
    }

    /// 打开内存数据库但不内置管理员（Web 初始化流程测试用）
    pub fn in_memory_no_admin(opts: &BookOptions) -> DbResult<Self> {
        Self::in_memory_with(opts, false)
    }

    fn in_memory_with(opts: &BookOptions, seed_admin: bool) -> DbResult<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        let db = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        db.seed_builtin(opts, seed_admin)?;
        Ok(db)
    }

    /// 灌入内置基础资料
    ///
    /// `seed_admin=false` 时跳过内置管理员账号，留给上层走「首次登录即管理员」流程。
    fn seed_builtin(&self, opts: &BookOptions, seed_admin: bool) -> DbResult<()> {
        for a in default_accounts() {
            accounts::insert(self, &a)?;
        }
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR REPLACE INTO cash_flow_item(code,name,grp,dir,disabled) VALUES(?1,?2,?3,?4,0)",
            )?;
            for it in default_cash_flow_items() {
                let grp = match it.group {
                    CashFlowGroup::Operating => "operating",
                    CashFlowGroup::Investing => "investing",
                    CashFlowGroup::Financing => "financing",
                };
                let dir = match it.dir {
                    CashFlowDirection::In => "in",
                    CashFlowDirection::Out => "out",
                };
                stmt.execute(rusqlite::params![it.code, it.name, grp, dir])?;
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO summary(text,use_count) VALUES(?1,0)")?;
            for s in default_summaries() {
                stmt.execute(rusqlite::params![s])?;
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO settle_type(name,sort) VALUES(?1,?2)")?;
            for (i, s) in default_settle_types().iter().enumerate() {
                stmt.execute(rusqlite::params![s, i as i64])?;
            }
        }
        if seed_admin {
            // 只内置一个管理员账号：首次登录即管理员，其余账号由管理员在
            // 「安全中心 → 用户管理」里开通（可设角色、口令与数据范围）。
            let mut u = User::new("admin", "系统管理员", fincore::Role::Admin);
            u.set_password("admin123");
            u.must_change_pwd = true;
            users::insert(self, &u)?;
        }
        reports::ensure_defaults(self)?;
        self.set_options(opts)?;
        Ok(())
    }

    #[inline]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // ---------------- meta ----------------
    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key=?1", rusqlite::params![key], |r| {
                r.get(0)
            })
            .ok()
    }
    pub fn meta_set(&self, key: &str, value: &str) -> DbResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// 账套参数
    pub fn options(&self) -> BookOptions {
        match self.meta_get("options") {
            Some(s) => serde_json::from_str(&s).unwrap_or_default(),
            None => BookOptions::default(),
        }
    }
    pub fn set_options(&self, o: &BookOptions) -> DbResult<()> {
        let s = serde_json::to_string(o)?;
        self.meta_set("options", &s)
    }

    /// 读取存在 meta 里的 JSON 配置（失败时返回 None）
    pub fn meta_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.meta_get(key)
            .and_then(|s| serde_json::from_str::<T>(&s).ok())
    }
    /// 写入 JSON 配置到 meta
    pub fn meta_set_json<T: serde::Serialize>(&self, key: &str, v: &T) -> DbResult<()> {
        let s = serde_json::to_string(v)?;
        self.meta_set(key, &s)
    }

    /// 可用凭证字
    pub fn voucher_words(&self) -> Vec<String> {
        let w = self.options().voucher_words;
        if w.is_empty() {
            default_voucher_words()
        } else {
            w
        }
    }

    // ---------------- 备份与维护 ----------------

    /// 备份账套（`VACUUM INTO` 产出的是已整理过的紧凑副本，可在软件运行时热备）
    pub fn backup<P: AsRef<Path>>(&self, to: P) -> DbResult<()> {
        let to = to.as_ref();
        if let Some(dir) = to.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| FinError::io(e.to_string()))?;
            }
        }
        let p = to.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{p}'"))
            .map_err(DbError::Sqlite)?;
        Ok(())
    }

    /// 自动备份：写入 `dir` 下以 `auto_` 开头、带时间戳的文件，
    /// 并只保留最近 `keep` 份（轮转删除最旧的），返回实际生成的文件路径。
    ///
    /// 命名 `auto_YYYYMMDD_HHMMSSmmm.fbk`（毫秒级防止同一秒冲突），
    /// 供 [`prune_auto_backups`] 识别。
    pub fn backup_auto<P: AsRef<Path>>(&self, dir: P, keep: usize) -> DbResult<PathBuf> {
        let dir = dir.as_ref().to_path_buf();
        let base = format!(
            "auto_{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S%3f")
        );
        // 毫秒级时间戳极少冲突；万一冲突（时钟回拨等）则追加序号
        let mut path = dir.join(format!("{base}.{BOOK_EXT}"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{base}_{n}.{BOOK_EXT}"));
            n += 1;
        }
        self.backup(&path)?;
        prune_auto_backups(&dir, keep)?;
        Ok(path)
    }

    /// 整理数据库文件（回收删除产生的空闲页）
    pub fn vacuum(&self) -> DbResult<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// 完整性检查
    pub fn integrity_check(&self) -> DbResult<Vec<String>> {
        let mut stmt = self.conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 统计信息：（凭证数, 分录数, 科目数）
    pub fn stats(&self) -> DbResult<(i64, i64, i64)> {
        let v: i64 = self.conn.query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM voucher_entry", [], |r| r.get(0))?;
        let a: i64 = self.conn.query_row("SELECT COUNT(*) FROM account", [], |r| r.get(0))?;
        Ok((v, e, a))
    }

    // ---------------- 日志 ----------------
    pub fn log(&self, user: &str, module: &str, action: &str, detail: &str) -> DbResult<()> {
        self.conn.execute(
            "INSERT INTO audit_log(ts,user,module,action,detail) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                user,
                module,
                action,
                detail
            ],
        )?;
        Ok(())
    }

    pub fn recent_logs(&self, limit: i64) -> DbResult<Vec<AuditLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,ts,user,module,action,detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit], map_log)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn search_logs(&self, keyword: &str, limit: i64) -> DbResult<Vec<AuditLog>> {
        let kw = format!("%{keyword}%");
        let mut stmt = self.conn.prepare(
            "SELECT id,ts,user,module,action,detail FROM audit_log
             WHERE user LIKE ?1 OR module LIKE ?1 OR action LIKE ?1 OR detail LIKE ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![kw, limit], map_log)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 清空全部业务数据，保留基础资料（用于重新建账 / 演示重置）
    pub fn clear_vouchers(&self) -> DbResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            "DELETE FROM voucher_entry;
             DELETE FROM voucher;
             DELETE FROM begin_balance;
             DELETE FROM period_state;",
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn map_log(r: &rusqlite::Row) -> rusqlite::Result<AuditLog> {
    Ok(AuditLog {
        id: r.get(0)?,
        ts: r.get(1)?,
        user: r.get(2)?,
        module: r.get(3)?,
        action: r.get(4)?,
        detail: r.get(5)?,
    })
}

/// 从行中读取金额（金额以 TEXT 存储）
pub fn read_money(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<fincore::Money> {
    let s: String = row.get(idx)?;
    Ok(fincore::Money::parse_or_zero(&s))
}

/// 从行中读取可空金额
pub fn read_money_opt(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<fincore::Money>> {
    let s: Option<String> = row.get(idx)?;
    Ok(s.map(|x| fincore::Money::parse_or_zero(&x)))
}

/// 从行中读取可空日期
pub fn read_date_opt(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<NaiveDate>> {
    let s: Option<String> = row.get(idx)?;
    match s {
        None => Ok(None),
        Some(s) => Ok(NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
    }
}

/// 金额写入参数：统一两位小数的十进制字符串
/// 读取账套里的一段 JSON 配置
pub fn options_json<T: serde::de::DeserializeOwned>(db: &Db, key: &str) -> Option<T> {
    db.meta_json::<T>(key)
}

/// 自动备份轮转：删除目录下 `auto_*.fbk` 中按修改时间排序最旧的，
/// 只保留最近 `keep` 份。`keep == 0` 表示不清理任何文件。
///
/// 只匹配 `auto_` 前缀的自动备份，绝不误删用户手工命名的备份。
pub fn prune_auto_backups(dir: &Path, keep: usize) -> DbResult<()> {
    if keep == 0 {
        return Ok(());
    }
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| FinError::io(e.to_string()))? {
        let entry = entry.map_err(|e| FinError::io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("auto_") || !name.ends_with(&format!(".{BOOK_EXT}")) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mt) = meta.modified() {
                files.push((mt, path));
            }
        }
    }
    if files.len() <= keep {
        return Ok(());
    }
    files.sort_by_key(|f| f.0); // 最旧的在前
    let excess = files.len() - keep;
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p); // 删除失败不致命，只留下孤儿文件
    }
    Ok(())
}

/// 写入一段 JSON 配置到账套
pub fn set_options_json<T: serde::Serialize>(db: &Db, key: &str, v: &T) -> DbResult<()> {
    db.meta_set_json(key, v)
}

#[inline]
pub fn money_param(m: fincore::Money) -> String {
    m.fmt_plain()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用账套：固定启用期间为 2026 年 1 月，避免依赖当前系统时间
    pub(crate) fn mem() -> Db {
        let mut o = BookOptions::default();
        o.start_period = fincore::Period::new(2026, 1).unwrap();
        Db::in_memory(&o).unwrap()
    }

    /// 测试用账套：不内置管理员，用于「首次登录即管理员」流程测试
    pub(crate) fn mem_no_admin() -> Db {
        let mut o = BookOptions::default();
        o.start_period = fincore::Period::new(2026, 1).unwrap();
        Db::in_memory_no_admin(&o).unwrap()
    }

    #[test]
    fn create_and_seed() {
        let db = mem();
        let (v, e, a) = db.stats().unwrap();
        assert_eq!((v, e), (0, 0));
        assert!(a > 100, "内置科目表应有一百多个科目，实际 {a}");
        assert_eq!(db.options().base_currency, "CNY");
        assert!(users::get(&db, "admin").unwrap().is_some());
    }

    #[test]
    fn options_roundtrip() {
        let db = mem();
        let mut o = BookOptions::default();
        o.company = "某某科技有限公司".to_string();
        o.start_period = fincore::Period::new(2026, 3).unwrap();
        db.set_options(&o).unwrap();
        assert_eq!(db.options().company, "某某科技有限公司");
        assert_eq!(
            db.options().start_period,
            fincore::Period::new(2026, 3).unwrap()
        );
    }

    #[test]
    fn log_records() {
        let db = mem();
        db.log("admin", "凭证", "新增", "记-0001").unwrap();
        let logs = db.recent_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].user, "admin");
        assert_eq!(db.search_logs("记-0001", 10).unwrap().len(), 1);
        assert!(db.search_logs("不存在的关键字", 10).unwrap().is_empty());
    }

    #[test]
    fn integrity() {
        let db = mem();
        let r = db.integrity_check().unwrap();
        assert_eq!(r, vec!["ok".to_string()]);
    }

    #[test]
    fn backup_auto_with_retention() {
        let db = mem();
        let dir = std::env::temp_dir().join(format!(
            "finbook_test_backup_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 故意放一个手工命名的备份，验证轮转不会误删它
        let manual = dir.join("user_backup.fbk");
        std::fs::write(&manual, b"manual").unwrap();

        // 生成 5 份自动备份，保留 3 份
        for _ in 0..5 {
            db.backup_auto(&dir, 3).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20)); // 保证时间戳/修改时间不同
        }
        let autos: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("auto_"))
            .collect();
        assert_eq!(autos.len(), 3, "自动备份应只保留最近 3 份：{autos:?}");
        assert!(
            std::fs::read_dir(&dir).unwrap().any(|e| {
                e.ok()
                    .map(|x| x.file_name().to_string_lossy() == "user_backup.fbk")
                    .unwrap_or(false)
            }),
            "手工备份不应被轮转删除"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
