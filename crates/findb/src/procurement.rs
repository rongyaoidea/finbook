//! 采购深化：请购单 / 到货 / 付款 / 退货 / 历史价格 / 统计 / 执行跟踪
//!
//! 对标金蝶/用友采购管理。金额一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 采购请购单
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PurchaseReq {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub item_code: String,
    pub item_name: String,
    pub qty: Money,
    pub status: String, // draft / approved / ordered / cancelled
    pub requester: String,
    pub memo: String,
}

fn map_req(r: &rusqlite::Row) -> rusqlite::Result<PurchaseReq> {
    Ok(PurchaseReq {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&r.get::<_, String>(3)?, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        item_code: r.get(4)?,
        item_name: r.get(5)?,
        qty: m(&r.get::<_, String>(6)?),
        status: r.get(7)?,
        requester: r.get(8)?,
        memo: r.get(9)?,
    })
}

const R_COLS: &str = "id,no,period,date,item_code,item_name,qty,status,requester,memo";

pub fn pr_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("QG{:04}{:02}", period.year(), period.month());
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM purchase_req WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn pr_save(db: &Db, r: &mut PurchaseReq) -> DbResult<i64> {
    let id = if r.id > 0 {
        db.conn().execute(
            "UPDATE purchase_req SET period=?2, date=?3, item_code=?4, item_name=?5,
             qty=?6, status=?7, requester=?8, memo=?9 WHERE id=?1",
            rusqlite::params![
                r.id, r.period.ymm(), r.date.format("%Y-%m-%d").to_string(),
                r.item_code, r.item_name, crate::exact_param(r.qty), r.status, r.requester, r.memo
            ],
        )?;
        r.id
    } else {
        db.conn().execute(
            "INSERT INTO purchase_req(no,period,date,item_code,item_name,qty,status,requester,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                r.no, r.period.ymm(), r.date.format("%Y-%m-%d").to_string(),
                r.item_code, r.item_name, crate::exact_param(r.qty), r.status, r.requester, r.memo
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    r.id = id;
    Ok(id)
}

pub fn pr_list(db: &Db, period: Period) -> DbResult<Vec<PurchaseReq>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {R_COLS} FROM purchase_req WHERE period=?1 ORDER BY id DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_req)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn pr_get(db: &Db, id: i64) -> DbResult<Option<PurchaseReq>> {
    db.conn()
        .query_row(&format!("SELECT {R_COLS} FROM purchase_req WHERE id=?1"), [id], map_req)
        .optional()
        .map_err(Into::into)
}

/// 请购单审批：draft → approved
pub fn pr_approve(db: &Db, id: i64) -> DbResult<()> {
    let r = pr_get(db, id)?.ok_or_else(|| fincore::FinError::msg("请购单不存在"))?;
    if r.status != "draft" {
        return Err(fincore::FinError::msg(format!("请购单已{}，不能审批", r.status)).into());
    }
    db.conn().execute(
        "UPDATE purchase_req SET status='approved' WHERE id=?1",
        [id],
    )?;
    Ok(())
}

// ===========================================================================
// 到货 / 付款 / 退货
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PoReceipt {
    pub id: i64,
    pub po_id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub qty: Money,
    pub memo: String,
}

pub fn po_receipt_add(db: &Db, r: &PoReceipt) -> DbResult<i64> {
    if r.qty.is_negative() || r.qty.is_zero() {
        return Err(fincore::FinError::msg("到货数量必须为正数").into());
    }
    let id = db.conn().execute(
        "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![r.po_id, r.period.ymm(), r.date.format("%Y-%m-%d").to_string(), crate::exact_param(r.qty), r.memo],
    )?;
    // 执行进度以 po_receipt 流水累计为准（Rust 侧 Decimal），不落冗余字段
    Ok(db.conn().last_insert_rowid())
}

pub fn po_receipt_list(db: &Db, po_id: i64) -> DbResult<Vec<PoReceipt>> {
    let mut st = db.conn().prepare(
        "SELECT id,po_id,period,date,qty,memo FROM po_receipt WHERE po_id=?1 ORDER BY id"
    )?;
    let rows = st
        .query_map([po_id], |r| {
            Ok(PoReceipt {
                id: r.get(0)?,
                po_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&r.get::<_, String>(3)?, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                qty: m(&r.get::<_, String>(4)?),
                memo: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 采购退货：生成负到货记录（数量为负），冲减收货
pub fn po_return_add(db: &Db, po_id: i64, period: Period, date: NaiveDate, qty: Money, memo: &str) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    let id = db.conn().execute(
        "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![po_id, period.ymm(), date.format("%Y-%m-%d").to_string(), qty.negated().to_string(), format!("退货 {}", memo)],
    )?;
    Ok(db.conn().last_insert_rowid())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PoPayment {
    pub id: i64,
    pub po_id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub amount: Money,
    pub memo: String,
}

pub fn po_payment_add(db: &Db, p: &PoPayment) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO po_payment(po_id,period,date,amount,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![p.po_id, p.period.ymm(), p.date.format("%Y-%m-%d").to_string(), crate::money_param(p.amount), p.memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn po_payment_sum(db: &Db, po_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT amount FROM po_payment WHERE po_id=?1")?;
    let rows = st
        .query_map([po_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

// ===========================================================================
// 历史价格 / 统计 / 执行跟踪
// ===========================================================================

/// 记录历史价格（采购订单保存时调用）
pub fn price_history_record(db: &Db, item_code: &str, supplier_code: &str, unit_price: Money, date: NaiveDate) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO price_history(item_code,supplier_code,unit_price,date) VALUES(?1,?2,?3,?4)
         ON CONFLICT(item_code,supplier_code,date) DO UPDATE SET unit_price=excluded.unit_price",
        rusqlite::params![item_code, supplier_code, crate::exact_param(unit_price), date.format("%Y-%m-%d").to_string()],
    )?;
    Ok(())
}

/// 某物料的历史价格（按日期倒序）
pub fn price_history(db: &Db, item_code: &str) -> DbResult<Vec<(String, Money, String)>> {
    let mut st = db.conn().prepare(
        "SELECT supplier_code, unit_price, date FROM price_history WHERE item_code=?1 ORDER BY date DESC"
    )?;
    let rows = st
        .query_map([item_code], |r| Ok((r.get(0)?, m(&r.get::<_, String>(1)?), r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 采购统计：期间内按供应商汇总采购金额
#[derive(Clone, Debug, serde::Serialize)]
pub struct PurchaseStat {
    pub supplier_code: String,
    pub supplier_name: String,
    pub order_count: i64,
    pub amount: Money,
}

pub fn purchase_stats(db: &Db, period: Period) -> DbResult<Vec<PurchaseStat>> {
    let orders = crate::scm::po_list(db, period, None)?;
    let mut map: std::collections::BTreeMap<String, PurchaseStat> = std::collections::BTreeMap::new();
    for o in orders {
        let e = map.entry(o.supplier_code.clone()).or_insert_with(|| PurchaseStat {
            supplier_code: o.supplier_code.clone(),
            supplier_name: o.supplier_name.clone(),
            order_count: 0,
            amount: Money::ZERO,
        });
        e.order_count += 1;
        e.amount += o.total_amount;
    }
    Ok(map.into_values().collect())
}

/// 采购订单执行跟踪：到货数量 vs 订单数量
#[derive(Clone, Debug, serde::Serialize)]
pub struct PoTrack {
    pub po_id: i64,
    pub no: String,
    pub supplier_name: String,
    pub ordered_qty: Money,
    pub received_qty: Money,
    /// 执行率（%）
    pub rate: Money,
}

pub fn po_execution_track(db: &Db, period: Period) -> DbResult<Vec<PoTrack>> {
    let orders = crate::scm::po_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::PoStatus::Cancelled) {
            continue;
        }
        let ordered: Money = o.lines.iter().map(|l| l.qty_ordered).sum();
        let mut received = Money::ZERO;
        for rc in po_receipt_list(db, o.id)? {
            received += rc.qty;
        }
        let rate = if ordered.is_zero() {
            Money::ZERO
        } else {
            ((received.abs() * Money::from_i64(100)) / ordered.abs().inner()).round2()
        };
        out.push(PoTrack {
            po_id: o.id,
            no: o.no,
            supplier_name: o.supplier_name,
            ordered_qty: ordered,
            received_qty: received,
            rate,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn requisition_flow() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut r = PurchaseReq {
            id: 0, no: pr_next_no(&db, p).unwrap(), period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            item_code: "140301".into(), item_name: "原料".into(),
            qty: m("100"), status: "draft".into(), requester: "张三".into(), memo: String::new(),
        };
        let id = pr_save(&db, &mut r).unwrap();
        assert!(id > 0);
        pr_approve(&db, id).unwrap();
        assert_eq!(pr_get(&db, id).unwrap().unwrap().status, "approved");
        // 重复审批报错
        assert!(pr_approve(&db, id).is_err());
        assert_eq!(pr_list(&db, p).unwrap().len(), 1);
    }

    #[test]
    fn receipt_payment_and_stats() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "S01", "供应商A", "u");
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0, po_id: 0, item_code: "140301".into(), item_name: "原料".into(),
            qty_ordered: m("100"), qty_received: m("0"), unit_price: m("10"),
            tax_rate: m("0"), amount: m("1000"), tax_amount: m("0"), memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();
        // 到货 60
        po_receipt_add(&db, &PoReceipt { id: 0, po_id, period: p, date: NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(), qty: m("60"), memo: String::new() }).unwrap();
        // 退货 10
        po_return_add(&db, po_id, p, NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(), m("10"), "破损").unwrap();
        // 付款 500
        po_payment_add(&db, &PoPayment { id: 0, po_id, period: p, date: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(), amount: m("500"), memo: String::new() }).unwrap();
        assert_eq!(po_payment_sum(&db, po_id).unwrap(), m("500"));
        // 执行跟踪：60-10=50 → 执行率 50%
        let track = po_execution_track(&db, p).unwrap();
        assert_eq!(track.len(), 1);
        assert_eq!(track[0].received_qty, m("50"));
        assert_eq!(track[0].rate, m("50"));
        // 统计
        let stats = purchase_stats(&db, p).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].amount, m("1000"));
    }

    #[test]
    fn price_history_recorded() {
        let db = mem();
        price_history_record(&db, "140301", "S01", m("10"), NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()).unwrap();
        price_history_record(&db, "140301", "S01", m("11"), NaiveDate::from_ymd_opt(2026, 2, 5).unwrap()).unwrap();
        let h = price_history(&db, "140301").unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].1, m("11")); // 最新在前
    }
}
