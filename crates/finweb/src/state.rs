//! Web 服务共享状态：数据库连接池、会话管理、鉴权提取器与错误类型。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use fincore::user::{PasswordPolicy, Perm, User};
use findb::{users, Db, DbError};
use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromStr;
use serde_json::json;

/// 全局共享状态（以 Arc 包裹，可被多请求并发引用）
pub struct WebState {
    pub pool: DbPool,
    pub sessions: SessionStore,
    pub policy: PasswordPolicy,
    pub book_path: PathBuf,
    pub company: String,
    pub version: String,
    /// 账套默认（启用）期间，ymm 形式，作为会话期间的初值
    pub default_period: i32,
}

impl WebState {
    pub fn new(
        pool: DbPool,
        policy: PasswordPolicy,
        book_path: PathBuf,
        company: String,
        version: String,
        default_period: i32,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            sessions: SessionStore::new(),
            policy,
            book_path,
            company,
            version,
            default_period,
        })
    }
}

// ---------------------------------------------------------------------------
// 数据库连接池
// ---------------------------------------------------------------------------

/// 简单的连接池：账套文件用 WAL 模式，本机多连接并发写入会自动等待，
/// 不会报 database is locked。每个请求借出一个 Db，用完后归还。
pub struct DbPool {
    inner: Mutex<Vec<Db>>,
    max: usize,
    path: PathBuf,
}

impl DbPool {
    pub fn new(path: &Path, max: usize) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            max: max.max(1),
            path: path.to_path_buf(),
        }
    }

    /// 借出一个数据库连接；池空时临时新建一个。
    pub fn get(&self) -> Result<PooledDb<'_>, DbError> {
        let db = {
            let mut g = self.inner.lock().unwrap();
            g.pop()
        };
        let db = match db {
            Some(d) => d,
            None => Db::open(&self.path)?,
        };
        Ok(PooledDb {
            db: Some(db),
            pool: self,
        })
    }

    fn checkin(&self, db: Db) {
        let mut g = self.inner.lock().unwrap();
        if g.len() < self.max {
            g.push(db);
        }
        // 超过上限则丢弃，连接随 Db 析构关闭
    }
}

/// 借用型连接，作用域结束自动归还池中。
pub struct PooledDb<'a> {
    db: Option<Db>,
    pool: &'a DbPool,
}

impl std::ops::Deref for PooledDb<'_> {
    type Target = Db;
    fn deref(&self) -> &Db {
        self.db.as_ref().unwrap()
    }
}

impl Drop for PooledDb<'_> {
    fn drop(&mut self) {
        if let Some(db) = self.db.take() {
            self.pool.checkin(db);
        }
    }
}

// ---------------------------------------------------------------------------
// 会话管理
// ---------------------------------------------------------------------------

pub struct SessionStore {
    inner: Mutex<HashMap<String, SessionInfo>>,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub username: String,
    /// 登录时的设备指纹（用于逐请求复核"一人一机"策略）
    pub device_id: String,
    pub last_active: i64,
    /// 当前工作期间（ymm），0 表示未设定（用账套默认值）
    pub period_ymm: i32,
}

/// 会话最长保留时间（秒）：与登录 Cookie 的 Max-Age 一致
const SESSION_MAX_SECS: i64 = 60 * 60 * 24 * 7;

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_token() -> String {
        let mut rng = rand::thread_rng();
        (0..32)
            .map(|_| {
                let n: u8 = rng.gen_range(0..16);
                char::from_digit(n as u32, 16).unwrap()
            })
            .collect()
    }

    pub fn create(&self, username: &str, device_id: &str, period_ymm: i32) -> String {
        let token = Self::new_token();
        let now = now_secs();
        let mut g = self.inner.lock().unwrap();
        // 顺手清理过期会话，避免长期运行时会话表无限增长
        g.retain(|_, i| now - i.last_active < SESSION_MAX_SECS);
        g.insert(
            token.clone(),
            SessionInfo {
                username: username.to_string(),
                device_id: device_id.to_string(),
                last_active: now,
                period_ymm,
            },
        );
        token
    }

    /// 取会话；空闲超时（分钟）大于 0 且已超时则视为失效并清除。
    pub fn get(&self, token: &str, idle_minutes: i64) -> Option<SessionInfo> {
        let mut g = self.inner.lock().unwrap();
        let info = g.get(token)?;
        if idle_minutes > 0 && now_secs() - info.last_active > idle_minutes * 60 {
            g.remove(token);
            return None;
        }
        Some(info.clone())
    }

    pub fn touch(&self, token: &str) {
        if let Some(i) = self.inner.lock().unwrap().get_mut(token) {
            i.last_active = now_secs();
        }
    }

    pub fn period(&self, token: &str) -> Option<i32> {
        self.inner.lock().unwrap().get(token).map(|i| i.period_ymm)
    }

    pub fn set_period(&self, token: &str, ymm: i32) {
        if let Some(i) = self.inner.lock().unwrap().get_mut(token) {
            i.period_ymm = ymm;
        }
    }

    pub fn remove(&self, token: &str) {
        self.inner.lock().unwrap().remove(token);
    }

    /// 清掉某个用户的全部会话（重置设备绑定 / 删除 / 停用账号时调用），
    /// 否则旧设备上的会话还能继续用到自然过期，"一人一机"会被绕过。
    pub fn remove_by_username(&self, username: &str) {
        self.inner.lock().unwrap().retain(|_, i| i.username != username);
    }
}

// ---------------------------------------------------------------------------
// 鉴权提取器
// ---------------------------------------------------------------------------

/// 当前登录用户（从会话 Cookie 解析，并每次请求回读数据库取最新权限）
pub struct CurrentUser {
    pub user: User,
    /// 会话令牌（服务端内部使用，不向外暴露）
    pub token: String,
}

impl CurrentUser {
    pub fn username(&self) -> &str {
        &self.user.username
    }

    /// 是否拥有某权限（角色权限 + 额外权限）
    pub fn can(&self, p: Perm) -> bool {
        self.user.can(p)
    }

    /// 校验权限，无权限返回 403
    pub fn require(&self, p: Perm) -> Result<(), AppError> {
        if self.can(p) {
            Ok(())
        } else {
            Err(AppError::forbidden(format!("没有「{}」权限", p.label())))
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<Arc<WebState>> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<WebState>,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get("finbook_sid")
            .map(|c| c.value().to_string())
            .ok_or_else(|| AppError::unauthorized("未登录或会话已失效"))?;
        let info = state
            .sessions
            .get(&token, state.policy.idle_minutes)
            .ok_or_else(|| AppError::unauthorized("会话已过期，请重新登录"))?;
        // 回读数据库：管理员刚改的权限/停用状态立即生效，无需用户重登
        let db = state.pool.get()?;
        let user = users::get(&db, &info.username)?
            .ok_or_else(|| AppError::unauthorized("账号已不存在，请重新登录"))?;
        if user.disabled {
            state.sessions.remove(&token);
            return Err(AppError::forbidden("账号已被停用，请联系管理员"));
        }
        // "一人一机"逐请求复核：管理员豁免；普通账号一旦绑定了新设备，
        // 旧设备上的会话立即失效（管理员重置绑定后旧会话不能继续用）
        if !user.is_admin() && !user.device_id.is_empty() && user.device_id != info.device_id {
            state.sessions.remove(&token);
            return Err(AppError::unauthorized(
                "该账号已在其他设备登录，本设备会话已被下线",
            ));
        }
        drop(db);
        state.sessions.touch(&token);
        Ok(CurrentUser { user, token })
    }
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

pub enum AppError {
    Unauthorized(String),
    Forbidden(String),
    BadRequest(String),
    NotFound(String),
    Db(DbError),
}

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self {
        AppError::Db(e)
    }
}

impl AppError {
    pub fn unauthorized(m: impl Into<String>) -> Self {
        AppError::Unauthorized(m.into())
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        AppError::Forbidden(m.into())
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        AppError::BadRequest(m.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            AppError::Db(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库错误：{e}")),
        };
        (status, axum::Json(json!({ "error": msg }))).into_response()
    }
}

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 构造 Set-Cookie 头值
pub fn cookie_header(token: &str, max_age_secs: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "finbook_sid={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        token, max_age_secs
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("finbook_sid=; Path=/; Max-Age=0"))
}

pub fn clear_cookie_header() -> HeaderValue {
    HeaderValue::from_static("finbook_sid=; Path=/; HttpOnly; Max-Age=0")
}

/// 期间 -> "YYYY-MM"
pub fn period_to_str(p: fincore::Period) -> String {
    format!("{:04}-{:02}", p.year(), p.month())
}

/// 解析 "2026-01" 或 "202601" 为 Period
pub fn parse_period(s: &str) -> Option<fincore::Period> {
    let s = s.trim();
    let (y, m) = if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        (s[0..4].parse().ok()?, s[4..6].parse().ok()?)
    } else if let Some((y, m)) = s.split_once('-') {
        (y.parse().ok()?, m.parse().ok()?)
    } else {
        return None;
    };
    fincore::Period::new(y, m).ok()
}

/// 解析金额字符串（默认 0）
pub fn parse_money(s: &str) -> fincore::Money {
    match Decimal::from_str(s.trim()) {
        Ok(d) => fincore::Money::new(d),
        Err(_) => fincore::Money::ZERO,
    }
}
