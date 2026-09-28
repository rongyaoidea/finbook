//! 凭证仓储
//!
//! 凭证的保存是"先存表头、再整表替换分录"的两步事务——分录整表替换比逐行 diff 简单得多，
//! 单机场景下性能完全够用，还能避免残留孤儿行。

use chrono::NaiveDate;
use fincore::user::User;
use fincore::{
    AuxRef, Entry, FinError, Issues, Money, Period, Voucher, VoucherSource, VoucherStatus,
};
use rusqlite::OptionalExtension;

use crate::{
    exact_param, money_param, read_date_opt, read_money, read_money_opt, Db, DbError, DbResult,
};

/// 凭证查询条件
#[derive(Clone, Default, Debug)]
pub struct VoucherQuery {
    pub from: Option<Period>,
    pub to: Option<Period>,
    pub status: Option<VoucherStatus>,
    pub word: Option<String>,
    pub no_from: Option<i32>,
    pub no_to: Option<i32>,
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    /// 摘要 / 科目编码 / 凭证号 关键字
    pub keyword: Option<String>,
    /// 只返回涉及该科目的凭证（含下级）
    pub account_code: Option<String>,
    /// 只返回涉及该辅助核算的凭证
    pub aux: Option<AuxRef>,
    /// 只返回该制单人填制的凭证（数据范围：仅看本人凭证）
    pub prepared_by: Option<String>,
    pub source: Option<VoucherSource>,
    pub limit: Option<i64>,
    /// 排序：true 为按日期+凭证号升序（默认），false 为降序
    pub asc: bool,
}

impl VoucherQuery {
    pub fn period(p: Period) -> Self {
        Self {
            from: Some(p),
            to: Some(p),
            asc: true,
            ..Default::default()
        }
    }
    pub fn with_status(mut self, s: Option<VoucherStatus>) -> Self {
        self.status = s;
        self
    }
    pub fn with_keyword(mut self, kw: &str) -> Self {
        let kw = kw.trim().to_string();
        self.keyword = if kw.is_empty() { None } else { Some(kw) };
        self
    }

    /// 套用用户的数据范围（DataScope）：
    /// - `own_voucher_only`：只看本人填制的凭证。
    /// - 科目范围：凭证可能涉及多个科目，查询层不便预过滤，
    ///   由界面层对结果逐张调用 `User::can_see_voucher` 过滤
    ///   （账簿/余额表等按科目聚合的模块则在查询条件里取交集）。
    pub fn with_data_scope(mut self, u: &User) -> Self {
        let scope = &u.data_scope;
        if scope.own_voucher_only {
            // 凭证的 prepared_by 存的是登录账号（username），保存侧写 username，
            // 这里必须按 username 过滤，否则 display_name ≠ username 的用户一张都看不到
            self.prepared_by = Some(u.username.clone());
        }
        self
    }
}

fn map_entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let aux_json: String = r.get(5)?;
    let mut aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
    // 现金流量项目单列存储，读回时补进 aux，界面上就能直接编辑
    let cf: Option<String> = r.get(16)?;
    if aux.cash_flow.is_none() {
        aux.cash_flow = cf;
    }
    let rate: Option<String> = r.get(12)?;
    Ok(Entry {
        id: r.get(0)?,
        line: r.get(2)?,
        summary: r.get(3)?,
        account_code: r.get(4)?,
        aux,
        debit: read_money(r, 6)?,
        credit: read_money(r, 7)?,
        qty: read_money_opt(r, 8)?,
        price: read_money_opt(r, 9)?,
        currency: r.get(10)?,
        rate: rate.and_then(|s| s.parse().ok()),
        amount_for: read_money_opt(r, 11)?,
        settle_type: r.get(13)?,
        settle_no: r.get(14)?,
        biz_date: read_date_opt(r, 15)?,
    })
}

fn map_voucher(r: &rusqlite::Row) -> rusqlite::Result<Voucher> {
    let status: String = r.get(5)?;
    let source: String = r.get(11)?;
    Ok(Voucher {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        date: {
            let s: String = r.get(2)?;
            NaiveDate::parse_from_str(&s, "%Y-%m-%d").unwrap_or_else(|_| {
                NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期必然合法")
            })
        },
        word: r.get(3)?,
        no: r.get(4)?,
        status: serde_json::from_str::<VoucherStatus>(&format!("\"{status}\""))
            .unwrap_or(VoucherStatus::Draft),
        attachments: r.get(6)?,
        prepared_by: r.get(7)?,
        audited_by: r.get(8)?,
        posted_by: r.get(9)?,
        cashier: r.get(10)?,
        source: serde_json::from_str::<VoucherSource>(&format!("\"{source}\""))
            .unwrap_or(VoucherSource::Manual),
        memo: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        entries: Vec::new(),
    })
}

const VOUCHER_COLS: &str = "id,period,date,word,no,status,attachments,prepared_by,audited_by,
                            posted_by,cashier,source,memo,created_at,updated_at";

/// 按 id 读取（含分录）
pub fn get(db: &Db, id: i64) -> DbResult<Option<Voucher>> {
    get_of(db.conn(), id)
}

/// 同 [`get`]，但只依赖连接：删除凭证的「读 → 校验 → 清核销 → 删」要收在同一事务内。
fn get_of(conn: &rusqlite::Connection, id: i64) -> DbResult<Option<Voucher>> {
    let mut v = conn
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?;
    if let Some(ref mut v) = v {
        v.entries = entries_on(conn, id)?;
    }
    Ok(v)
}

pub fn entries_of(db: &Db, voucher_id: i64) -> DbResult<Vec<Entry>> {
    entries_on(db.conn(), voucher_id)
}

fn entries_on(conn: &rusqlite::Connection, voucher_id: i64) -> DbResult<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id,voucher_id,line,summary,account_code,aux_json,debit,credit,qty,price,
                currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item
         FROM voucher_entry WHERE voucher_id=?1 ORDER BY line",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![voucher_id], map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 为列表结果批量补充分录（列表界面展示摘要与借贷合计依赖分录）。
/// 一次 IN 查询按批取回，避免逐张 N+1。
pub fn fill_entries(db: &Db, vouchers: &mut [Voucher]) -> DbResult<()> {
    const CHUNK: usize = 500; // SQLite 绑定参数上限以下，分批防超限
    for chunk in vouchers.chunks_mut(CHUNK) {
        let ids: Vec<i64> = chunk.iter().map(|v| v.id).collect();
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT id,voucher_id,line,summary,account_code,aux_json,debit,credit,qty,price,\
                    currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item
             FROM voucher_entry WHERE voucher_id IN ({}) ORDER BY voucher_id, line",
            placeholders.join(",")
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            ids.iter().map(|i| i as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                let vid: i64 = r.get(1)?;
                Ok((vid, map_entry(r)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut by_id: std::collections::HashMap<i64, Vec<Entry>> =
            std::collections::HashMap::new();
        for (vid, e) in rows {
            by_id.entry(vid).or_default().push(e);
        }
        for v in chunk.iter_mut() {
            v.entries = by_id.remove(&v.id).unwrap_or_default();
        }
    }
    Ok(())
}

/// 条件查询（只含表头，列表界面不需要分录）
pub fn list(db: &Db, q: &VoucherQuery) -> DbResult<Vec<Voucher>> {
    let mut sql = format!(
        "SELECT {VOUCHER_COLS} FROM voucher v WHERE 1=1"
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(f) = q.from {
        sql.push_str(" AND v.period >= ?");
        params.push(Box::new(f.ymm()));
    }
    if let Some(t) = q.to {
        sql.push_str(" AND v.period <= ?");
        params.push(Box::new(t.ymm()));
    }
    if let Some(s) = q.status {
        sql.push_str(" AND v.status = ?");
        params.push(Box::new(serde_json::to_value(s)?.as_str().unwrap_or("draft").to_string()));
    }
    if let Some(ref w) = q.word {
        sql.push_str(" AND v.word = ?");
        params.push(Box::new(w.clone()));
    }
    if let Some(n) = q.no_from {
        sql.push_str(" AND v.no >= ?");
        params.push(Box::new(n));
    }
    if let Some(n) = q.no_to {
        sql.push_str(" AND v.no <= ?");
        params.push(Box::new(n));
    }
    if let Some(d) = q.date_from {
        sql.push_str(" AND v.date >= ?");
        params.push(Box::new(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(d) = q.date_to {
        sql.push_str(" AND v.date <= ?");
        params.push(Box::new(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(src) = q.source {
        sql.push_str(" AND v.source = ?");
        params.push(Box::new(
            serde_json::to_value(src)?.as_str().unwrap_or("manual").to_string(),
        ));
    }
    if let Some(ref who) = q.prepared_by {
        sql.push_str(" AND v.prepared_by = ?");
        params.push(Box::new(who.clone()));
    }
    if let Some(ref code) = q.account_code {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id AND e.account_code LIKE ? ESCAPE '\\')",
        );
        params.push(Box::new(format!("{}%", crate::escape_like(code))));
    }
    if let Some(ref aux) = q.aux {
        // 逐维度整段匹配，不能把整条 aux_key 当子串 LIKE：
        //   旧写法 `aux_key LIKE '%customer=C001%'` 会连带命中 `customer=C0011`，
        //   也会把 `project=ACME` 命中到按 `customer=AC` 过滤的查询上（实测确认）。
        // 现在把两侧都补上 \x1f 分隔符再匹配整段，等价于 Rust 侧
        // `balances::aux_key_contains` 的"按维度切开逐段精确匹配"语义。
        for part in aux.key_parts() {
            sql.push_str(
                " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (char(31) || e.aux_key || char(31)) LIKE ? ESCAPE '\\')",
            );
            params.push(Box::new(format!(
                "%\u{1f}{}\u{1f}%",
                crate::escape_like(&part)
            )));
        }
    }
    if let Some(ref kw) = q.keyword {
        sql.push_str(
            " AND (v.memo LIKE ? ESCAPE '\\' OR CAST(v.no AS TEXT) LIKE ? ESCAPE '\\'
                   OR EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (e.summary LIKE ? ESCAPE '\\' OR e.account_code LIKE ? ESCAPE '\\')))",
        );
        let k = format!("%{}%", crate::escape_like(kw));
        for _ in 0..4 {
            params.push(Box::new(k.clone()));
        }
    }

    sql.push_str(" ORDER BY v.date ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });
    sql.push_str(", v.word ASC, v.no ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });

    if let Some(l) = q.limit {
        sql.push_str(" LIMIT ?");
        params.push(Box::new(l));
    }

    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), map_voucher)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 保存（新增或更新）。返回凭证 id。
///
/// 这里是**持久化层的最后一道防线**：即使调用方忘了校验，也不允许把
/// 借贷不平衡、科目不合法、期间已结账、或已审核/已记账的凭证写进账套。
/// 界面层负责给友好提示，数据库层必须无条件拦住。
pub fn save(db: &Db, v: &mut Voucher) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = save_in(&tx, v)?;
    tx.commit()?;
    v.id = id;
    Ok(id)
}

/// 在**调用方已开启的事务**里保存凭证：守卫与落库都在该事务内完成，既不发
/// `BEGIN` 也不提交。
///
/// 「生成凭证 + 回写业务单据 / 扣库存 / 归集成本」需要原子的调用方用它。
/// `save` 自带事务，那些流程只能拆成两次提交，中途失败就留下「库存已扣、账上无
/// 凭证」的半成品；`next_no` 取号也因跨事务而会并发撞号。
pub fn save_in(tx: &rusqlite::Transaction, v: &mut Voucher) -> DbResult<i64> {
    save_on(tx, v)
}

/// 守卫 + 落库，全部跑在给定的连接上（通常就是调用方的事务；`Transaction` 会
/// Deref 到 `Connection`）。不发 BEGIN、不提交。
fn save_on(tx: &rusqlite::Connection, v: &mut Voucher) -> DbResult<i64> {
    // ---- 0. 前置守卫 ----
    // 守卫一律在同一事务里读，否则「查结账线 / 查凭证号占用 → 写入」之间存在
    // 别人见缝插针的窗口（期间刚被结账、或同号凭证刚被别人插入）。
    let closed = crate::periods::closed_upto_of(tx)?;
    if let Some(upto) = closed {
        if v.period <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能再保存该期间的凭证",
                upto.label()
            ))
            .into());
        }
    }
    if v.id > 0 {
        // 更新：草稿可直接改；已审核/已记账需先反审核/反记账。
        // 只取 status 一列，不必像 get() 那样把分录全捞出来。
        let old_status: Option<String> = tx
            .query_row(
                "SELECT status FROM voucher WHERE id=?1",
                rusqlite::params![v.id],
                |r| r.get(0),
            )
            .optional()?;
        let old_status = old_status
            .ok_or_else(|| FinError::not_found(format!("凭证 #{}", v.id)))?;
        let old_status = serde_json::from_str::<VoucherStatus>(&format!("\"{old_status}\""))
            .unwrap_or(VoucherStatus::Draft);
        if !old_status.can_edit() {
            return Err(FinError::state(format!(
                "凭证当前状态为「{}」，不能修改（请先反审核 / 反记账）",
                old_status.label()
            ))
            .into());
        }
        // 启用审核环节的账套：已审核凭证不能直接改，须先反审核
        if old_status == VoucherStatus::Audited && crate::options_of(tx).enable_audit {
            return Err(FinError::state("已审核凭证不能修改，请先反审核").into());
        }
    } else if v.status == VoucherStatus::Void {
        return Err(FinError::state("新增凭证不能直接标记为「已作废」".to_string()).into());
    }
    let taken: i64 = tx.query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND word=?2 AND no=?3 AND id<>?4",
        rusqlite::params![v.period.ymm(), v.word, v.no, v.id],
        |r| r.get(0),
    )?;
    if taken > 0 {
        return Err(FinError::msg(format!(
            "{}-{:04} 已存在，请更换凭证号",
            v.word, v.no
        ))
        .into());
    }
    let chart = crate::accounts::chart_of(tx)?;
    let opts = crate::options_of(tx);
    fincore::engine::validate_for_save(v, &fincore::engine::ValidateCtx::new(&chart, &opts, closed))
        .into_result()?;

    // ---- 1. 落库 ----
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let id: i64 = if v.id > 0 {
        tx.execute(
            "UPDATE voucher SET period=?2,date=?3,word=?4,no=?5,attachments=?6,status=?7,
                    prepared_by=?8,audited_by=?9,posted_by=?10,cashier=?11,source=?12,memo=?13,
                    updated_at=?14
             WHERE id=?1",
            rusqlite::params![
                v.id,
                v.period.ymm(),
                v.date.format("%Y-%m-%d").to_string(),
                v.word,
                v.no,
                v.attachments,
                serde_json::to_value(v.status)?.as_str().unwrap_or("draft"),
                v.prepared_by,
                v.audited_by,
                v.posted_by,
                v.cashier,
                serde_json::to_value(v.source)?.as_str().unwrap_or("manual"),
                v.memo,
                now,
            ],
        )?;
        tx.execute("DELETE FROM voucher_entry WHERE voucher_id=?1", rusqlite::params![v.id])?;
        v.id
    } else {
        tx.execute(
            "INSERT INTO voucher(period,date,word,no,attachments,status,prepared_by,audited_by,
                                 posted_by,cashier,source,memo,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
            rusqlite::params![
                v.period.ymm(),
                v.date.format("%Y-%m-%d").to_string(),
                v.word,
                v.no,
                v.attachments,
                serde_json::to_value(v.status)?.as_str().unwrap_or("draft"),
                v.prepared_by,
                v.audited_by,
                v.posted_by,
                v.cashier,
                serde_json::to_value(v.source)?.as_str().unwrap_or("manual"),
                v.memo,
                now,
            ],
        )?;
        tx.last_insert_rowid()
    };

    // 分录：跳过借贷均为零的空行
    let mut stmt = tx.prepare(
        "INSERT INTO voucher_entry(voucher_id,period,line,summary,account_code,aux_key,aux_json,
                debit,credit,qty,price,currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
    )?;
    let mut line = 0i32;
    for e in &v.entries {
        if e.is_blank() {
            continue;
        }
        line += 1;
        stmt.execute(rusqlite::params![
            id,
            v.period.ymm(),
            line,
            e.summary,
            e.account_code,
            e.aux.key(),
            serde_json::to_string(&e.aux)?,
            money_param(e.debit),
            money_param(e.credit),
            e.qty.map(exact_param),
            e.price.map(exact_param),
            e.currency,
            e.amount_for.map(money_param),
            e.rate.map(|r| r.to_string()),
            e.settle_type,
            e.settle_no,
            e.biz_date.map(|d| d.format("%Y-%m-%d").to_string()),
            e.aux.cash_flow.clone(),
        ])?;
    }
    drop(stmt);
    v.id = id;
    Ok(id)
}

/// 删除凭证（连同分录，外键 ON DELETE CASCADE）
///
/// 「读凭证 → 校验 → 清核销 → 删表」整体收在一个 `write_tx` 里：核销清理是逐条
/// `DELETE`，第 3 条失败时前两条已经在别的事务里提交，凭证却还在——账上留着一条
/// 已核销的凭证，核销记录却只剩半截，比删不掉更难查。
///
/// 文件删除在提交之后做：反过来做（旧实现先删文件）一旦删库失败——状态被拦、
/// 拿不到写锁、外键报错——凭证还留在账上而附件文件已经没了，变成引用空文件的坏账。
/// 删文件失败只留下孤儿文件，无害得多。
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    // 附件清单先取（attach::list 是纯读，与事务无关）
    let files: Vec<String> = crate::attach::list(db, id)?
        .into_iter()
        .filter(|a| !a.inline)
        .filter_map(|a| a.path)
        .collect();
    if db.conn().is_autocommit() {
        let tx = db.write_tx()?;
        delete_in(&tx, id)?;
        tx.commit()?;
    } else {
        // 调用方（如 receipt_unaudit）已开启事务：复用它，失败由它整体回滚。
        // 这里再 BEGIN 会被 SQLite 拒掉（cannot start a transaction within a
        // transaction），所以必须走同连接直写这条路。
        delete_in_of(db.conn(), id)?;
    }
    let dir = crate::attach::dir_of(db);
    for rel in files {
        // 与 attach::delete 保持同样的路径约束，避免越出附件目录
        if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
            continue;
        }
        let _ = std::fs::remove_file(dir.join(rel));
    }
    Ok(())
}

/// 在调用方已开启的事务里删凭证（不 commit）：供已有事务的流程复用。
pub fn delete_in(tx: &rusqlite::Transaction, id: i64) -> DbResult<()> {
    delete_in_of(tx, id)
}

fn delete_in_of(conn: &rusqlite::Connection, id: i64) -> DbResult<()> {
    // 持久层最后防线：状态与结账线在这里再校验一次，UI/Web 漏检也删不掉
    let v = get_of(conn, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    // 启用审核环节的账套：已审核凭证须先反审核才能删除
    if v.status == VoucherStatus::Audited && crate::options_of(conn).enable_audit {
        return Err(FinError::state("已审核凭证不能删除，请先反审核").into());
    }
    fincore::engine::validate_delete(&v, crate::periods::closed_upto_of(conn)?).into_result()?;
    // 删除前清掉该凭证分录上的核销配对（手工/收付款自动核销），避免留下悬空核销记录
    let entry_ids: Vec<i64> = {
        let mut st = conn.prepare("SELECT id FROM voucher_entry WHERE voucher_id=?1")?;
        let ids = st
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };
    for e in entry_ids {
        conn.execute(
            "DELETE FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
            rusqlite::params![e],
        )?;
    }
    // 条件删除：状态在上面的校验与本次写入之间若被改动（并发反记账/作废），这里拒绝
    let n = conn.execute(
        "DELETE FROM voucher WHERE id=?1 AND status<>'posted'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(FinError::state("凭证状态已变化，请刷新后重试").into());
    }
    Ok(())
}

/// 下一个可用凭证号
///
/// 单独取号只在**同一个写事务里接着 `save_in`** 时才可靠；两者跨事务时并发调用者
/// 会取到同一个号，靠 `voucher(period,word,no)` 唯一索引兜底成一次失败。
pub fn next_no(db: &Db, period: Period, word: &str) -> DbResult<i32> {
    next_no_of(db.conn(), period, word)
}

/// 同 `next_no`，但只依赖连接，可在调用方的事务内取号。
pub fn next_no_of(tx: &rusqlite::Connection, period: Period, word: &str) -> DbResult<i32> {
    let max: Option<i32> = tx
        .query_row(
            "SELECT MAX(no) FROM voucher WHERE period=?1 AND word=?2",
            rusqlite::params![period.ymm(), word],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(max.unwrap_or(0) + 1)
}

/// 检查凭证号是否被占用（排除自身）
pub fn no_taken(db: &Db, period: Period, word: &str, no: i32, except_id: i64) -> DbResult<bool> {
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND word=?2 AND no=?3 AND id<>?4",
        rusqlite::params![period.ymm(), word, no, except_id],
        |r| r.get(0),
    )?;
    Ok(c > 0)
}

// ---------------- 状态流转 ----------------

fn set_status_on(conn: &rusqlite::Connection, id: i64, s: VoucherStatus) -> DbResult<()> {
    conn.execute(
        "UPDATE voucher SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            id,
            serde_json::to_value(s)?.as_str().unwrap_or("draft"),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// 记账（未记账 → 已记账；无审核环节，核对无误后直接记账）
/// 一张凭证的更正链信息
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AmendLink {
    /// 本凭证更正的是哪张
    pub amends_id: i64,
    pub amends_no: String,
    /// 本凭证被哪张更正了
    pub amended_by: i64,
    pub amended_by_no: String,
    /// 更正原因（必填 —— 没有原因的更正等于没更正）
    pub reason: String,
}

/// 读一张凭证的更正链（凭证不存在时返回 None）
pub fn amend_link(db: &Db, id: i64) -> DbResult<Option<AmendLink>> {
    let row: Option<(i64, i64, String)> = db
        .conn()
        .query_row(
            "SELECT amends_id, amended_by, amend_reason FROM voucher WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((amends_id, amended_by, reason)) = row else {
        return Ok(None);
    };
    // 单号做成「记-000012」这种可读形式：链上要能一眼看懂是哪张，
    // 光给个数字 id 在对账时没用
    let label = |vid: i64| -> String {
        db.conn()
            .query_row(
                "SELECT word, no FROM voucher WHERE id=?1",
                [vid],
                |r| Ok(format!("{}-{:06}", r.get::<_, String>(0)?, r.get::<_, i32>(1)?)),
            )
            .unwrap_or_default()
    };
    Ok(Some(AmendLink {
        amends_id,
        amends_no: if amends_id > 0 { label(amends_id) } else { String::new() },
        amended_by,
        amended_by_no: if amended_by > 0 { label(amended_by) } else { String::new() },
        reason,
    }))
}

/// 单据更正链：作废原凭证 + 生成一张带链接的新凭证
///
/// ## 为什么要有这个
///
/// finbook 有反记账与取消审核，所以错凭证**能改** —— 但改完的两张凭证之间
/// **没有任何链接**，事后审计只能靠时间和金额去猜哪张在冲哪张。
///
/// 「不能改」反而更安全：它会逼人走更正流程；「能改」会让人直接改，
/// 而直接改完的两张凭证互相不认识，才是真正查不出来的状态。
///
/// ## 与 `reverse`（红字冲销）的分工 —— 两条路径的审计特征不同
///
/// finbook 现在有两条更正路径，用错会导致账簿形态不合准则：
///
/// | | 本函数（amend，更正链） | `reverse`（红字冲销）
/// |---|---|---|
/// | 原凭证 | **作废**（`status='void'`） | **原样保留、仍已记账**
/// | 总账痕迹 | 原凭证从总账**消失** | 一笔反向分录，**留在总账里**
/// | 新凭证 | 草稿，落**原期间** | 草稿，落**当前期间** |
/// | 适合 | 科目错、摘要错、分录结构错 —— 整单要重做 | 金额错、时点错 —— 只想增减一笔 |
///
/// **中国准则对「以前年度差错」要求红字冲销 + 蓝字重做**（即 `reverse` 形态：
/// 总账里要看得见那一笔反向记录）。ERPNext 的 Cancel 同样是「ledger 不删除、
/// 加反向分录」而不是作废。
///
/// 所以：**只做金额/时点调整 → 走红字冲销**（总账留痕、符合准则）；
/// 整单内容错了要重做 → 走本函数（链清楚，但总账里看不到那一笔冲销）。
/// 拿本函数处理需要红冲留痕的场景，总账上会「看不出曾经记错过」——
/// 链信息只在凭证表里，账簿层面不体现。
///
/// ## 规则
///
/// - 只有**已记账**的凭证需要更正。草稿直接改，不必走这里。
/// - 更正原因**必填**：没有原因的更正等于没更正。
/// - 一张凭证**只能被更正一次**（`amended_by` 非 0 就拒绝），否则链会分叉，
///   审计时看不出「哪条是最终版本」。
/// - 全部动作在**一个事务**里：原凭证已作废却没有替代凭证是最坏状态 ——
///   账实凭空少一笔，而且凭证号已经用掉了。
/// - 新凭证落在**原凭证的期间与日期**（不是当前期间/今天）：更正属于被更正的那个
///   会计期间，否则两张凭证分属不同期间，月度报表会出现「凭空一笔」；而凭证校验
///   强制 `date` 必须属于 `period`，用今天的日子配 1 月的期间根本存不进去。
pub fn amend(db: &Db, id: i64, reason: &str, who: &str) -> DbResult<i64> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(FinError::msg("更正原因不能为空：没有原因的更正等于没更正").into());
    }
    let tx = db.write_tx()?;
    let old: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // 「已被更正过」要**先于**状态检查说：更正过的凭证一定是「已作废」，
    // 反过来判就会得到笼统的「只有已记账的凭证才需要更正」，
    // 而真正的理由（别再改这张，去改新那张）被吞掉了。
    let already: i64 = tx.query_row(
        "SELECT amended_by FROM voucher WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if already > 0 {
        return Err(FinError::state(format!(
            "本凭证已被 #{already} 更正过，不能重复更正 —— 更正链分叉后审计就看不出哪条是最终版本了"
        ))
        .into());
    }
    if old.status != VoucherStatus::Posted {
        return Err(FinError::state(format!(
            "只有已记账的凭证才需要更正，当前是「{}」—— 草稿/未记账的直接改就行",
            old.status.label()
        ))
        .into());
    }
    // 期间已结账的凭证不能更正（与 set_void 同口径）
    let closed: Option<i32> =
        tx.query_row("SELECT MAX(period) FROM period_state WHERE closed=1", [], |r| r.get(0))?;
    if let Some(upto) = closed {
        if old.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能更正该期间的凭证",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }

    // ① 作废原凭证：已记账的不能直接作废，先回退到审核态再作废
    let back = if old.audited_by.is_some() { VoucherStatus::Audited } else { VoucherStatus::Draft };
    set_status_on(&tx, id, back)?;
    tx.execute("UPDATE voucher SET posted_by=NULL WHERE id=?1", [id])?;
    set_status_on(&tx, id, VoucherStatus::Void)?;

    // ② 复制成新凭证（草稿态，等会计审核记账）
    let entries = entries_on(&tx, id)?;
    if entries.is_empty() {
        return Err(FinError::state("原凭证没有分录，无法更正".to_string()).into());
    }
    // 日期必须落在**原期间内**（凭证校验强制 date 属于 period）。
    // 所以更正凭证沿用原凭证日期，而不是今天 —— 用今天会把一张 1 月的更正
    // 记进 9 月的账，月度报表凭空多一笔。ERPNext 也是同期间更正。
    let mut v = Voucher::new(old.period, old.date, &old.word, 0);
    v.prepared_by = who.to_string();
    v.source = old.source;
    v.memo = format!("更正 {}-{:06}（{}）", old.word, old.no, reason);
    v.attachments = old.attachments;
    v.entries = entries;
    v.no = next_no_of(&tx, old.period, &old.word)?;
    let new_id = save_on(&tx, &mut v)?;

    // ③ 双向建链
    tx.execute(
        "UPDATE voucher SET amends_id=?2, amend_reason=?3 WHERE id=?1",
        rusqlite::params![new_id, id, reason],
    )?;
    tx.execute(
        "UPDATE voucher SET amended_by=?2, amend_reason=?3, updated_at=?4 WHERE id=?1",
        rusqlite::params![id, new_id, reason, now],
    )?;
    // ④ 操作日志必须**在同一个事务里**（与 post / unpost 同口径）。
    // 放在调用方写的话：更正已提交、日志还没写就崩溃 —— 账改了却查不到谁改的，
    // 那正是这套更正链要解决的审计洞，不能自己再捅一个。
    crate::log_on(
        &tx,
        who,
        "凭证",
        "更正",
        &format!("凭证 #{id} -> #{new_id}：{reason}"),
    )?;
    tx.commit()?;
    Ok(new_id)
}

pub fn post(db: &Db, id: i64, who: &str) -> DbResult<()> {
    // 校验与状态更新必须在同一个写事务里重读：否则并发结账/作废后，
    // 本请求按事务外的旧快照仍会写入 posted。
    let tx = db.write_tx()?;
    let label = post_tx(&tx, id, who)?;
    crate::log_on(&tx, who, "凭证", "记账", &label)?;
    tx.commit()?;
    Ok(())
}

/// 反记账（已记账 → 未记账）
pub fn unpost(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    fincore::engine::validate_unpost(&v, closed.map(Period::from_ymm)).into_result()?;
    // 反记账回到审核前的状态：有审核记录的回「已审核」，否则回「未记账」
    let back = if v.audited_by.is_some() { "audited" } else { "draft" };
    tx.execute(
        "UPDATE voucher SET status=?3, posted_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![
            id,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            back
        ],
    )?;
    crate::log_on(
        &tx,
        v.posted_by.as_deref().unwrap_or_default(),
        "凭证",
        "反记账",
        &v.voucher_no(),
    )?;
    tx.commit()?;
    Ok(())
}

/// 审核（未记账 → 已审核）。幂等：已审核时直接返回成功。
pub fn audit(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status == VoucherStatus::Audited {
        return Ok(());
    }
    if v.status != VoucherStatus::Draft {
        return Err(FinError::state(format!("状态为「{}」，不能审核", v.status.label())).into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET status='audited', audited_by=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, who, now],
    )?;
    crate::log_on(&tx, who, "凭证", "审核", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 反审核（已审核 → 未记账）
pub fn unaudit(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status != VoucherStatus::Audited {
        return Err(FinError::state("只有已审核凭证才能反审核").into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET status='draft', audited_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, now],
    )?;
    crate::log_on(&tx, who, "凭证", "反审核", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 出纳签字（未记账凭证记录签字人）。幂等：已签字直接返回成功（保留首位签字人，改签先取消）。
///
/// 与 `BookOptions::require_cashier` 配合：账套开启后，涉及现金/银行科目的凭证
/// 须先经出纳签字才能记账（把关在 `post_tx`）。
pub fn sign(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    match v.status {
        VoucherStatus::Draft | VoucherStatus::Audited => {}
        VoucherStatus::Posted => {
            return Err(FinError::state("已记账凭证不能签字，请先反记账").into())
        }
        VoucherStatus::Void => return Err(FinError::state("已作废凭证不能签字").into()),
    }
    if v.cashier.is_some() {
        return Ok(());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET cashier=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, who, now],
    )?;
    crate::log_on(&tx, who, "凭证", "出纳签字", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 取消出纳签字（未记账凭证清空签字人）。幂等：未签字直接返回成功。
pub fn unsign(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    match v.status {
        VoucherStatus::Draft | VoucherStatus::Audited => {}
        VoucherStatus::Posted => {
            return Err(FinError::state("已记账凭证不能取消签字，请先反记账").into())
        }
        VoucherStatus::Void => return Err(FinError::state("已作废凭证不能取消签字").into()),
    }
    if v.cashier.is_none() {
        return Ok(());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET cashier=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, now],
    )?;
    crate::log_on(&tx, who, "凭证", "取消签字", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 作废 / 恢复。作废与取消作废都会记入操作日志。
pub fn set_void(db: &Db, id: i64, void: bool, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    // 已结账期间的凭证不能作废/恢复，否则已封账期间的报表口径会被改写
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(upto) = closed {
        if v.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能作废或恢复凭证",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }
    if void {
        let iss = fincore::engine::validate_void(&v);
        iss.into_result()?;
        set_status_on(&tx, id, VoucherStatus::Void)?;
    } else {
        if v.status != VoucherStatus::Void {
            return Err(FinError::state("该凭证未处于作废状态").into());
        }
        // 已被更正的凭证不能取消作废。
        //
        // 更正链要求「原=作废、新=生效」是唯一形态。恢复作废后原凭证回到未记账，
        // 看着「只是回到未记账」没什么，但**它可以被重新记账** —— 那时它与那张
        // 更正凭证同时进总账，一笔钱记两遍，链上还挂着两个生效版本。
        // 审计看到的是「原凭证已作废、已被更正」，账上却是两笔。
        let by: i64 = tx.query_row(
            "SELECT amended_by FROM voucher WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if by > 0 {
            return Err(FinError::state(format!(
                "本凭证已被 #{by} 更正过，不能恢复作废 —— 恢复后它与那张更正凭证会同时进总账。要改请改那张更正凭证"
            ))
            .into());
        }
        let back = if v.posted_by.is_some() {
            VoucherStatus::Posted
        } else if v.audited_by.is_some() {
            VoucherStatus::Audited
        } else {
            VoucherStatus::Draft
        };
        set_status_on(&tx, id, back)?;
    }
    // 状态与日志同事务：崩溃也不会留下"状态已改、无审计"的缺口
    crate::log_on(
        &tx,
        who,
        "凭证",
        if void { "作废" } else { "取消作废" },
        &v.voucher_no(),
    )?;
    tx.commit()?;
    Ok(())
}

/// 批量记账（未记账 → 已记账）
pub fn post_many(db: &Db, ids: &[i64], who: &str) -> DbResult<(usize, Vec<String>)> {
    let mut ok = 0usize;
    let mut errs = Vec::new();
    let mut labels = Vec::new();
    let tx = db.write_tx()?;
    for id in ids {
        match post_tx(&tx, *id, who) {
            Ok(label) => {
                ok += 1;
                labels.push(label);
            }
            Err(e) => errs.push(format!("#{id}：{e}")),
        }
    }
    tx.commit()?;
    if !labels.is_empty() {
        db.log(who, "凭证", "批量记账", &labels.join("、"))?;
    }
    Ok((ok, errs))
}

/// 返回该凭证的凭证号，供调用方写操作日志
fn post_tx(tx: &rusqlite::Transaction, id: i64, who: &str) -> Result<String, DbError> {
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if !v.status.can_post() {
        return Err(FinError::state(format!("状态为「{}」，不能记账", v.status.label())).into());
    }
    // 与单条记账保持同一套把关：借贷必须平衡，期间不能已结账
    if !v.balanced() {
        return Err(FinError::state("凭证借贷不平衡，不能记账").into());
    }
    // 启用审核环节的账套：必须先审核才能记账
    let opts = crate::options_of(tx);
    if opts.enable_audit && v.status != VoucherStatus::Audited {
        return Err(FinError::state("该账套启用了审核环节，请先审核凭证再记账").into());
    }
    // 启用出纳签字前置的账套：涉及现金/银行科目的凭证须出纳签字后才能记账
    if opts.require_cashier && v.cashier.is_none() {
        let touches: Option<i32> = tx
            .query_row(
                "SELECT 1 FROM voucher_entry e JOIN account a ON a.code = e.account_code
                 WHERE e.voucher_id = ?1 AND (a.is_cash = 1 OR a.is_bank = 1)
                 LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if touches.is_some() {
            return Err(FinError::state(
                "该账套要求出纳签字：涉及现金/银行科目的凭证需出纳先签字再记账",
            )
            .into());
        }
    }
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(upto) = closed {
        if v.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能记账",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }
    tx.execute(
        "UPDATE voucher SET status='posted', posted_by=?2 WHERE id=?1",
        rusqlite::params![id, who],
    )?;
    Ok(v.voucher_no())
}

/// 统计某期间的凭证状态分布
pub fn status_summary(db: &Db, period: Period) -> DbResult<(i64, i64, i64, i64)> {
    let c = |s: &str| -> DbResult<i64> {
        Ok(db.conn().query_row(
            "SELECT COUNT(*) FROM voucher WHERE period=?1 AND status=?2",
            rusqlite::params![period.ymm(), s],
            |r| r.get(0),
        )?)
    };
    Ok((c("draft")?, c("audited")?, c("posted")?, c("void")?))
}

/// 检查期间内凭证号是否连续（断号检查）
pub fn find_gaps(db: &Db, period: Period, word: &str) -> DbResult<Vec<i32>> {
    let mut stmt = db.conn().prepare(
        "SELECT no FROM voucher WHERE period=?1 AND word=?2 ORDER BY no",
    )?;
    let nos: Vec<i32> = stmt
        .query_map(rusqlite::params![period.ymm(), word], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut gaps = Vec::new();
    let mut expect = 1;
    for n in nos {
        while expect < n {
            gaps.push(expect);
            expect += 1;
        }
        expect = n + 1;
    }
    Ok(gaps)
}

/// 红字冲销一张凭证：生成借贷互换、摘要加「冲销」前缀的反向凭证并落库。
///
/// 原凭证保留不动（符合审计要求），冲销凭证落在 `period`（通常为当前期），
/// 状态为「未记账」，由用户核对后手动记账。
pub fn reverse(
    db: &Db,
    id: i64,
    who: &str,
    period: Period,
    date: NaiveDate,
) -> DbResult<i64> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status == VoucherStatus::Void {
        return Err(FinError::state("已作废凭证无需冲销").into());
    }
    if v.entries.iter().filter(|e| !e.is_blank()).count() == 0 {
        return Err(FinError::state("凭证无有效分录，无法冲销").into());
    }
    let mut r = fincore::engine::reverse_voucher(&v);
    r.period = period;
    r.date = date;
    r.no = next_no(db, period, &v.word)?;
    r.status = VoucherStatus::Draft;
    r.prepared_by = who.to_string();
    r.posted_by = None;
    r.audited_by = None;
    r.cashier = None;
    r.source = VoucherSource::Manual;
    r.memo = format!("红字冲销 {}-{:04}（{}）", v.word, v.no, v.date.format("%Y-%m-%d"));
    let nid = save(db, &mut r)?;
    db.log(
        who,
        "凭证",
        "红字冲销",
        &format!("{}-{:04} → 冲销凭证 {}-{:04}", v.word, v.no, v.word, r.no),
    )?;
    Ok(nid)
}

/// 把某期间内同凭证字的凭证号重排为连续（按 日期 + 原凭证号 升序）。
///
/// 用于删除后消除断号。返回重排的凭证张数。
pub fn renumber(db: &Db, period: Period, word: &str) -> DbResult<usize> {
    let rows: Vec<(i64, NaiveDate, i32)> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT id,date,no FROM voucher WHERE period=?1 AND word=?2 ORDER BY date,no")?;
        let rows = stmt
            .query_map(rusqlite::params![period.ymm(), word], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i32>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, ds, no)| {
                let d = NaiveDate::parse_from_str(&ds, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期必然合法"));
                (id, d, no)
            })
            .collect()
    };
    if rows.is_empty() {
        return Ok(0);
    }
    let closed = crate::periods::closed_upto(db)?;
    if let Some(upto) = closed {
        if period <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能重排凭证号",
                upto.label()
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    let mut stmt = tx.prepare("UPDATE voucher SET no=?2 WHERE id=?1")?;
    // 两阶段重排：日期序与原凭证号序可能不一致，直接按目标号更新会撞到
    // 尚未让位的号（UNIQUE(period,word,no)）。先写成互不冲突的临时负数，
    // 再写成 1..n，整批最多两次 UPDATE。
    let mut n = 0i32;
    for (id, _d, _no) in &rows {
        n += 1;
        stmt.execute(rusqlite::params![id, -n])?;
    }
    let mut n = 0i32;
    for (id, _d, _no) in &rows {
        n += 1;
        stmt.execute(rusqlite::params![id, n])?;
    }
    drop(stmt);
    tx.commit()?;
    db.log("系统", "凭证", "重排凭证号", &format!("{}-{} 共 {} 张", period.label(), word, rows.len()))?;
    Ok(rows.len())
}

/// 按科目汇总某期间的分录（用于多栏账、现金流量表等）
///
/// 聚合在 Rust 侧用 `Decimal` 完成：SQLite 没有十进制类型，`SUM(CAST(x AS REAL))`
/// 会引入浮点误差，宁可多传几行数据也不能让账不平。
#[derive(Clone, Debug)]
pub struct AccountSum {
    pub account_code: String,
    pub aux: AuxRef,
    pub debit: Money,
    pub credit: Money,
}

pub fn sum_by_account(
    db: &Db,
    from: Option<Period>,
    to: Option<Period>,
) -> DbResult<Vec<AccountSum>> {
    let (f, t) = (
        from.map(|p| p.ymm()).unwrap_or(0),
        to.map(|p| p.ymm()).unwrap_or(999_999),
    );
    let mut stmt = db.conn().prepare(
        "SELECT e.account_code, e.aux_json, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status != 'void' AND e.period BETWEEN ?1 AND ?2",
    )?;
    let mut acc: std::collections::BTreeMap<(String, String), (Money, Money, AuxRef)> =
        std::collections::BTreeMap::new();
    let mut rows = stmt.query(rusqlite::params![f, t])?;
    while let Some(r) = rows.next()? {
        let code: String = r.get(0)?;
        let aux_json: String = r.get(1)?;
        let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
        let d = read_money(r, 2)?;
        let c = read_money(r, 3)?;
        let e = acc.entry((code, aux.key())).or_insert((Money::ZERO, Money::ZERO, aux));
        e.0 += d;
        e.1 += c;
    }
    Ok(acc
        .into_iter()
        .map(|((code, _), (d, c, aux))| AccountSum {
            account_code: code,
            aux,
            debit: d,
            credit: c,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    // ------------------------------------------------------------------
    // 单据更正链（Cancel -> Amend）
    // ------------------------------------------------------------------

    fn am_voucher(db: &Db, amt: &str) -> i64 {
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(p, chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(), "记", 0);
        v.no = next_no(db, p, "记").unwrap();
        v.memo = "测试凭证".to_string();
        // 科目选型照同文件既有用例（`voucher_roundtrip` 用的是 1001 / 100201）：
        // 1001 是无辅助的末级；100201 核算银行账户，必须带 bank 辅助。
        // 本用例只验更正链的链接与作废语义，不关心业务含义。
        v.push_entry(Entry {
            debit: Money::parse(amt).unwrap(),
            ..Entry::new(1, "1001", "借 库存现金")
        });
        v.push_entry(Entry {
            credit: Money::parse(amt).unwrap(),
            aux: AuxRef {
                bank: Some("B01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "100201", "贷 银行存款")
        });
        save(db, &mut v).unwrap()
    }

    /// 更正链的完整效果：原凭证作废、新凭证草稿待记账、两边互相认得
    ///
    /// 回归背景：finbook 有反记账与取消审核，所以错凭证**能改** —— 但改完的两张
    /// 凭证之间没有任何链接，事后审计只能靠时间和金额去猜哪张在冲哪张。
    /// 「不能改」反而更安全：它会逼人走更正流程。
    #[test]
    fn amend_voids_original_and_links_both_ways() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        assert_eq!(get(&db, old).unwrap().unwrap().status, VoucherStatus::Posted);

        let new_id = amend(&db, old, "金额录错，应为 1200", "boss").unwrap();
        assert!(new_id > 0);

        // 原凭证已作废（作废凭证不进总账，所以总账不会金额翻倍）
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Void,
            "原凭证必须作废，否则新旧两张都会进总账"
        );
        // 新凭证是草稿，等会计审核
        let new_v = get(&db, new_id).unwrap().unwrap();
        assert_eq!(new_v.status, VoucherStatus::Draft);
        assert!(
            new_v.memo.contains("更正") && new_v.memo.contains("金额录错"),
            "摘要要说清是更正哪张、为什么：{}",
            new_v.memo
        );
        // 分录原样复制
        let old_ents = entries_of(&db, old).unwrap();
        assert_eq!(new_v.entries.len(), old_ents.len());
        assert_eq!(
            new_v.entries[0].debit, old_ents[0].debit,
            "分录必须原样复制，否则「更正」就变成了别的业务"
        );
        assert_eq!(new_v.entries[1].credit, old_ents[1].credit);

        // 双向建链
        let a_old = amend_link(&db, old).unwrap().unwrap();
        assert_eq!(a_old.amended_by, new_id, "原凭证要标注「被谁更正」");
        assert!(
            a_old.amended_by_no.starts_with("记-"),
            "要可读单号：{}",
            a_old.amended_by_no
        );
        assert_eq!(a_old.reason, "金额录错，应为 1200");
        let a_new = amend_link(&db, new_id).unwrap().unwrap();
        assert_eq!(a_new.amends_id, old, "新凭证要标注「更正自谁」");
        assert!(!a_new.amends_no.is_empty());

        // 记账后总账只认新凭证那一笔
        post(&db, new_id, "boss").unwrap();
        let on_hand: f64 = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(CAST(e.debit AS REAL)),0) FROM voucher_entry e
                 JOIN voucher v ON v.id=e.voucher_id
                 WHERE e.account_code='1001' AND v.status='posted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // 借方总额仍应只有一笔（新凭证的），作废的那张不进总账
        assert!(
            (on_hand - 1000.0).abs() < 0.01,
            "总账只应有一笔 1000（作废的那张不进总账），实际 {on_hand}"
        );
    }

    /// 操作日志必须与更正**同在一条记录里**：日志写在事务外的话，
    /// 更正已提交、日志还没写就崩溃 —— 账改了却查不到谁改的，
    /// 那正是这套更正链要解决的审计洞。
    #[test]
    fn amend_writes_audit_log_with_the_reason() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        amend(&db, old, "金额录错，应为 1200", "张三").unwrap();
        let (who, action, detail): (String, String, String) = db
            .conn()
            .query_row(
                "SELECT user, action, detail FROM audit_log
                 WHERE module='凭证' AND action='更正' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(who, "张三", "日志要记是谁改的");
        assert_eq!(action, "更正");
        assert!(
            detail.contains("金额录错") && detail.contains(&format!("#{old}")),
            "日志要带原凭证 id 和更正原因（只记动作不记理由等于没记）：{detail}"
        );
    }

    /// 已被更正的凭证不能「恢复作废」。
    ///
    /// 这是本次改动引入的漏洞：更正把原凭证置为作废，而 UI 上「恢复作废」的
    /// 显隐条件正是 `status === 'void'`。恢复后原凭证回到未记账 —— 看着无害，
    /// 但**它可以被重新记账**，届时它与更正凭证同时进总账，一笔钱记两遍，
    /// 而链上仍显示「原凭证已作废、已被更正」。审计与账实直接矛盾。
    #[test]
    fn amended_voucher_cannot_be_restored_from_void() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "金额录错", "boss").unwrap();
        post(&db, new_id, "boss").unwrap();

        let e = set_void(&db, old, false, "boss").unwrap_err();
        assert!(
            format!("{e:?}").contains("更正过"),
            "错误信息要指向「已被更正过」，实际：{e:?}"
        );
        // 被拒后原凭证必须仍是作废 —— 不能被半途恢复
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Void,
            "守卫失败不该动凭证状态"
        );

        // 终态核对：总账里 1001 只有一笔，不是两笔
        let on_hand: f64 = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(CAST(e.debit AS REAL)),0) FROM voucher_entry e
                 JOIN voucher v ON v.id=e.voucher_id
                 WHERE e.account_code='1001' AND v.status='posted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (on_hand - 1000.0).abs() < 0.01,
            "一笔钱只能记一次，实际总账 {on_hand}"
        );
    }

    /// 未被更正的普通作废凭证仍可恢复（守卫不能误伤既有流程）
    #[test]
    fn void_restore_still_works_without_amend() {
        let db = mem();
        let id = am_voucher(&db, "1000");
        set_void(&db, id, true, "boss").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Void);
        set_void(&db, id, false, "boss").unwrap();
        assert_eq!(
            get(&db, id).unwrap().unwrap().status,
            VoucherStatus::Draft,
            "没被更正过的凭证要能正常恢复，否则守卫误伤了作废/恢复这条老路"
        );
    }

    /// 更正原因必填：没有原因的更正等于没更正
    #[test]
    fn amend_requires_a_reason() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let e = amend(&db, old, "   ", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("更正原因"), "实际 {:?}", e);
        // 被拒时原凭证必须还是已记账，不能被半途作废
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Posted,
            "校验失败不该动原凭证"
        );
    }

    /// 一张凭证只能被更正一次：链分叉后审计就看不出哪条是最终版本
    #[test]
    fn amend_refuses_to_branch_the_chain() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "第一次更正", "boss").unwrap();
        let e = amend(&db, old, "再改一次", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("重复更正"), "实际 {:?}", e);
        // 新凭证自己是草稿，所以更正它会被状态拦下
        let e2 = amend(&db, new_id, "改新凭证", "boss").unwrap_err();
        assert!(
            format!("{e2:?}").contains("已记账"),
            "草稿凭证不需要走更正流程：{:?}",
            e2
        );
    }

    /// 草稿凭证不能走更正：它直接改就行，更正只会凭空多一张单
    #[test]
    fn amend_rejects_draft_voucher() {
        let db = mem();
        let id = am_voucher(&db, "1000");
        let e = amend(&db, id, "草稿也要更正", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("已记账"), "实际 {:?}", e);
    }

    /// 审核环节默认**开**（对标金蝶云·星空 + 会计内控底线：制单/审核/记账三权分离）。
    ///
    /// 这个用例是 `crate::tests::mem()` 显式 `enable_audit = false` 的正当性来源：
    /// 夹具为了让别的用例专注各自主题而关掉审核，默认值本身必须有人盯住，否则
    /// 「默认关」和「测试方便」就永远分不清了。
    ///
    /// 三段都要验：
    /// ① `BookOptions::default()` 就是开的
    /// ② 默认账套下草稿直接记账被拒，且错误信息指向「先审核」这个下一步
    /// ③ 审核通过后可以记账（闸门不是死路）
    #[test]
    fn audit_default_blocks_direct_post() {
        // ① 默认值
        let d = fincore::BookOptions::default();
        assert!(
            d.enable_audit,
            "审核环节必须默认开启：默认关等于「谁录的单谁就能记」，三权分离形同虚设"
        );
        assert!(
            !d.require_cashier,
            "出纳签字不是全行业默认（只对资金类有意义），应保持关闭待显式开启"
        );

        // ② 默认账套：草稿直接记账被拒
        let o = fincore::BookOptions {
            start_period: Period::new(2026, 1).unwrap(),
            ..Default::default()
        };
        let db = crate::Db::in_memory(&o).unwrap();
        // sample() 已是借贷平衡的完整凭证（借 1001 库存现金 / 贷 100201 银行存款）
        let mut v = sample(Period::new(2026, 1).unwrap(), 10, 1);
        let id = save(&db, &mut v).unwrap();
        let err = post(&db, id, "u").unwrap_err().to_string();
        assert!(err.contains("审核"), "错误信息应指向「先审核」：{err}");

        // ③ 审核后可记（闸门不是死路）
        audit(&db, id, "auditor").unwrap();
        post(&db, id, "u").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
    }

    fn sample(period: Period, day: u32, no: i32) -> Voucher {
        let d = NaiveDate::from_ymd_opt(period.year(), period.month(), day).unwrap();
        let mut v = Voucher::new(period, d, "记", no);
        v.prepared_by = "张三".to_string();
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            ..Entry::new(1, "1001", "提取现金")
        });
        v.push_entry(Entry {
            credit: Money::parse("1000").unwrap(),
            aux: AuxRef {
                // 100201 核算银行账户，必须填银行账户档案
                bank: Some("B01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "100201", "提取现金")
        });
        v
    }

    /// 辅助核算过滤必须按维度**整段**匹配。
    /// 回归：旧实现把整条 aux_key 包成 `%key%` 做子串 LIKE，导致
    /// 按 `customer=C001` 过滤会连带返回 `customer=C0011` 的凭证，
    /// 按 `customer=AC` 过滤会返回 `customer=ACME` 的凭证（财务上的越权读取）。
    #[test]
    fn aux_filter_is_exact_segment_match() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        let mk = |cust: &str, no: i32| {
            let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let mut v = Voucher::new(p, d, "记", no);
            v.prepared_by = "张三".to_string();
            v.push_entry(Entry {
                debit: Money::parse("100").unwrap(),
                aux: AuxRef {
                    customer: Some(cust.into()),
                    ..Default::default()
                },
                ..Entry::new(1, "112201", "应收货款")
            });
            v.push_entry(Entry {
                credit: Money::parse("100").unwrap(),
                ..Entry::new(2, "600101", "产品销售收入")
            });
            let id = save(&db, &mut v).unwrap();
            post(&db, id, "张三").unwrap();
            id
        };
        let id_c001 = mk("C001", 1);
        let id_c0011 = mk("C0011", 2);
        let id_acme = mk("ACME", 3);

        let ids_for = |cust: &str| -> Vec<i64> {
            let mut want = AuxRef::default();
            want.customer = Some(cust.to_string());
            let mut q = VoucherQuery::period(p);
            q.aux = Some(want);
            let mut ids: Vec<i64> = list(&db, &q).unwrap().iter().map(|v| v.id).collect();
            ids.sort();
            ids
        };

        // 对照：不过滤时三张都在
        let all: Vec<i64> = list(&db, &VoucherQuery::period(p))
            .unwrap()
            .iter()
            .map(|v| v.id)
            .collect();
        assert_eq!(all.len(), 3, "对照：不过滤应返回 3 张");

        assert_eq!(ids_for("C001"), vec![id_c001], "C001 不得带上 C0011");
        assert_eq!(ids_for("C0011"), vec![id_c0011]);
        assert_eq!(ids_for("ACME"), vec![id_acme]);
        assert!(ids_for("AC").is_empty(), "AC 是子串不是整段，不应命中 ACME");
        assert!(ids_for("C00").is_empty());
        assert!(ids_for("C002").is_empty(), "不存在的客户不应命中任何凭证");
    }

    #[test]
    fn save_get_delete() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();
        assert!(id > 0);

        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.entries.len(), 2);
        assert_eq!(got.entries[0].debit, Money::parse("1000").unwrap());
        assert_eq!(got.entries[1].aux.bank.as_deref(), Some("B01"));
        assert!(got.balanced());

        delete(&db, id).unwrap();
        assert!(get(&db, id).unwrap().is_none());
    }

    /// 回归：删除凭证的「校验 + 清核销 + 删表」必须整体原子。
    ///
    /// 旧实现把校验与清核销都放在事务外、每条 `unsettle_entry` 各自提交：
    /// 3 条分录里第 3 条清核销失败时，前 2 条核销记录已经被提交并消失，凭证却还在，
    /// 账上留下一张「分录挂账、但核销记录只剩半截」的凭证。
    #[test]
    fn delete_rolls_back_unsettle_on_failure() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 待删凭证：两条应收分录（600 / 700），各自与收款凭证的应收分录核销
        let mut v = Voucher::new(p, d, "记", 1);
        v.prepared_by = "张三".to_string();
        for (i, amt) in ["600", "700"].iter().enumerate() {
            v.push_entry(Entry {
                debit: Money::parse(amt).unwrap(),
                aux: AuxRef { customer: Some("C01".into()), ..Default::default() },
                ..Entry::new(i as i32 * 2 + 1, "112201", "应收")
            });
            v.push_entry(Entry {
                credit: Money::parse(amt).unwrap(),
                ..Entry::new(i as i32 * 2 + 2, "600101", "收入")
            });
        }
        let vid = save(&db, &mut v).unwrap();
        // 应收凭证必须**已记账**：核销的「被核销方」(from 侧) 要求已记账
        // （settle::settle 守卫，全仓 H-3 口径）
        post(&db, vid, "u").unwrap();
        // 收款凭证：贷应收 600 / 700。保持**草稿**——已记账的凭证删不掉，
        // 而本用例要验证的正是「删掉带核销记录的凭证时清核销的原子性」，
        // 所以删的对象取这张收款凭证（核销的 to 侧，允许是草稿：对标金蝶
        // 「单据审核 → 核销 → 生成凭证」三个并列步骤）
        let mut pay = Voucher::new(p, d, "记", 2);
        pay.prepared_by = "张三".to_string();
        pay.push_entry(Entry {
            debit: Money::parse("1300").unwrap(),
            aux: AuxRef { bank: Some("B01".into()), ..Default::default() },
            ..Entry::new(1, "100201", "收款")
        });
        for (i, amt) in ["600", "700"].iter().enumerate() {
            pay.push_entry(Entry {
                credit: Money::parse(amt).unwrap(),
                aux: AuxRef { customer: Some("C01".into()), ..Default::default() },
                ..Entry::new(i as i32 + 2, "112201", "冲应收")
            });
        }
        let pid = save(&db, &mut pay).unwrap();
        let e = entries_of(&db, vid).unwrap();
        let f = entries_of(&db, pid).unwrap();
        crate::settle::settle(&db, e[0].id, f[1].id, Money::parse("600").unwrap(), "u").unwrap();
        crate::settle::settle(&db, e[2].id, f[2].id, Money::parse("700").unwrap(), "u").unwrap();
        let cnt = |db: &Db| -> i64 {
            db.conn().query_row("SELECT COUNT(*) FROM settle_record", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(cnt(&db), 2);

        // 让第 2 条核销记录删不掉：删凭证必须整体失败，且第 1 条核销记录必须还在
        let second: i64 = db
            .conn()
            .query_row("SELECT id FROM settle_record ORDER BY id LIMIT 1 OFFSET 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER t_block BEFORE DELETE ON settle_record
                 WHEN OLD.id = {second} BEGIN SELECT RAISE(ABORT, '核销记录被占用'); END;"
            ))
            .unwrap();
        assert!(delete(&db, pid).is_err(), "清核销失败必须让删除失败");
        assert_eq!(
            cnt(&db),
            2,
            "第 1 条核销记录不得被提交掉（回归前会只剩 1 条，而凭证还在）"
        );
        assert!(get(&db, pid).unwrap().is_some(), "凭证必须还在");

        // 去掉阻塞后删除应成功，核销记录一并清干净
        db.conn().execute_batch("DROP TRIGGER t_block;").unwrap();
        delete(&db, pid).unwrap();
        assert!(get(&db, pid).unwrap().is_none());
        assert_eq!(cnt(&db), 0);
        // 应收凭证本身不受影响
        assert!(get(&db, vid).unwrap().is_some());
    }

    /// 回归：已记账凭证不能删（`AND status<>'posted'` 守卫）。
    #[test]
    fn delete_rejects_posted_voucher() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();
        post(&db, id, "张三").unwrap();
        assert!(delete(&db, id).is_err(), "已记账凭证不得删除");
        assert!(get(&db, id).unwrap().is_some());
        unpost(&db, id).unwrap();
        delete(&db, id).unwrap();
        assert!(get(&db, id).unwrap().is_none());
    }

    #[test]
    fn blank_lines_dropped() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.push_entry(Entry::new(3, "1001", "")); // 空行
        let id = save(&db, &mut v).unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.entries.len(), 2, "空分录不应入库");
    }

    #[test]
    fn line_renumbering() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.push_entry(Entry {
            debit: Money::parse("500").unwrap(),
            ..Entry::new(3, "660101", "广告费")
        });
        v.push_entry(Entry {
            credit: Money::parse("500").unwrap(),
            ..Entry::new(4, "1001", "广告费")
        });
        let id = save(&db, &mut v).unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(
            got.entries.iter().map(|e| e.line).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn reverse_generates_mirror_voucher() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.status = VoucherStatus::Posted;
        v.posted_by = Some("张三".into());
        let id = save(&db, &mut v).unwrap();

        let rid = reverse(&db, id, "李四", p, NaiveDate::from_ymd_opt(2026, 1, 6).unwrap()).unwrap();
        let r = get(&db, rid).unwrap().unwrap();
        // 原凭证不动
        let orig = get(&db, id).unwrap().unwrap();
        assert_eq!(orig.status, VoucherStatus::Posted);
        // 冲销凭证：借贷互换、未记账、摘要带「冲销」前缀
        assert_eq!(r.status, VoucherStatus::Draft);
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].credit, Money::parse("1000").unwrap()); // 原借方 → 冲销贷方
        assert!(r.entries.iter().all(|e| e.summary.starts_with("冲销")));
        assert!(r.balanced());
        assert_eq!(r.no, 2, "冲销凭证应取下一个凭证号");
    }

    #[test]
    fn renumber_compacts_gaps() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v1 = sample(p, 5, 1);
        save(&db, &mut v1).unwrap();
        let mut v2 = sample(p, 6, 2);
        save(&db, &mut v2).unwrap();
        let mut v4 = sample(p, 7, 4);
        save(&db, &mut v4).unwrap(); // 断号 3

        assert_eq!(find_gaps(&db, p, "记").unwrap(), vec![3]);
        let n = renumber(&db, p, "记").unwrap();
        assert_eq!(n, 3);
        assert!(find_gaps(&db, p, "记").unwrap().is_empty());
        // 按日期顺序重排后，各张编号 1..=3
        let rows = list(&db, &VoucherQuery::period(p)).unwrap();
        assert_eq!(rows.iter().map(|v| v.no).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn flow_post_unpost() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();

        // 无审核环节：未记账凭证直接记账
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Draft);
        post(&db, id, "张三").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
        unpost(&db, id).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Draft);
        // 反记账后再记账
        post(&db, id, "张三").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
    }

    #[test]
    fn next_no_and_gaps() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        assert_eq!(next_no(&db, p, "记").unwrap(), 1);
        let mut v = sample(p, 5, 1);
        save(&db, &mut v).unwrap();
        assert_eq!(next_no(&db, p, "记").unwrap(), 2);
        let mut v3 = sample(p, 6, 3);
        save(&db, &mut v3).unwrap();
        assert_eq!(find_gaps(&db, p, "记").unwrap(), vec![2]);
        assert_eq!(next_no(&db, p, "记").unwrap(), 4);
    }

    #[test]
    fn query_filters() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v1 = sample(p, 5, 1);
        save(&db, &mut v1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        let mut v2 = sample(p2, 5, 1);
        save(&db, &mut v2).unwrap();

        assert_eq!(list(&db, &VoucherQuery::period(p)).unwrap().len(), 1);
        assert_eq!(list(&db, &VoucherQuery::default()).unwrap().len(), 2);
        assert_eq!(
            list(&db, &VoucherQuery::default().with_status(Some(VoucherStatus::Draft)))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            list(&db, &VoucherQuery::default().with_keyword("提取")).unwrap().len(),
            2
        );
        assert_eq!(
            list(&db, &VoucherQuery {
                account_code: Some("1001".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            list(&db, &VoucherQuery {
                aux: Some(AuxRef { bank: Some("B01".into()), ..Default::default() }),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn status_counts() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        save(&db, &mut v).unwrap();
        assert_eq!(status_summary(&db, p).unwrap(), (1, 0, 0, 0));
    }

    #[test]
    fn data_scope_own_voucher_only() {
        use fincore::user::DataScope;

        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // prepared_by 存的是登录账号（username），与保存侧一致
        let mut v1 = sample(p, 5, 1);
        v1.prepared_by = "zhangsan".to_string();
        save(&db, &mut v1).unwrap();
        let mut v2 = sample(p, 6, 2);
        v2.prepared_by = "lisi".to_string();
        save(&db, &mut v2).unwrap();

        // zhangsan：只看到自己填制的 1 张（display_name 与 username 不同，仍应按 username 过滤）
        let mut zhang = fincore::User::new("zhangsan", "张三", fincore::Role::Accountant);
        zhang.data_scope = DataScope {
            own_voucher_only: true,
            ..Default::default()
        };
        let rows = list(&db, &VoucherQuery::period(p).with_data_scope(&zhang)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].prepared_by, "zhangsan");

        // 管理员/无限制用户：2 张都可见
        let admin = fincore::User::new("admin", "管理员", fincore::Role::Admin);
        let rows = list(&db, &VoucherQuery::period(p).with_data_scope(&admin)).unwrap();
        assert_eq!(rows.len(), 2);
    }
}
