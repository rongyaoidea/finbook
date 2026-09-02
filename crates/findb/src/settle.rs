//! 往来核销与账龄分析
//!
//! 核销是"单据级"的：一张应收单可以分多次收款核销，也可以一笔款核销多张单。
//! 所以这里存的是**分录 ↔ 分录 + 金额**，而不是"客户整体结清"。

use chrono::NaiveDate;
use fincore::engine::aging::{AgingBucket, AgingItem, AgingLine};
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

/// 一条核销记录
#[derive(Clone, Debug)]
pub struct SettleRecord {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub aux_key: String,
    /// 被核销的分录（原单据）
    pub from_entry: i64,
    /// 核销方分录（收款 / 付款）
    pub to_entry: i64,
    pub amount: Money,
    pub settled_by: String,
    pub settled_at: String,
}

fn map_rec(r: &rusqlite::Row) -> rusqlite::Result<SettleRecord> {
    Ok(SettleRecord {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        aux_key: r.get(3)?,
        from_entry: r.get(4)?,
        to_entry: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        settled_by: r.get(7)?,
        settled_at: r.get(8)?,
    })
}

const COLS: &str = "id,period,account_code,aux_key,from_entry,to_entry,amount,settled_by,settled_at";

/// 某科目的全部核销记录
pub fn list(db: &Db, account: &str) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE account_code=?1 ORDER BY period, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![account], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 涉及某分录的全部核销
pub fn list_for_entry(db: &Db, entry_id: i64) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE from_entry=?1 OR to_entry=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![entry_id], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某分录已被核销掉的金额
pub fn settled_of(db: &Db, entry_id: i64) -> DbResult<Money> {
    // 金额在库里是 TEXT，SUM 会走浮点，精度不可控；这里取行在 Rust 侧用定点累加
    let mut st = db.conn().prepare(
        "SELECT amount FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
    )?;
    let rows = st
        .query_map(rusqlite::params![entry_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sum = Money::ZERO;
    for s in rows {
        sum += Money::parse_or_zero(&s);
    }
    Ok(sum)
}

/// 批量取多个分录的已核销额（避免 N+1 查询）
pub fn settled_map(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut out = std::collections::HashMap::new();
    if entry_ids.is_empty() {
        return Ok(out);
    }
    let ph = entry_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // 同样不能用 SUM()：TEXT 列求和会走 REAL，分位精度不可控
    let sql = format!(
        "SELECT k, amount FROM (
            SELECT from_entry AS k, id, amount FROM settle_record WHERE from_entry IN ({ph})
            UNION ALL
            SELECT to_entry AS k, id, amount FROM settle_record WHERE to_entry IN ({ph})
         ) ORDER BY k"
    );
    let mut st = db.conn().prepare(&sql)?;
    let ids2: Vec<&dyn rusqlite::ToSql> = entry_ids
        .iter()
        .flat_map(|i| [i as &dyn rusqlite::ToSql, i as &dyn rusqlite::ToSql])
        .collect();
    let rows = st
        .query_map(ids2.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, Money::parse_or_zero(&r.get::<_, String>(1)?)))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (k, v) in rows {
        *out.entry(k).or_insert(Money::ZERO) += v;
    }
    Ok(out)
}

fn settled_map_stub(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
    settled_map(db, entry_ids)
}

/// 手工核销。会自动校验：两边分录同科目、同往来单位、金额不超过各自未核销额。
pub fn settle(
    db: &Db,
    from_entry: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    if from_entry == to_entry {
        return Err(fincore::FinError::msg("不能把分录核销到自己身上").into());
    }
    if amount <= Money::ZERO {
        return Err(fincore::FinError::msg("核销金额必须大于零").into());
    }
    let f = match entry_of(db, from_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("被核销分录").into()),
    };
    let t = match entry_of(db, to_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("核销方分录").into()),
    };
    if f.account_code != t.account_code {
        return Err(fincore::FinError::msg(format!(
            "两条分录不在同一科目（{} vs {}），不能核销",
            f.account_code, t.account_code
        ))
        .into());
    }
    if f.aux_key != t.aux_key {
        return Err(fincore::FinError::msg(
            "两条分录的往来单位不一致，不能核销",
        )
        .into());
    }
    // 检查超额
    let open_f = f.signed().abs() - settled_of(db, from_entry)?;
    let open_t = t.signed().abs() - settled_of(db, to_entry)?;
    if amount > open_f {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过被核销方未核销额 {open_f}"
        ))
        .into());
    }
    if amount > open_t {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过核销方未核销额 {open_t}"
        ))
        .into());
    }

    // 读取现有累计核销额（TEXT 列，避免在 SQL 侧做字符串加法）
    let existing = db
        .conn()
        .query_row(
            "SELECT amount FROM settle_record WHERE from_entry=?1 AND to_entry=?2",
            rusqlite::params![from_entry, to_entry],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();
    let old_amount = Money::parse_or_zero(&existing);
    let new_amount = old_amount + amount;

    db.conn().execute(
        "INSERT INTO settle_record(period,account_code,aux_key,from_entry,to_entry,amount,
            settled_by,settled_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(from_entry,to_entry) DO UPDATE SET
             amount=excluded.amount, settled_by=excluded.settled_by, settled_at=excluded.settled_at",
        rusqlite::params![
            f.period.ymm(),
            f.account_code,
            f.aux_key,
            from_entry,
            to_entry,
            crate::money_param(new_amount), // 无千分位的定点数，保证后续计算正确
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn unsettle(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM settle_record WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 清除某分录的全部核销（删改凭证时用）
pub fn unsettle_entry(db: &Db, entry_id: i64) -> DbResult<usize> {
    Ok(db.conn().execute(
        "DELETE FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
        rusqlite::params![entry_id],
    )?)
}

// ---------------- 往来单据 ----------------

/// 一条往来分录（含已核销额）
#[derive(Clone, Debug)]
pub struct OpenEntry {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub word: String,
    pub no: i32,
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    pub aux_key: String,
    pub settle_no: String,
    pub debit: Money,
    pub credit: Money,
    pub settled: Money,
}

impl OpenEntry {
    /// 带符号金额（借方为正）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    /// 未核销额（带符号）
    pub fn open(&self) -> Money {
        let o = self.signed().abs() - self.settled;
        if o < Money::parse("0.005").unwrap() {
            Money::ZERO
        } else {
            o
        }
    }
    pub fn is_open(&self) -> bool {
        !self.open().is_zero()
    }
    /// 方向标签
    pub fn dir_label(&self) -> &'static str {
        if self.debit > Money::ZERO {
            "借"
        } else {
            "贷"
        }
    }
}

fn entry_of(db: &Db, entry_id: i64) -> DbResult<Option<OpenEntry>> {
    db.conn()
        .query_row(
            "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                    e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.id=?1",
            rusqlite::params![entry_id],
            |r| {
                let d: String = r.get(3)?;
                Ok(OpenEntry {
                    entry_id: r.get(0)?,
                    voucher_id: r.get(1)?,
                    period: Period::from_ymm(r.get(2)?),
                    date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                    word: r.get(4)?,
                    no: r.get(5)?,
                    line: r.get(6)?,
                    summary: r.get(7)?,
                    account_code: r.get(8)?,
                    aux_key: r.get(9)?,
                    settle_no: r.get(10)?,
                    debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                    credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                    settled: Money::ZERO,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// 取某科目（含下级）截至某日的全部已记账往来分录，并填充已核销额
pub fn open_entries(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
) -> DbResult<Vec<OpenEntry>> {
    let mut st = db.conn().prepare(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE (e.account_code = ?1 OR e.account_code LIKE ?1||'%')
           AND v.period <= ?2 AND v.status != 'void'
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY v.date, v.no, e.line",
    )?;
    let mut rows = st
        .query_map(rusqlite::params![account, upto.ymm()], |r| {
            let d: String = r.get(3)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let ids: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
    let sm = settled_map_stub(db, &ids)?;
    for r in rows.iter_mut() {
        r.settled = sm.get(&r.entry_id).copied().unwrap_or(Money::ZERO);
    }
    if !include_all {
        rows.retain(|r| r.is_open());
    }
    Ok(rows)
}

// ---------------- 自动核销 ----------------

/// 自动核销结果
#[derive(Clone, Debug, Default)]
pub struct AutoSettleResult {
    pub pairs: usize,
    pub amount: Money,
    /// 同金额精确匹配的笔数
    pub exact: usize,
    /// 尾数清零（余额小于阈值直接抹平）的笔数
    pub written_off: usize,
}

/// 自动核销
///
/// 策略（保守优先，宁可少勾也不错勾）：
/// 1. 同一往来单位内，先找**金额完全相等的**一借一贷配对（最可靠）
/// 2. 剩余部分尝试用一笔收款逐条吃掉最早的单据（FIFO），吃掉后余额小于阈值则抹平
/// 3. 匹配不上的全部留空，交给手工
pub fn auto_settle(
    db: &Db,
    account: &str,
    upto: Period,
    tolerance: Money,
    who: &str,
) -> DbResult<AutoSettleResult> {
    let mut res = AutoSettleResult::default();
    let all = open_entries(db, account, upto, false)?;

    // 按往来单位分组
    let mut groups: std::collections::BTreeMap<String, Vec<OpenEntry>> =
        std::collections::BTreeMap::new();
    for e in all {
        groups.entry(e.aux_key.clone()).or_default().push(e);
    }

    for (_key, mut g) in groups {
        // 第一轮：精确等额配对
        let mut i = 0usize;
        while i < g.len() {
            if g[i].open().is_zero() {
                i += 1;
                continue;
            }
            let dir_pos = g[i].debit > Money::ZERO;
            let amt = g[i].open();
            let hit = g
                .iter()
                .position(|x| {
                    x.entry_id != g[i].entry_id
                        && (x.debit > Money::ZERO) != dir_pos
                        && x.open() == amt
                        && !x.open().is_zero()
                });
            if let Some(j) = hit {
                let (from, to) = if dir_pos { (g[i].entry_id, g[j].entry_id) } else { (g[j].entry_id, g[i].entry_id) };
                let id = settle(db, from, to, amt, who)?;
                let _ = id;
                res.pairs += 1;
                res.exact += 1;
                res.amount += amt;
                // 重新读一次，保证 open() 是最新的
                g = open_entries_of(db, &g)?;
                i = 0;
                continue;
            }
            i += 1;
        }

        // 第二轮：FIFO 逐笔吃掉，余额小于容差则抹平
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 200 {
                break;
            }
            g = open_entries_of(db, &g)?;
            let debtors: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.debit > Money::ZERO && x.is_open())
                .collect();
            let credits: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.credit > Money::ZERO && x.is_open())
                .collect();
            if debtors.is_empty() || credits.is_empty() {
                break;
            }
            let d = debtors[0];
            let c = credits[0];
            let amt = d.open().min(c.open());
            if amt.is_zero() {
                break;
            }
            let id = settle(db, d.entry_id, c.entry_id, amt, who)?;
            let _ = id;
            res.pairs += 1;
            res.amount += amt;
            // 吃掉后若剩余小于容差，直接抹平
            let d_left = d.open() - amt;
            let c_left = c.open() - amt;
            for (left, id) in [(d_left, d.entry_id), (c_left, c.entry_id)] {
                if !left.is_zero() && left <= tolerance {
                    // 找反向未结清分录把尾数吃掉
                    if let Some(other) = g.iter().find(|x| {
                        x.entry_id != id && x.is_open() && (x.debit > Money::ZERO) != (d.debit > Money::ZERO)
                    }) {
                        let _ = settle(db, id, other.entry_id, left, who);
                        res.pairs += 1;
                        res.written_off += 1;
                        res.amount += left;
                    }
                }
            }
        }
    }
    Ok(res)
}

/// 用当前库里的数据刷新一批分录的已核销额（保留原顺序与范围）
fn open_entries_of(db: &Db, old: &[OpenEntry]) -> DbResult<Vec<OpenEntry>> {
    let ids: Vec<i64> = old.iter().map(|o| o.entry_id).collect();
    let sm = settled_map(db, &ids)?;
    let mut out = Vec::with_capacity(old.len());
    let mut st = db.conn().prepare(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id WHERE e.id=?1",
    )?;
    for id in ids {
        let mut e: OpenEntry = st.query_row(rusqlite::params![id], |r| {
            let d: String = r.get(3)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
            })
        })?;
        e.settled = sm.get(&id).copied().unwrap_or(Money::ZERO);
        out.push(e);
    }
    Ok(out)
}

// ---------------- 账龄 ----------------

/// 账龄分析
pub fn aging(
    db: &Db,
    account: &str,
    upto: Period,
    as_of: NaiveDate,
    buckets: &[AgingBucket],
) -> DbResult<Vec<AgingLine>> {
    let entries = open_entries(db, account, upto, false)?;
    let items: Vec<AgingItem> = entries
        .iter()
        .map(|e| AgingItem {
            key: e.aux_key.clone(),
            date: e.date,
            amount: e.signed(),
            settled: e.settled,
            doc_no: format!("{}-{}", e.word, e.no),
        })
        .collect();
    Ok(fincore::engine::aging::analyze(&items, as_of, buckets)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::voucher::{AuxRef, Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_settle_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    /// 建一张已记账的往来凭证
    fn ar_voucher(
        db: &Db,
        period: Period,
        date: NaiveDate,
        no: i32,
        cust: &str,
        amount: &str,
        dir_debit: bool,
        counterpart: &str,
    ) -> (i64, i64) {
        let mut v = Voucher::new(Period::from_date(date), date, "记", no);
        let a = Money::parse(amount).unwrap();
        let e1 = Entry {
            debit: if dir_debit { a } else { Money::ZERO },
            credit: if dir_debit { Money::ZERO } else { a },
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "往来")
        };
        let e2 = Entry {
            debit: if dir_debit { Money::ZERO } else { a },
            credit: if dir_debit { a } else { Money::ZERO },
            aux: if counterpart == "100201" {
                AuxRef {
                    bank: Some("BANK01".into()),
                    ..Default::default()
                }
            } else {
                AuxRef::default()
            },
            ..Entry::new(2, counterpart, "往来")
        };
        let _ = period;
        v.push_entry(e1);
        v.push_entry(e2);
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "poster").unwrap();
        let entries = crate::vouchers::entries_of(db, vid).unwrap();
        (vid, entries[0].id)
    }

    #[test]
    fn manual_settle_flow() {
        let db = tmpdb("manual");
        let p = Period::new(2026, 1).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let (_, e_ar) = ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        let (_, e_cash) = ar_voucher(&db, p, d2, 2, "C01", "600", false, "100201");

        settle(&db, e_ar, e_cash, Money::parse("600").unwrap(), "u1").unwrap();
        assert_eq!(settled_of(&db, e_ar).unwrap(), Money::parse("600").unwrap());

        // 超额核销要拦
        assert!(settle(&db, e_ar, e_cash, Money::parse("500").unwrap(), "u1").is_err());
        // 自己核销自己要拦
        assert!(settle(&db, e_ar, e_ar, Money::parse("10").unwrap(), "u1").is_err());

        let opens = open_entries(&db, "1122", p, false).unwrap();
        assert_eq!(opens.len(), 1); // 收款那条已被核完
        assert_eq!(opens[0].open(), Money::parse("400").unwrap());

        // 取消核销后恢复
        let recs = list_for_entry(&db, e_ar).unwrap();
        unsettle(&db, recs[0].id).unwrap();
        assert_eq!(settled_of(&db, e_ar).unwrap(), Money::ZERO);
    }

    #[test]
    fn cross_account_rejected() {
        let db = tmpdb("cross");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (_, e1) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
        let (_, e2) = ar_voucher(&db, p, d, 2, "C02", "100", false, "100201");
        // 往来单位不同
        assert!(settle(&db, e1, e2, Money::parse("100").unwrap(), "u").is_err());
    }

    #[test]
    fn auto_settle_exact_then_fifo() {
        let db = tmpdb("auto");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 两笔应收各 500，一笔收款 800
        ar_voucher(&db, p, d, 1, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 2, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 3, "C01", "800", false, "100201");

        let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
        assert!(res.pairs >= 2, "{res:?}");
        let opens = open_entries(&db, "1122", p, false).unwrap();
        let total: Money = opens.iter().map(|o| o.open()).sum();
        assert_eq!(total, Money::parse("200").unwrap());
    }

    #[test]
    fn aging_report() {
        let db = tmpdb("aging");
        let p = Period::new(2026, 3).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        ar_voucher(&db, p, d2, 2, "C01", "2000", true, "600101");

        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let b = fincore::engine::aging::buckets_by_days();
        let lines = aging(&db, "1122", p, as_of, &b).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total, Money::parse("3000").unwrap());
        assert_eq!(lines[0].amounts[0], Money::parse("1000").unwrap()); // 11 天
        // 2025-06-01 → 2026-03-31 约 303 天，落在 181-365 档
        assert_eq!(lines[0].amounts[4], Money::parse("2000").unwrap());
    }
}
