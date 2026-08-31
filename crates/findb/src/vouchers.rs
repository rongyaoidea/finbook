//! 凭证仓储
//!
//! 凭证的保存是"先存表头、再整表替换分录"的两步事务——分录整表替换比逐行 diff 简单得多，
//! 单机场景下性能完全够用，还能避免残留孤儿行。

use chrono::NaiveDate;
use fincore::{
    AuxRef, Entry, FinError, Issues, Money, Period, Voucher, VoucherSource, VoucherStatus,
};
use rusqlite::OptionalExtension;

use crate::{money_param, read_date_opt, read_money, read_money_opt, Db, DbError, DbResult};

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
    let mut v = db
        .conn()
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?;
    if let Some(ref mut v) = v {
        v.entries = entries_of(db, id)?;
    }
    Ok(v)
}

pub fn entries_of(db: &Db, voucher_id: i64) -> DbResult<Vec<Entry>> {
    let mut stmt = db.conn().prepare(
        "SELECT id,voucher_id,line,summary,account_code,aux_json,debit,credit,qty,price,
                currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item
         FROM voucher_entry WHERE voucher_id=?1 ORDER BY line",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![voucher_id], map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
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
    if let Some(ref code) = q.account_code {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id AND e.account_code LIKE ?)",
        );
        params.push(Box::new(format!("{code}%")));
    }
    if let Some(ref aux) = q.aux {
        let key = aux.key();
        if !key.is_empty() {
            sql.push_str(
                " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id AND e.aux_key LIKE ?)",
            );
            params.push(Box::new(format!("%{key}%")));
        }
    }
    if let Some(ref kw) = q.keyword {
        sql.push_str(
            " AND (v.memo LIKE ? OR CAST(v.no AS TEXT) LIKE ?
                   OR EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (e.summary LIKE ? OR e.account_code LIKE ?)))",
        );
        let k = format!("%{kw}%");
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
    // ---- 0. 前置守卫 ----
    let closed = crate::periods::closed_upto(db)?;
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
        // 更新：只有草稿能改；已审核需先反审核，已记账需先反记账
        let old = get(db, v.id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{}", v.id)))?;
        if !old.status.can_edit() {
            return Err(FinError::state(format!(
                "凭证当前状态为「{}」，不能修改（请先反审核 / 反记账）",
                old.status.label()
            ))
            .into());
        }
    } else if v.status != VoucherStatus::Draft {
        return Err(FinError::state(format!(
            "新增凭证的状态必须是「草稿」，实际为「{}」",
            v.status.label()
        ))
        .into());
    }
    if no_taken(db, v.period, &v.word, v.no, v.id)? {
        return Err(FinError::msg(format!(
            "{}-{:04} 已存在，请更换凭证号",
            v.word, v.no
        ))
        .into());
    }
    let chart = crate::accounts::chart(db)?;
    let opts = db.options();
    fincore::engine::validate_for_save(v, &fincore::engine::ValidateCtx::new(&chart, &opts, closed))
        .into_result()?;

    // ---- 1. 落库 ----
    let tx = db.conn().unchecked_transaction()?;
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
            e.qty.map(money_param),
            e.price.map(money_param),
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
    tx.commit()?;
    v.id = id;
    Ok(id)
}

/// 删除凭证（连同分录，外键 ON DELETE CASCADE）
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM voucher WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 下一个可用凭证号
pub fn next_no(db: &Db, period: Period, word: &str) -> DbResult<i32> {
    let max: Option<i32> = db
        .conn()
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

fn set_status(db: &Db, id: i64, s: VoucherStatus) -> DbResult<()> {
    db.conn().execute(
        "UPDATE voucher SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            id,
            serde_json::to_value(s)?.as_str().unwrap_or("draft"),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// 审核（并写审核人）
pub fn audit(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let mut iss = fincore::engine::validate_audit(&v, who);
    if !v.balanced() {
        iss.push("凭证借贷不平衡，不能审核".to_string());
    }
    iss.into_result()?;
    db.conn().execute(
        "UPDATE voucher SET status='audited', audited_by=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            id,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    db.log(who, "凭证", "审核", &v.voucher_no())?;
    Ok(())
}

/// 反审核
pub fn unaudit(db: &Db, id: i64) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let mut iss = Issues::new();
    if v.status != VoucherStatus::Audited {
        iss.push(format!("凭证当前状态为「{}」，不能反审核", v.status.label()));
    }
    iss.into_result()?;
    db.conn().execute(
        "UPDATE voucher SET status='draft', audited_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()],
    )?;
    db.log(v.audited_by.as_deref().unwrap_or_default(), "凭证", "反审核", &v.voucher_no())?;
    Ok(())
}

/// 记账
pub fn post(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let opts = db.options();
    let mut iss = fincore::engine::validate_post(&v, &opts);
    if !v.balanced() {
        iss.push("凭证借贷不平衡，不能记账".to_string());
    }
    if let Some(upto) = crate::periods::closed_upto(db)? {
        if v.period <= upto {
            iss.push(format!("{} 及以前期间已结账", upto.label()));
        }
    }
    iss.into_result()?;
    db.conn().execute(
        "UPDATE voucher SET status='posted', posted_by=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            id,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    db.log(who, "凭证", "记账", &v.voucher_no())?;
    Ok(())
}

/// 反记账
pub fn unpost(db: &Db, id: i64) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let iss = fincore::engine::validate_unpost(&v, crate::periods::closed_upto(db)?);
    iss.into_result()?;
    db.conn().execute(
        "UPDATE voucher SET status='audited', posted_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()],
    )?;
    db.log(v.posted_by.as_deref().unwrap_or_default(), "凭证", "反记账", &v.voucher_no())?;
    Ok(())
}

/// 出纳签字
pub fn cashier_sign(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let mut iss = Issues::new();
    if v.status != VoucherStatus::Draft && v.status != VoucherStatus::Audited {
        iss.push(format!("凭证当前状态为「{}」，不能签字", v.status.label()));
    }
    if !v.entries.iter().any(|e| !e.is_blank()) {
        iss.push("凭证无内容".to_string());
    }
    iss.into_result()?;
    db.conn().execute(
        "UPDATE voucher SET cashier=?2 WHERE id=?1",
        rusqlite::params![id, who],
    )?;
    db.log(who, "凭证", "出纳签字", &v.voucher_no())?;
    Ok(())
}

/// 作废 / 恢复。作废与取消作废都会记入操作日志。
pub fn set_void(db: &Db, id: i64, void: bool, who: &str) -> DbResult<()> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if void {
        let iss = fincore::engine::validate_void(&v);
        iss.into_result()?;
        set_status(db, id, VoucherStatus::Void)?;
    } else {
        if v.status != VoucherStatus::Void {
            return Err(FinError::state("该凭证未处于作废状态").into());
        }
        let back = if v.posted_by.is_some() {
            VoucherStatus::Posted
        } else if v.audited_by.is_some() {
            VoucherStatus::Audited
        } else {
            VoucherStatus::Draft
        };
        set_status(db, id, back)?;
    }
    db.log(
        who,
        "凭证",
        if void { "作废" } else { "取消作废" },
        &v.voucher_no(),
    )?;
    Ok(())
}

/// 批量审核
pub fn audit_many(db: &Db, ids: &[i64], who: &str) -> DbResult<(usize, Vec<String>)> {
    let mut ok = 0usize;
    let mut errs = Vec::new();
    let mut labels = Vec::new();
    let tx = db.conn().unchecked_transaction()?;
    for id in ids {
        match audit_tx(&tx, *id, who) {
            Ok(label) => {
                ok += 1;
                labels.push(label);
            }
            Err(e) => errs.push(format!("#{id}：{e}")),
        }
    }
    tx.commit()?;
    if !labels.is_empty() {
        db.log(who, "凭证", "批量审核", &labels.join("、"))?;
    }
    Ok((ok, errs))
}

/// 返回该凭证的凭证号，供调用方写操作日志
fn audit_tx(tx: &rusqlite::Transaction, id: i64, who: &str) -> Result<String, DbError> {
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let mut iss = fincore::engine::validate_audit(&v, who);
    if !v.balanced() {
        iss.push("凭证借贷不平衡，不能审核".to_string());
    }
    iss.into_result()?;
    tx.execute(
        "UPDATE voucher SET status='audited', audited_by=?2 WHERE id=?1",
        rusqlite::params![id, who],
    )?;
    Ok(v.voucher_no())
}

/// 批量记账
pub fn post_many(db: &Db, ids: &[i64], who: &str) -> DbResult<(usize, Vec<String>)> {
    let mut ok = 0usize;
    let mut errs = Vec::new();
    let mut labels = Vec::new();
    let tx = db.conn().unchecked_transaction()?;
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
    if v.status != VoucherStatus::Audited {
        return Err(FinError::state(format!("状态为「{}」，不能记账", v.status.label())).into());
    }
    // 与单条记账保持同一套把关：借贷必须平衡，期间不能已结账
    if !v.balanced() {
        return Err(FinError::state("凭证借贷不平衡，不能记账").into());
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
         WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2",
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
    fn flow_audit_post() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();

        // 制单人与审核人不能是同一人
        assert!(audit(&db, id, "张三").is_err());
        // 未审核不能记账
        assert!(post(&db, id, "李四").is_err());
        audit(&db, id, "李四").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Audited);
        post(&db, id, "王五").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
        unpost(&db, id).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Audited);
        unaudit(&db, id).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Draft);
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
}
